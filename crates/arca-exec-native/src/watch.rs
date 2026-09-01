//! watch (módulo de arca-exec-native): el **hilo vigía** por instancia.
//!
//! CORRECCIÓN CENTRAL DE LA r2 — antes de esta versión la muerte de una
//! sub-app se detectaba solo indirectamente (EOF del canal AIPC), lo que
//! rompía dos pruebas:
//!
//! * `e2e_panic_de_la_app_exit_101`  → el exit code 101 del panic no se
//!   propagaba de forma confiable.
//! * `e2e_spawn_handshake_ping_kill9_dead` → un SIGKILL no se reportaba
//!   como muerte por señal.
//!
//! Ahora cada instancia tiene un hilo dedicado haciendo `waitpid` bloqueante:
//! * exit normal  → `Evento::Salida { code }` (con `WEXITSTATUS`, p.ej. 101)
//! * señal        → `Evento::MuertoPorSenal { senal }` (p.ej. 9 = SIGKILL)
//! * y además recolecta (reap) al hijo → **nunca quedan zombis**.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;

use arca_log::{log_error, log_info};

/// Evento final de una instancia.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evento {
    /// El proceso terminó por sí mismo con `exit code`.
    Salida { pid: i32, code: i32 },
    /// El proceso murió por una señal (p.ej. 9 = SIGKILL).
    MuertoPorSenal { pid: i32, senal: i32 },
}

/// Nombre legible de una señal (para logs).
pub fn nombre_senal(s: i32) -> &'static str {
    match s {
        4 => "SIGILL",
        6 => "SIGABRT",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => "SIG?",
    }
}

/// Lanza el hilo vigía para `pid`. El evento final viaja por `tx`;
/// `termino` se activa apenas el waitpid vuelve (para que `Drop` no espere).
pub fn lanzar(pid: i32, tx: Sender<Evento>, termino: Arc<AtomicBool>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("watch-{pid}"))
        .spawn(move || {
            arca_log::init(); // barato e idempotente; fija t0 si este hilo corre primero
            loop {
                let mut status: libc::c_int = 0;
                let r = unsafe { libc::waitpid(pid, &mut status, 0) };
                if r == -1 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(libc::EINTR) {
                        continue; // señal sin importancia para nosotros: reintentar
                    }
                    // ECHILD: ya lo recolectó otro waitpid (no debería pasar).
                    log_error!("arca::exec-native::watch", "waitpid falló",
                               "pid" => &pid.to_string(),
                               "errno" => &err.raw_os_error().unwrap_or(0).to_string());
                    break;
                }
                termino.store(true, Ordering::Release);
                if libc::WIFEXITED(status) {
                    let code = libc::WEXITSTATUS(status);
                    log_info!("arca::exec-native::watch", "instancia exit",
                              "pid" => &pid.to_string(), "code" => &code.to_string());
                    let _ = tx.send(Evento::Salida { pid, code });
                    break;
                }
                if libc::WIFSIGNALED(status) {
                    let senal = libc::WTERMSIG(status);
                    log_info!("arca::exec-native::watch", "instancia muerta por señal",
                              "pid" => &pid.to_string(),
                              "senal" => &senal.to_string(),
                              "nombre" => nombre_senal(senal));
                    let _ = tx.send(Evento::MuertoPorSenal { pid, senal });
                    break;
                }
                // WIFSTOPPED/CONT: sin ptrace no ocurre; seguimos esperando.
            }
        })
        .expect("lanzar hilo watch")
}
