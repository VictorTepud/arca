//! arca-exec-native (L2): motor de ejecución nativa — **lado del supervisor**.
//!
//! Responsibilities de F0-F1 (r2):
//! 1. `spawn`   — lanzar la sub-app (binario estático) con el canal AIPC en fd 3
//!                + sandbox básico (`no_new_privs`, `RLIMIT_NOFILE`).
//! 2. `watch`   — el **hilo vigía**: `waitpid` bloqueante por instancia.
//!    ▸ Reporta `Evento::Salida { code }` (exit normal, p.ej. 101 en un panic)
//!    ▸ Reporta `Evento::MuertoPorSenal { senal }` (p.ej. 9 = SIGKILL)
//!    ▸ **Recolecta** al hijo → nunca quedan zombis.
//!    Este hilo es la corrección central de la r2: antes la muerte solo se
//!    detectaba indirectamente (EOF del canal) y los exit codes se perdían.
//! 3. `drain`   — drenar stdout/stderr de la sub-app, re-emitir con contexto.
//! 4. AIPC      — handshake HELLO + PING/PONG + SHUTDOWN.
//!
//! En F2 el spawn gana seccomp-BPF real y la vigilancia gana pidfd.

pub mod drain;
pub mod spawn;
pub mod watch;

use std::fs::File;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
// (los hilos viven en `Option` para poder remontarlos en `Drop`, que solo
// recibe `&mut self` y `JoinHandle::join` consume el handle)

use arca_log::{log_info, log_warn};
use arca_ipc;

pub use spawn::SpawnCfg;
pub use watch::Evento;

/// Modo de prueba de la sub-app `arca-ping` (se pasa como `ARCA_MODO`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modo {
    /// Ciclo normal: responde pings y apaga limpio.
    Serve,
    /// Pánico controlado justo después del handshake (exit 101).
    Panic,
    /// Se cuelga: solo muere con SIGKILL.
    Hang,
}

impl Modo {
    /// Valor que viaja por la variable de entorno `ARCA_MODO`.
    pub fn como_str(self) -> &'static str {
        match self {
            Modo::Serve => "serve",
            Modo::Panic => "panic",
            Modo::Hang => "hang",
        }
    }
}

/// Una sub-app viva bajo vigilancia del supervisor.
#[derive(Debug)]
pub struct Instancia {
    pid: i32,
    app: String,
    instancia: u32,
    canal: Option<File>,
    rx_ev: Receiver<Evento>,
    /// Último evento recibido (cacheado: `Evento` es `Copy`).
    evento_final: Option<Evento>,
    termino: Arc<AtomicBool>,
    h_watch: Option<JoinHandle<()>>,
    h_out: Option<JoinHandle<()>>,
    h_err: Option<JoinHandle<()>>,
    cap_out: Arc<Mutex<Vec<String>>>,
    cap_err: Arc<Mutex<Vec<String>>>,
    _hijo: Child,
}

impl Instancia {
    /// Lanza una sub-app y espera su HELLO (handshake, timeout 2 s).
    pub fn lanzar(cfg: SpawnCfg) -> std::io::Result<Instancia> {
        spawn::lanzar(cfg)
    }

    /// PID del proceso de la sub-app.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Nombre de la app (p.ej. "dev.arca.ping").
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Número de instancia asignado por el supervisor.
    pub fn instancia(&self) -> u32 {
        self.instancia
    }

    /// Envía un PING y mide cuánto tarda el PONG (timeout 2 s).
    pub fn ping(&mut self) -> std::io::Result<Duration> {
        let canal = self
            .canal
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotConnected, "canal cerrado"))?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x4152_4341);
        let t0 = std::time::Instant::now();
        arca_ipc::enviar(canal, arca_ipc::TAG_PING, &nonce.to_le_bytes())?;
        let (tag, payload) = arca_ipc::recibir(canal)?;
        if tag != arca_ipc::TAG_PONG || payload != nonce.to_le_bytes() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("esperaba PONG con nonce {nonce:#x}, llegó tag={tag} len={}", payload.len()),
            ));
        }
        Ok(t0.elapsed())
    }

    /// Pide el apagado ordenado (SHUTDOWN, razón User).
    pub fn apagar(&mut self) -> std::io::Result<()> {
        let canal = self
            .canal
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotConnected, "canal cerrado"))?;
        arca_ipc::enviar(canal, arca_ipc::TAG_SHUTDOWN, &[arca_ipc::RAZON_USER])
    }

    /// Mata la sub-app con SIGKILL (kill -9). No puede ser ignorado.
    /// La muerte la detecta el hilo vigía y queda como `MuertoPorSenal(9)`.
    pub fn matar9(&self) {
        log_info!("arca::exec-native", "kill SIGKILL", "pid" => &self.pid.to_string());
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
    }

    /// Cierra el canal AIPC sin enviar SHUTDOWN. La sub-app buena detecta el
    /// EOF y se apaga sola (exit 0).
    pub fn cerrar_canal(&mut self) {
        self.canal = None;
    }

    /// Espera el evento final de la sub-app (exit o señal) hasta `timeout`.
    /// El evento queda cacheado: llamadas repetidas devuelven el mismo valor.
    pub fn esperar_salida(&mut self, timeout: Duration) -> Result<Evento, RecvTimeoutError> {
        if let Some(ev) = self.evento_final {
            return Ok(ev);
        }
        let ev = self.rx_ev.recv_timeout(timeout)?;
        self.evento_final = Some(ev);
        Ok(ev)
    }

    /// Espera el evento final **y remonta los hilos de drenaje**.
    ///
    /// Esto garantiza que `stdout_texto()` / `stderr_texto()` llamados después
    /// contengan TODA la salida de la sub-app: el vigía avisa la muerte nada
    /// más volver `waitpid`, pero los drenajes necesitan un instante más para
    /// leer lo último que quedó en los pipes. Sin esto, leer capturas justo
    /// después del evento es una condición de carrera.
    pub fn finalizar(&mut self, timeout: Duration) -> Result<Evento, RecvTimeoutError> {
        let ev = self.esperar_salida(timeout);
        if ev.is_ok() {
            // la sub-app murió → sus pipes se cerraron → los drenajes llegan a
            // EOF por sí solos; remontarlos aquí es rápido y seguro.
            self.canal = None;
            if let Some(h) = self.h_out.take() {
                let _ = h.join();
            }
            if let Some(h) = self.h_err.take() {
                let _ = h.join();
            }
            if let Some(h) = self.h_watch.take() {
                let _ = h.join();
            }
        }
        ev
    }

    /// Copia del stdout capturado de la sub-app (líneas crudas).
    pub fn stdout_texto(&self) -> String {
        self.cap_out.lock().map(|c| c.join("\n")).unwrap_or_default()
    }

    /// Copia del stderr capturado de la sub-app (líneas crudas).
    pub fn stderr_texto(&self) -> String {
        self.cap_err.lock().map(|c| c.join("\n")).unwrap_or_default()
    }

    fn ya_termino(&self) -> bool {
        self.termino.load(Ordering::Acquire)
    }
}

impl Drop for Instancia {
    /// Nunca dejamos zombis ni hilos colgados: si la sub-app sigue viva al
    /// soltar la instancia, pedimos apagado ordenado, y si no muere en 2 s,
    /// SIGKILL. Luego se remontan (join) los hilos del supervisor.
    fn drop(&mut self) {
        if !self.ya_termino() {
            let _ = self.apagar();
            if self.rx_ev.recv_timeout(Duration::from_secs(2)).is_err() {
                self.matar9();
                let _ = self.rx_ev.recv_timeout(Duration::from_secs(2));
            }
        }
        self.canal = None;
        if let Some(h) = self.h_watch.take() {
            let _ = h.join();
        }
        if let Some(h) = self.h_out.take() {
            let _ = h.join();
        }
        if let Some(h) = self.h_err.take() {
            let _ = h.join();
        }
        if !self.ya_termino() {
            log_warn!("arca::exec-native", "instancia soltada sin evento final",
                      "pid" => &self.pid.to_string(), "app" => &self.app);
        }
    }
}

/// Contador global de instancias (visible en los logs como `instance=N`).
pub(crate) fn siguiente_instancia() -> u32 {
    use std::sync::atomic::AtomicU32;
    static CONT: AtomicU32 = AtomicU32::new(1);
    CONT.fetch_add(1, Ordering::SeqCst)
}
