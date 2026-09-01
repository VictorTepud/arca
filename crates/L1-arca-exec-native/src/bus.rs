//! `ConnBus`: adaptador de [`Conn`] (arca-ipc) al trait [`BusTransport`]
//! (arca-exec-abi). Regla de huérfanos: el trait vive en L1-exec-abi, el tipo
//! en L0-ipc — SOLO un crate L1 (este) puede impl el par.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arca_exec_abi::BusTransport;
use arca_ipc::Conn;
use arca_protocol::{ControlMsg, SignalMsg};
use arca_types::{ArcaError, Res};

/// Envoltorio con contador de seq monótono (docs/04 §5: seq por dirección).
pub struct ConnBus {
    conn: Option<Conn>,
    seq: AtomicU64,
}

impl ConnBus {
    /// Bus VACÍO: el watcher lo rellena con la [`Conn`] tras el handshake
    /// (los mensajes previos fallan con error tipado, no se pierden en
    /// silencio).
    pub fn empty() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            conn: None,
            seq: AtomicU64::new(1),
        }))
    }

    /// Entrega la conexión ya handshakedeada al bus compartido.
    pub fn install(&mut self, conn: Conn) {
        self.conn = Some(conn);
    }

    /// Roba la conexión (shutdown del lado host).
    pub fn take(&mut self) -> Option<Conn> {
        self.conn.take()
    }

    /// Recibe un mensaje de control del proceso (con deadline propio).
    pub fn recv_ctl_msg(&mut self, timeout: std::time::Duration) -> Res<arca_protocol::ControlMsg> {
        self.with_conn(|c| {
            if c.deadline_ms() == 0 {
                c.set_deadline((timeout.as_millis().min(u32::MAX as u128)) as u32)?;
            }
            c.recv_ctl_msg()
        })
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn with_conn<T>(&mut self, f: impl FnOnce(&mut Conn) -> Res<T>) -> Res<T> {
        match self.conn.as_mut() {
            Some(c) => f(c),
            None => Err(ArcaError::Internal(
                "bus nativo: conexión aún no establecida (handshake)",
            )),
        }
    }
}

impl BusTransport for ConnBus {
    fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
        let seq = self.next_seq();
        self.with_conn(|c| c.send_ctl(msg, seq, &[]))
    }

    fn send_signal(&mut self, s: &SignalMsg) -> Res<()> {
        let seq = self.next_seq();
        self.with_conn(|c| c.send_signal(s, seq))
    }

    fn set_deadline(&mut self, ms: u32) {
        if let Some(c) = self.conn.as_mut() {
            let _ = c.set_deadline(ms);
        }
    }
}
