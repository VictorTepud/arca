//! Watcher de vida de la instancia: handshake + reap (waitpid) + drain.

use std::os::fd::AsFd as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arca_exec_abi::{AppEvent, DeathReason, HandleDriver};
use arca_ipc::{handshake_server, Conn};
use arca_protocol::ShmLayout;
use arca_shm::Memfd;
use nix::sys::signal::kill;
use nix::sys::signal::Signal::SIGKILL;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::Duration;
use tracing::{info, warn};

use crate::bus::ConnBus;
use crate::spec::LaunchSpec;

/// Periodo del watchdog de detach: cada cuánto el watcher comprueba si el
/// host aún sostiene el handle. Host soltado → SIGKILL + reporte (contrato
/// de `HandleDriver::is_attached`: el watcher DEBE terminar).
const DETACH_POLL_MS: u64 = 250;

/// Techo del reap tras el SIGKILL de detach (el hijo no puede sobrevivirlo:
/// solo protege de un waitpid eterno por señal de entrega diferida).
const REAP_TRAS_KILL: Duration = Duration::from_secs(2);

/// Estado del hilo watcher (uno por instancia).
pub(crate) struct WatchCtx {
    /// Pid de la app (waitpid/kill).
    pub pid: Pid,
    /// Extremo host del socketpair (AIPC ctl).
    pub conn: Conn,
    /// Spec serializable (identidad esperada del handshake).
    pub spec: LaunchSpec,
    /// memfds a entregar por SCM_RIGHTS en WELCOME (orden: frames, input).
    pub memfds: Vec<Memfd>,
    /// Geometría shm anunciada.
    pub layout: ShmLayout,
    /// Ventanas del Attach v1.
    pub windows: Vec<arca_protocol::WindowSpec>,
    /// Bus compartido (se instala la Conn tras el handshake).
    pub bus: Arc<Mutex<ConnBus>>,
    /// Driver de eventos del handle.
    pub driver: HandleDriver,
}

/// Hilo de vigilancia: handshake → Hello/Ready → reap bloqueante → Dead.
///
/// Arquitectura del reap (fix de las e2e flaky, ver docs del crate):
/// - un hilo dedicado hace `waitpid` **bloqueante**: la muerte se detecta en
///   µs (el kernel despierta al hilo) — sin drift de scheduler, a diferencia
///   del viejo poll `WNOHANG` cada 5 ms que bajo carga pasaba de 50 ms;
/// - este hilo espera el resultado con timeout de [`DETACH_POLL_MS`]: cada
///   vencimiento comprueba `driver.is_attached()`; si el host soltó el
///   handle → SIGKILL + reap + reporte (antes: fuga de hilo Y de proceso).
pub(crate) fn watch(ctx: WatchCtx) {
    // Destructure completo: sin moves parciales (todos los campos se usan).
    let WatchCtx {
        pid,
        conn,
        spec,
        memfds,
        layout,
        windows,
        bus,
        driver,
    } = ctx;
    // 1) handshake (bloquea ≤ 2 s: deadline interno de arca-ipc).
    let expect = match arca_types::AppId::new(&spec.app_id) {
        Ok(a) => arca_ipc::HelloExpect {
            app_id: a,
            instance: arca_types::InstanceId::new(spec.instance),
            artifact_hash: arca_types::Digest(spec.artifact),
        },
        Err(e) => {
            warn!(target: "arca::exec-native::watch", err = %e, "app_id inválido en spec");
            let _ = waitpid(pid, Some(WaitPidFlag::WNOHANG));
            driver.report_death(DeathReason::Lost, None);
            return;
        }
    };
    let caps: Vec<arca_types::Capability> = arca_types::Capability::all()
        .iter()
        .copied()
        .filter(|c| spec.caps_bits & (1u32 << c.index()) != 0)
        .collect();
    let mut conn = conn;
    let raw_fds: Vec<std::os::fd::RawFd> = memfds.iter().map(|m| m.raw_fd()).collect();
    match handshake_server(&mut conn, &expect, &raw_fds, layout, &caps, &windows) {
        Ok(_ready) => {
            // Invariante de orden: la Conn se instala ANTES de emitir Ready
            // (el host reacciona a Ready enviando mensajes al instante).
            if let Ok(mut b) = bus.lock() {
                b.install(conn);
            }
            // Invariante ABI: Hello antes que Ready; Spawned lo emitió launch.
            driver.emit(AppEvent::Hello);
            driver.emit(AppEvent::Ready);
        }
        Err(e) => {
            warn!(target: "arca::exec-native::watch", err = %e, pid = pid.as_raw(), "handshake falló");
            reap_fin_de_vida(pid);
            driver.report_death(DeathReason::Lost, None);
            return;
        }
    }

    // Invariante: los Memfd viven hasta aquí (cerrados tras el sendmsg).
    drop(memfds);

    // 2) reap bloqueante en hilo propio + watchdog de detach aquí.
    reap_con_watchdog(pid, driver);
}

/// Espera la muerte del hijo con `waitpid` BLOQUEANTE (hilo dedicado) y
/// reporta el evento final. Mientras tanto, cada [`DETACH_POLL_MS`] comprueba
/// que el host siga interesado (`is_attached`): soltado → SIGKILL.
fn reap_con_watchdog(pid: Pid, driver: HandleDriver) {
    let (tx, rx) = std::sync::mpsc::channel::<WaitStatus>();
    std::thread::Builder::new()
        .name(format!("reap-{}", pid.as_raw()))
        .spawn(move || reap_bloqueante(pid, tx))
        .expect("spawn hilo reap");

    loop {
        match rx.recv_timeout(Duration::from_millis(DETACH_POLL_MS)) {
            Ok(WaitStatus::Exited(_, code)) => {
                driver.report_death(DeathReason::Exit { code }, None);
                info!(target: "arca::exec-native::watch", pid = pid.as_raw(), code, "instancia exit");
                return;
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                driver.report_death(DeathReason::Signaled { signal: sig as i32 }, None);
                info!(
                    target: "arca::exec-native::watch",
                    pid = pid.as_raw(),
                    signal = sig as i32,
                    "instancia señal"
                );
                return;
            }
            // Stopped/Continued no ocurren (no usamos ptrace): fail-closed.
            Ok(_) => {
                driver.report_death(DeathReason::Lost, None);
                return;
            }
            // El hilo reap murió sin resultado (p. ej. ECHILD): Lost.
            Err(RecvTimeoutError::Disconnected) => {
                driver.report_death(DeathReason::Lost, None);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                if driver.is_attached() {
                    continue; // vivo y con host interesado: esperar más
                }
                // Contrato is_attached: host soltado → matar y reportar.
                warn!(
                    target: "arca::exec-native::watch",
                    pid = pid.as_raw(),
                    "host soltó el handle: SIGKILL y reporte"
                );
                let _ = kill(pid, SIGKILL);
                match rx.recv_timeout(REAP_TRAS_KILL) {
                    Ok(WaitStatus::Exited(_, code)) => {
                        driver.report_death(DeathReason::Exit { code }, None);
                    }
                    Ok(WaitStatus::Signaled(_, sig, _)) => {
                        driver.report_death(DeathReason::Signaled { signal: sig as i32 }, None);
                    }
                    _ => {
                        driver.report_death(DeathReason::Lost, None);
                    }
                }
                return;
            }
        }
    }
}

/// Hilo dedicado: `waitpid` BLOQUEANTE hasta la muerte real del hijo.
/// EINTR se reintenta; cualquier otro error (ECHILD: ya recolectado)
/// cierra el canal → el watcher reporta `Lost` (fail-closed).
fn reap_bloqueante(pid: Pid, tx: Sender<WaitStatus>) {
    loop {
        match waitpid(pid, None) {
            Ok(st) => {
                let _ = tx.send(st);
                return;
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => return, // canal cae → Lost
        }
    }
}

/// Reap de cortesía cuando el hijo ya murió fuera del flujo normal
/// (fallo de handshake): recoge el zombi si existe, silenciosamente.
fn reap_fin_de_vida(pid: Pid) {
    let _ = waitpid(pid, Some(WaitPidFlag::WNOHANG));
}

/// Hilo de drain de stdout/stderr del hijo → tracing (target por app).
///
/// Ring de línea de 64 KB con drop-el-más-viejo (spec 14 §5): un hijo
/// parlanchín nunca bloquea (el pipe drena siempre).
pub(crate) fn drain_pipe(pipe: OwnedFd, app_id: String, canal: &'static str) {
    let mut buf = [0u8; 4096];
    let mut line = String::new();
    loop {
        match nix::unistd::read(pipe.as_fd(), &mut buf) {
            Ok(0) | Err(_) => break, // EOF (hijo murió) o error: fin
            Ok(n) => {
                line.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(pos) = line.find('\n') {
                    let (l, rest) = line.split_at(pos);
                    let l = l.trim_end_matches('\r');
                    if !l.is_empty() {
                        info!(target: "arca::exec-native::drain", app = %app_id, canal, "{l}");
                    }
                    line = rest[1..].to_owned();
                }
                if line.len() > 64 * 1024 {
                    line.drain(..line.len() - 64 * 1024);
                }
            }
        }
    }
}

/// PathBuf de utilidad interno (evita import huérfano en lib.rs).
#[allow(dead_code)]
pub(crate) fn _app_dir_marker(_p: &PathBuf) {}
