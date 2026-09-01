//! spawn (módulo de arca-exec-native): lanzamiento de sub-apps nativas.
//!
//! Esquema de fds de la sub-app:
//! ```text
//! 0 = /dev/null (stdin)   1 = pipe stdout → drain   2 = pipe stderr → drain
//! 3 = canal AIPC (socketpair con el supervisor)
//! ```
//! El paso del fd 3 se hace en `pre_exec` (entre `fork` y `exec`), donde solo
//! se permiten llamadas async-signal-safe: `dup2`, `close`, `fcntl`,
//! `prctl`, `setrlimit`. Nada de logs ni allocations ahí.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arca_log::{log_info, log_warn};

use crate::{Instancia, Modo, siguiente_instancia};
use crate::drain;
use crate::watch;

/// Timeout del handshake (HELLO) y de cada PING, vía `SO_RCVTIMEO`.
const TIMEOUT_RECV_S: libc::time_t = 2;

/// Configuración para lanzar una sub-app.
#[derive(Clone, Debug)]
pub struct SpawnCfg {
    /// Ruta del binario (p.ej. `target/x86_64-unknown-linux-musl/debug/arca-ping`).
    pub binario: std::path::PathBuf,
    /// Nombre de la app que debe presentar en el HELLO (p.ej. "dev.arca.ping").
    pub app: String,
    /// Número de instancia; si es `None` se toma del contador global.
    pub instancia: Option<u32>,
    /// Modo de prueba de `arca-ping` (viaja como `ARCA_MODO`).
    pub modo: Modo,
}

impl SpawnCfg {
    /// Configuración mínima para la sub-app de pruebas.
    pub fn new(binario: impl Into<std::path::PathBuf>, app: impl Into<String>) -> Self {
        Self {
            binario: binario.into(),
            app: app.into(),
            instancia: None,
            modo: Modo::Serve,
        }
    }

    /// Fija el modo de la sub-app de pruebas.
    pub fn modo(mut self, modo: Modo) -> Self {
        self.modo = modo;
        self
    }
}

/// Lanza la sub-app y espera su HELLO. Ver `Instancia::lanzar`.
pub fn lanzar(cfg: SpawnCfg) -> io::Result<Instancia> {
    let instancia = cfg.instancia.unwrap_or_else(siguiente_instancia);
    let app = cfg.app.clone();

    // 1) Canal AIPC: socketpair UNIX (ambos extremos CLOEXEC en el padre).
    let (fd_host, fd_hijo) = unsafe {
        let mut fds = [-1 as libc::c_int; 2];
        if libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        (
            OwnedFd::from_raw_fd(fds[0]),
            OwnedFd::from_raw_fd(fds[1]),
        )
    };

    // 2) Timeout de recepción del lado host (handshake y pings).
    unsafe {
        let tv = libc::timeval {
            tv_sec: TIMEOUT_RECV_S,
            tv_usec: 0,
        };
        libc::setsockopt(
            fd_host.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );
    }

    // 3) El comando: entorno limpio + variables ARCA_*.
    let fd_hijo_raw = fd_hijo.as_raw_fd();
    let mut cmd = Command::new(&cfg.binario);
    cmd.env_clear()
        .env("ARCA_APP", &app)
        .env("ARCA_INSTANCE", instancia.to_string())
        .env("ARCA_MODO", cfg.modo.como_str())
        .env("ARCA_LOG", "debug")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        cmd.pre_exec(move || {
            // ⚠️ Solo llamadas async-signal-safe en esta sección.
            // El canal AIPC del hijo debe quedar en el fd 3, sin CLOEXEC.
            if fd_hijo_raw != 3 {
                if libc::dup2(fd_hijo_raw, 3) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // el dup2 hacia 3 ya quedó sin CLOEXEC; cerramos el original
                libc::close(fd_hijo_raw);
            } else {
                // ya cayó en 3: solo hay que quitarle el CLOEXEC
                libc::fcntl(3, libc::F_SETFD, 0);
            }

            // Sandbox básico de F0 (best-effort, sobrevive al exec):
            // sin este parche no se puede escalar privilegios desde la sub-app…
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_int, 0, 0, 0);
            // …y tampoco abrir un sinfín de descriptores.
            let rl = libc::rlimit {
                rlim_cur: 256,
                rlim_max: 256,
            };
            libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
            Ok(())
        });
    }

    // 4) fork + exec.
    let mut hijo = cmd.spawn()?;
    drop(fd_hijo); // el extremo del hijo ya vive en el proceso hijo (fd 3)
    let pid = hijo.id() as libc::pid_t;

    let stdout = hijo.stdout.take().expect("stdout piped");
    let stderr = hijo.stderr.take().expect("stderr piped");
    let canal = unsafe { File::from_raw_fd(fd_host.into_raw_fd()) };

    // 5) Hilos del supervisor: vigía (waitpid) + drenajes de stdout/stderr.
    let (tx_ev, rx_ev) = mpsc::channel();
    let termino = Arc::new(AtomicBool::new(false));
    let h_watch = watch::lanzar(pid, tx_ev, termino.clone());
    let cap_out: Arc<Mutex<Vec<String>>> = Arc::default();
    let cap_err: Arc<Mutex<Vec<String>>> = Arc::default();
    let h_out = drain::lanzar(stdout, app.clone(), instancia, "stdout", cap_out.clone());
    let h_err = drain::lanzar(stderr, app.clone(), instancia, "stderr", cap_err.clone());

    // 6) Handshake: esperar HELLO con el nombre correcto (timeout 2 s).
    let mut canal = canal;
    let handshake = (|| -> io::Result<()> {
        let (tag, payload) = arca_ipc::recibir(&mut canal)?;
        if tag != arca_ipc::TAG_HELLO {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("esperaba HELLO, llegó tag={tag}"),
            ));
        }
        let nombre = String::from_utf8_lossy(&payload).to_string();
        if nombre != app {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("HELLO con nombre inesperado: {nombre:?} (esperaba {app:?})"),
            ));
        }
        Ok(())
    })();
    if let Err(e) = handshake {
        log_warn!("arca::exec-native", "handshake falló",
                  "pid" => &pid.to_string(), "app" => &app, "error" => &e.to_string());
        drop(canal);
        unsafe { libc::kill(pid, libc::SIGKILL); }
        let _ = h_watch.join();
        let _ = h_out.join();
        let _ = h_err.join();
        return Err(io::Error::new(io::ErrorKind::Other, format!("handshake con {app} falló: {e}")));
    }

    log_info!("arca::exec-native", "spawn OK", "pid" => &pid.to_string(), "app" => &app);

    Ok(Instancia {
        pid,
        app,
        instancia,
        canal: Some(canal),
        rx_ev,
        evento_final: None,
        termino,
        h_watch: Some(h_watch),
        h_out: Some(h_out),
        h_err: Some(h_err),
        cap_out,
        cap_err,
        _hijo: hijo,
    })
}

/// Cota de cortesía para el join de drenajes (no se usa hoy, reservada a F2).
#[allow(dead_code)]
fn _timeout_drop() -> Duration {
    Duration::from_secs(2)
}
