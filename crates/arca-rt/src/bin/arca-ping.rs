//! dev.arca.ping — sub-app de prueba del motor nativo (F0-F2, r3).
//!
//! No se ejecuta a mano: la lanza el supervisor y le entrega el canal AIPC.
//! Variables de entorno que recibe:
//!
//! - `ARCA_APP`       → nombre de la app ("dev.arca.ping")
//! - `ARCA_INSTANCE`  → número de instancia del supervisor
//! - `ARCA_MODO`      → `serve` (defecto) | `panic` | `hang`
//! - `ARCA_LOG`       → nivel de log (el supervisor lo pone en `debug`)
//! - `ARCA_CANAL`     → `fd3` (defecto, tests en PC) | `stdio` (APK Android)
//!
//! El canal AIPC puede llegar por dos vías:
//!
//! - **fd3**: socketpair heredado en el descriptor 3 — lo usa el motor en PC
//!   (`arca-exec-native`, entre `fork` y `exec`).
//! - **stdio**: stdin/stdout del proceso — lo usa la app Android (Java,
//!   `ProcessBuilder`, que no puede pasar fds arbitrarios). Las tramas AIPC
//!   viajan por stdout y los logs SIEMPRE por stderr, así el canal queda
//!   limpio para datos.
//!
//! Modos:
//! - **serve**: handshake HELLO, responde PING→PONG, apaga limpio al recibir
//!   SHUTDOWN (exit 0) o al cerrarse el canal (exit 0).
//! - **panic**: handshake HELLO y luego `panic!` **en el hilo main** →
//!   el proceso muere con exit code 101 (así lo exige la prueba 4).
//! - **hang**: handshake HELLO y se queda dormido → solo muere con SIGKILL
//!   (para la prueba 5).

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::process;
use std::time::Duration;

use arca_log::{log_debug, log_info, log_warn};
use arca_rt::ipc;

fn main() {
    arca_log::init();
    let instancia = std::env::var("ARCA_INSTANCE").unwrap_or_else(|_| "0".into());
    let pid = process::id().to_string();
    log_debug!("arca::log", "log de sub-app listo",
               "instance" => &instancia, "pid" => &pid);

    let modo = std::env::var("ARCA_MODO").unwrap_or_default();
    let por_stdio = std::env::var("ARCA_CANAL")
        .unwrap_or_default()
        .eq_ignore_ascii_case("stdio");

    if por_stdio {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut d = Duplex { r: stdin.lock(), w: stdout.lock() };
        correr(&modo, &mut d);
    } else {
        let mut canal = unsafe { File::from_raw_fd(3) };
        correr(&modo, &mut canal);
    }
}

/// Canal AIPC por stdin+stdout a la vez (para el host Java de Android).
/// Implementa `Read + Write` para reusar las mismas funciones del protocolo.
struct Duplex<R, W> {
    r: R,
    w: W,
}

impl<R: Read, W> Read for Duplex<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.r.read(buf)
    }
}

impl<R, W: Write> Write for Duplex<R, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.w.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

/// Presentación (HELLO) + despacho según el modo.
/// Un solo genérico sirve para `File` (fd 3) y `Duplex` (stdio).
fn correr<D: Read + Write>(modo: &str, canal: &mut D) {
    if let Err(e) = ipc::enviar(canal, ipc::TAG_HELLO, b"dev.arca.ping") {
        log_warn!("arca::rt", "no pude enviar HELLO", "error" => &e.to_string());
        process::exit(1);
    }
    // imprescindible en stdio: stdout está bufferado y el host espera la trama
    let _ = canal.flush();

    match modo {
        "panic" => {
            // ⚠️ a propósito en el hilo main: un panic aquí mata al proceso
            // con exit code 101 (lo verifica la prueba 4, en PC y Android).
            log_info!("arca::rt", "modo panic: provocando pánico controlado");
            panic!("boom controlado (prueba e2e_panic_de_la_app_exit_101)");
        }
        "hang" => {
            log_info!("arca::rt", "modo hang: dormido hasta que llegue SIGKILL");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        _ => servir(canal),
    }
}

/// Ciclo normal de la sub-app: atender mensajes hasta que nos apaguen.
fn servir<D: Read + Write>(canal: &mut D) {
    loop {
        let (tag, payload) = match ipc::recibir(canal) {
            Ok(x) => x,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                log_info!("arca::rt", "canal cerrado por el supervisor, salgo");
                break;
            }
            Err(e) => {
                log_warn!("arca::rt", "error leyendo el canal, salgo",
                          "error" => &e.to_string());
                break;
            }
        };
        match tag {
            ipc::TAG_PING => {
                // eco del nonce tal cual llegó
                if ipc::enviar(canal, ipc::TAG_PONG, &payload).is_err() {
                    log_warn!("arca::rt", "no pude responder PONG, salgo");
                    break;
                }
                let _ = canal.flush();
            }
            ipc::TAG_SHUTDOWN => {
                let razon = payload.first().copied().unwrap_or(0);
                let nombre = ipc::nombre_razon(razon);
                log_info!("arca::rt", "Shutdown recibido", "reason" => nombre);
                break;
            }
            otro => {
                log_warn!("arca::rt", "mensaje desconocido, ignorado",
                          "tag" => &otro.to_string());
            }
        }
    }
    log_info!("arca::rt", "apagado limpio", "code" => "0");
    process::exit(0);
}
