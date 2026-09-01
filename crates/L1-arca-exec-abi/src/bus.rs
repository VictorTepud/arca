//! [`BusTransport`] + [`BusHandle`]: el único canal hacia el proceso
//! (invariante spec 13 §4: ningún executor habla directo con shm/ipc
//! internos).
//!
//! Implementaciones previstas (desviación 4): `arca-ipc` (UDS real, T12)
//! para native y `arca-exec-wasm` (in-proc, T25) para wasm. El host
//! construye el transporte, lo envuelve en [`BusHandle`] y lo entrega a
//! [`Executor::launch`](crate::Executor::launch).

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use arca_protocol::{ControlMsg, SignalMsg};
use arca_types::{ArcaError, Res};

/// Bloqueo con mapeo de veneno → `ArcaError::Internal` de contexto
/// estático + log del detalle dinámico (política spec 01 §5 / ADR-014).
fn lock<'a, T: ?Sized>(m: &'a Mutex<T>, ctx: &'static str) -> Res<MutexGuard<'a, T>> {
    m.lock().map_err(|p| {
        tracing::error!(target: "arca::exec-abi::bus", ctx, error = %p, "mutex envenenado");
        ArcaError::Internal(ctx)
    })
}

/// Transporte de mensajes hacia el proceso (trait objeto-seguro,
/// desviación 4).
///
/// Las llamadas llegan serializadas por el `Mutex` del [`BusHandle`]: las
/// implementaciones NO necesitan sincronización propia. `Send` es
/// supertrait → `dyn BusTransport` ya es `Send` y el handle es
/// `Send + Sync`.
pub trait BusTransport: Send {
    /// Envía un mensaje del canal de control (framing AIPC + sendmsg con
    /// fds por SCM_RIGHTS cuando aplique).
    fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()>;

    /// Envía una señal QoS (canal de señal: eventfd o socket, docs/04 §4).
    fn send_signal(&mut self, s: &SignalMsg) -> Res<()>;

    /// Ajusta el deadline del watchdog del transporte (ms). Best-effort:
    /// un hint de QoS, no un contrato — por eso no devuelve error.
    fn set_deadline(&mut self, ms: u32);
}

/// Handle clonable hacia el transporte del proceso.
///
/// Envuelve `Arc<Mutex<dyn BusTransport>>`: clonar es barato y todas las
/// operaciones quedan serializadas. Esto es TODO lo que un executor ve
/// del ipc — la shm y los fds los negocia el host por su lado (docs/04 §3).
#[derive(Clone)]
pub struct BusHandle {
    transport: Arc<Mutex<dyn BusTransport>>,
}

impl BusHandle {
    /// Envuelve un transporte (lo hace el host: UDS real o in-proc).
    pub fn new(transport: Arc<Mutex<dyn BusTransport>>) -> Self {
        Self { transport }
    }

    /// Envía un mensaje de control al proceso (serializado con el resto
    /// de operaciones del bus).
    pub fn send_ctl(&self, msg: &ControlMsg) -> Res<()> {
        let mut t = lock(&self.transport, "bus: transporte envenenado")?;
        t.send_ctl(msg)
    }

    /// Envía una señal QoS al proceso.
    pub fn send_signal(&self, s: &SignalMsg) -> Res<()> {
        let mut t = lock(&self.transport, "bus: transporte envenenado")?;
        t.send_signal(s)
    }

    /// Ajusta el deadline del watchdog del transporte (best-effort; ver
    /// [`BusTransport::set_deadline`]). Sin error de retorno por diseño:
    /// si el transporte está envenenado se registra y se pierde el hint.
    pub fn set_deadline(&self, ms: u32) {
        match self.transport.lock() {
            Ok(mut t) => t.set_deadline(ms),
            Err(p) => tracing::warn!(
                target: "arca::exec-abi::bus",
                error = %p,
                "set_deadline: transporte envenenado (hint perdido)"
            ),
        }
    }
}

impl fmt::Debug for BusHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusHandle").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Bus de grabación: encola ctl/señal y expone el deadline.
    struct Grabadora {
        ctl: Mutex<Vec<ControlMsg>>,
        sig: Mutex<Vec<SignalMsg>>,
        deadline: AtomicU32,
    }

    impl Grabadora {
        fn new() -> Arc<Mutex<Self>> {
            Arc::new(Mutex::new(Self {
                ctl: Mutex::new(Vec::new()),
                sig: Mutex::new(Vec::new()),
                deadline: AtomicU32::new(0),
            }))
        }
    }

    impl BusTransport for Grabadora {
        fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
            self.ctl.lock().expect("ctl").push(msg.clone());
            Ok(())
        }
        fn send_signal(&mut self, s: &SignalMsg) -> Res<()> {
            self.sig.lock().expect("sig").push(*s);
            Ok(())
        }
        fn set_deadline(&mut self, ms: u32) {
            self.deadline.store(ms, Ordering::Relaxed);
        }
    }

    #[test]
    fn bus_handle_delega_y_es_compartido_por_clones() {
        let transport = Grabadora::new();
        let bus = BusHandle::new(transport.clone());
        let clon = bus.clone(); // el executor lo recibe por clonación

        let ping = ControlMsg::Ping { t_ns: 42 };
        bus.send_ctl(&ping).expect("send_ctl");
        clon.send_signal(&SignalMsg::Busy).expect("send_signal");
        clon.set_deadline(250);

        let g = transport.lock().expect("transport");
        assert_eq!(g.ctl.lock().expect("ctl").len(), 1);
        assert!(matches!(
            g.ctl.lock().expect("ctl")[0],
            ControlMsg::Ping { t_ns: 42 }
        ));
        assert_eq!(*g.sig.lock().expect("sig"), vec![SignalMsg::Busy]);
        assert_eq!(g.deadline.load(Ordering::Relaxed), 250);
    }

    #[test]
    fn bus_handle_transporte_envenenado_no_paniea() {
        // El guard exterior queda envenenado → tipado, sin unwrap del host.
        struct Mordida;
        impl BusTransport for Mordida {
            fn send_ctl(&mut self, _: &ControlMsg) -> Res<()> {
                panic!("envenena el bus")
            }
            fn send_signal(&mut self, _: &SignalMsg) -> Res<()> {
                Ok(())
            }
            fn set_deadline(&mut self, _: u32) {}
        }
        let bus = BusHandle::new(Arc::new(Mutex::new(Mordida)));
        // Primer send envenena el Mutex del transporte (panic dentro del impl).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bus.send_ctl(&ControlMsg::Pause)
        }));
        // Los siguientes fallan tipados (no panic) y el hint se pierde sin ruido.
        let err = bus.send_ctl(&ControlMsg::Resume).expect_err("debe fallar");
        assert!(matches!(err, ArcaError::Internal(_)));
        bus.set_deadline(10);
    }
}
