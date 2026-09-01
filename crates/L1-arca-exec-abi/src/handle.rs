//! [`AppHandle`] (lado host) y [`HandleDriver`] (lado executor) sobre un
//! `Arc<HandleInner>`.
//!
//! # Anti-leak (spec 13 §5, fila "Leak de handles")
//!
//! El host sostiene `Arc<HandleInner>`; el executor sostiene solo un
//! [`Weak`]. Cuando el host suelta el último clone del handle, el inner
//! se libera y el driver deja de tener efecto (`emit` → `false`,
//! `is_attached` → `false`): el watcher del executor debe terminar. El
//! drop del handle NO mata al proceso (como `std::process::Child`): para
//! terminar se usa [`AppHandle::shutdown`].
//!
//! # Orden de locks (invariante interna)
//!
//! `emit_mx` → `terminal.st` → `state` → `subs`. `emit_mx` serializa las
//! emisiones para que "nada se entrega tras `Dead`" sea estructural y no
//! una cuestión de disciplina. `shutdown` nunca retiene un lock mientras
//! invoca el kill hook.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

use arca_protocol::{ControlMsg, ShutdownReason};
use arca_types::{AppId, ArcaError, InstanceId, Res};

use crate::bus::BusHandle;
use crate::state::{AppEvent, AppState, DeathReason, ExitStatus};

/// Bloqueo con mapeo de veneno → `ArcaError::Internal` de contexto
/// estático + log del detalle dinámico (política spec 01 §5 / ADR-014).
fn lock<'a, T: ?Sized>(m: &'a Mutex<T>, ctx: &'static str) -> Res<MutexGuard<'a, T>> {
    m.lock().map_err(|p| {
        tracing::error!(
            target: "arca::exec-abi::handle",
            ctx,
            error = %p,
            "mutex envenenado"
        );
        ArcaError::Internal(ctx)
    })
}

/// Ventana de asentamiento tras invocar el kill hook: tiempo máximo que
/// espera `shutdown` a que el deathWatch reporte la muerte que el hook
/// causó (SIGKILL es casi inmediato; el margen cubre scheduling/reaping).
const KILL_SETTLE: Duration = Duration::from_millis(500);

/// Hook de terminación forzada que instala el executor (interna).
type KillFn = Arc<dyn Fn() -> Res<()> + Send + Sync>;

/// Una instancia viva, vista por el host (spec 13 §3).
///
/// Barata de clonar (Arc interno): los clones comparten estado, stream de
/// eventos y terminal. Ciclo de uso del host: `on_state_change()` una vez
/// (o suscripciones tardías), `send` a discreción, `shutdown` para
/// terminar.
#[derive(Clone)]
pub struct AppHandle {
    pub(crate) inner: Arc<HandleInner>,
}

/// Estado compartido de la instancia. Campos de solo-escritura-emit y
/// solo-lectura-host: ver el orden de locks del módulo.
pub(crate) struct HandleInner {
    app_id: AppId,
    instance: InstanceId,
    /// Estado de ciclo (lo escribe `emit`, lo lee `state`).
    state: Mutex<AppState>,
    /// Bus hacia el proceso — TODO mensaje pasa por aquí.
    bus: BusHandle,
    /// Estado terminal + wakeup de `shutdown`.
    terminal: Terminal,
    /// Serializa emisiones (orden del stream).
    emit_mx: Mutex<()>,
    /// `Spawned` ya emitido (lo fija la construcción).
    spawned: AtomicBool,
    /// `Dead` ya emitido (one-shot; spec 13 §5, fila "Dead duplicado").
    dead_sent: AtomicBool,
    /// Receiver primario: se entrega UNA vez en `on_state_change`.
    primary_rx: Mutex<Option<Receiver<AppEvent>>>,
    /// Suscriptores del stream (el primero lo crea `new_pair`).
    subs: Mutex<Vec<Sender<AppEvent>>>,
    /// Kill hook del executor (escalada de `shutdown` tras el grace).
    kill: Mutex<Option<KillFn>>,
}

/// Estado terminal: escritor `emit(Dead)`, esperador `shutdown`.
struct Terminal {
    st: Mutex<Option<ExitStatus>>,
    cv: Condvar,
}

impl Terminal {
    fn new() -> Self {
        Self {
            st: Mutex::new(None),
            cv: Condvar::new(),
        }
    }

    /// Lectura sin espera (`None` = vivo o mutex envenenado).
    fn peek(&self) -> Option<ExitStatus> {
        self.st.lock().ok().and_then(|g| *g)
    }

    /// Espera hasta `d` a que exista estado terminal.
    /// `None` = timeout o mutex envenenado (degenerado: `shutdown`
    /// escalará y terminará fallando ruidoso).
    fn wait(&self, d: Duration) -> Option<ExitStatus> {
        let guard = match self.st.lock() {
            Ok(g) => g,
            Err(p) => {
                tracing::error!(
                    target: "arca::exec-abi::handle",
                    error = %p,
                    "terminal: mutex envenenado"
                );
                return None;
            }
        };
        match self.cv.wait_timeout_while(guard, d, |st| st.is_none()) {
            Ok((g, _)) => *g,
            Err(p) => {
                tracing::error!(
                    target: "arca::exec-abi::handle",
                    error = %p,
                    "terminal: espera envenenada"
                );
                None
            }
        }
    }

    /// Fija el estado terminal y despierta esperadores. `notify_all` bajo
    /// lock para no perder wakeups. Solo el primer valor cuenta (el
    /// one-shot lo decide el `AtomicBool` del llamador).
    fn set(&self, s: ExitStatus) {
        if let Ok(mut g) = self.st.lock() {
            if g.is_none() {
                *g = Some(s);
            }
            self.cv.notify_all();
        } else {
            tracing::error!(
                target: "arca::exec-abi::handle",
                "terminal: set con mutex envenenado (estado terminal perdido)"
            );
        }
    }
}

impl HandleInner {
    /// Emisión con las garantías de ciclo de vida del crate:
    ///
    /// - `Spawned`: one-shot (lo emite `new_pair`).
    /// - Intermedios (`Hello`/`Ready`/`Paused`/`Resumed`/`Unhealthy`/
    ///   `FrameStalled`): solo entre `Spawned` y `Dead`.
    /// - `Dead`: terminal y one-shot; fija `terminal` (despierta
    ///   `shutdown`), muta estado y entrega — en ese orden.
    ///
    /// Devuelve `false` si el evento se rechaza (duplicado, fuera de
    /// orden, o mutex envenenado). Es la ÚNICA vía de emisión: todo pasa
    /// por aquí, incluida la síntesis de `shutdown`.
    fn emit(&self, ev: AppEvent) -> bool {
        // NOTA: `_serial` (no `_`) mantiene el guard vivo hasta el final
        // del cuerpo: serializa la emisión completa.
        let Ok(_serial) = lock(&self.emit_mx, "emit: serializador envenenado") else {
            return false;
        };
        match ev {
            AppEvent::Spawned => {
                // One-shot: lo emite la construcción; un segundo Spawned
                // (por un executor despistado) se rechaza.
                if self.spawned.swap(true, Ordering::SeqCst) {
                    return false;
                }
                // Sin transición: el handle nace (y sigue) Spawning.
                self.forward(&AppEvent::Spawned);
                true
            }
            AppEvent::Dead { reason, minidump } => {
                // One-shot: el primer Dead gana; el resto se descarta
                // (spec 13 §5, fila "Dead duplicado").
                if self.dead_sent.swap(true, Ordering::SeqCst) {
                    return false;
                }
                // Orden: terminal (despierta shutdown) → estado → entrega.
                self.terminal.set(ExitStatus::from(reason));
                match self.state.lock() {
                    Ok(mut st) => {
                        if !matches!(*st, AppState::Dead { .. }) {
                            *st = AppState::Dead { reason };
                        }
                    }
                    Err(p) => tracing::error!(
                        target: "arca::exec-abi::handle",
                        app_id = %self.app_id,
                        instance = %self.instance,
                        error = %p,
                        "emit Dead: estado envenenado"
                    ),
                }
                self.forward(&AppEvent::Dead { reason, minidump });
                true
            }
            other => {
                // Intermedios: exigen Spawned (ya emitido al construir) y
                // ausencia de Dead (nada se entrega tras la muerte).
                if !self.spawned.load(Ordering::SeqCst) || self.dead_sent.load(Ordering::SeqCst) {
                    return false;
                }
                match self.state.lock() {
                    Ok(mut st) => match other {
                        AppEvent::Ready => *st = AppState::Ready,
                        AppEvent::Paused => *st = AppState::Paused,
                        AppEvent::Resumed => *st = AppState::Running,
                        // Hello/Unhealthy/FrameStalled: salud/handshake,
                        // no ciclo (spec 13 §5, fila 1).
                        _ => {}
                    },
                    Err(p) => tracing::error!(
                        target: "arca::exec-abi::handle",
                        app_id = %self.app_id,
                        instance = %self.instance,
                        error = %p,
                        "emit: estado envenenado (el evento se entrega igual)"
                    ),
                }
                self.forward(&other);
                true
            }
        }
    }

    /// Entrega a los suscriptores vivos; los caídos se eliminan.
    fn forward(&self, ev: &AppEvent) {
        match self.subs.lock() {
            Ok(mut subs) => subs.retain(|tx| tx.send(ev.clone()).is_ok()),
            Err(p) => tracing::error!(
                target: "arca::exec-abi::handle",
                app_id = %self.app_id,
                instance = %self.instance,
                error = %p,
                "forward: suscriptores envenenados (evento perdido)"
            ),
        }
    }

    /// Snapshot del kill hook (clonado para invocarlo sin locks).
    fn kill_snapshot(&self) -> Option<KillFn> {
        match self.kill.lock() {
            Ok(g) => g.clone(),
            Err(p) => {
                tracing::error!(
                    target: "arca::exec-abi::handle",
                    app_id = %self.app_id,
                    instance = %self.instance,
                    error = %p,
                    "kill: hook envenenado (no hay escalada)"
                );
                None
            }
        }
    }
}

impl AppHandle {
    /// Constructor para implementadores de [`Executor`](crate::Executor)
    /// (NO para el host). Crea el par de una instancia: handle para el
    /// host, [`HandleDriver`] para su deathWatch.
    ///
    /// Emite [`AppEvent::Spawned`] de inmediato: "spawn aceptado" = primer
    /// evento del stream, garantizado por construcción y no por disciplina
    /// del executor (invariante "Spawned siempre es el primero"). El
    /// handle nace en [`AppState::Spawning`]; ni Hello ni Ready se
    /// esperan aquí.
    ///
    /// El driver lleva un `Weak`: si el host suelta todos los clones, el
    /// driver deja de surtir efecto (ver docs del módulo).
    pub fn new_pair(
        app_id: AppId,
        instance: InstanceId,
        bus: BusHandle,
    ) -> (AppHandle, HandleDriver) {
        let (tx, rx) = std::sync::mpsc::channel();
        let inner = Arc::new(HandleInner {
            app_id,
            instance,
            state: Mutex::new(AppState::Spawning),
            bus,
            terminal: Terminal::new(),
            emit_mx: Mutex::new(()),
            spawned: AtomicBool::new(false),
            dead_sent: AtomicBool::new(false),
            primary_rx: Mutex::new(Some(rx)),
            subs: Mutex::new(vec![tx]),
            kill: Mutex::new(None),
        });
        let handle = Self {
            inner: inner.clone(),
        };
        let driver = HandleDriver {
            inner: Arc::downgrade(&inner),
        };
        // Invariante de arranque: no puede fallar con mutex recién creados;
        // en debug lo verificamos igual (romperlo rompería el contrato).
        debug_assert!(
            inner.emit(AppEvent::Spawned),
            "Spawned rechazado en construcción"
        );
        (handle, driver)
    }

    /// Id de la instancia (el asignado por el host en el `AppSpec`).
    pub fn id(&self) -> InstanceId {
        self.inner.instance
    }

    /// Estado de ciclo actual.
    ///
    /// Fail-closed: si el mutex interno está envenenado (panic de un
    /// watcher con el lock tomado) se reporta `Dead { Lost }` — el host
    /// jamás sigue operando una instancia en estado desconocido.
    pub fn state(&self) -> AppState {
        match self.inner.state.lock() {
            Ok(g) => *g,
            Err(p) => {
                tracing::error!(
                    target: "arca::exec-abi::handle",
                    app_id = %self.inner.app_id,
                    instance = %self.inner.instance,
                    error = %p,
                    "state: envenenado — fail-closed como Dead(Lost)"
                );
                AppState::Dead {
                    reason: DeathReason::Lost,
                }
            }
        }
    }

    /// Envía un mensaje de control al proceso — SIEMPRE por el bus
    /// (invariante spec 13 §4). Rechaza si la instancia ya está muerta
    /// (el bus real también fallaría; esto falla más claro).
    pub fn send(&self, msg: &ControlMsg) -> Res<()> {
        if matches!(self.state(), AppState::Dead { .. }) {
            tracing::warn!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                "send a instancia ya muerta"
            );
            return Err(ArcaError::Internal("exec-abi: send a instancia muerta"));
        }
        self.inner.bus.send_ctl(msg)
    }

    /// Apaga la instancia con un `grace` de cortesía y devuelve el estado
    /// terminal. Fases:
    ///
    /// 1. Si ya hay `Dead` reportado → devuelve ese estado (idempotente,
    ///    sin duplicar eventos ni mensajes).
    /// 2. Envía `Shutdown { reason: User }` por el bus (v1 no parametriza
    ///    el motivo — decisión documentada) y espera el `Dead` natural
    ///    hasta agotar `grace`.
    /// 3. Escalada: invoca el kill hook del executor (SIGKILL/fin de
    ///    instancia wasm) y espera el asentamiento del deathWatch.
    /// 4. Si ni el hook produjo reporte a tiempo (watcher caído/proceso
    ///    colgado): sintetiza `Dead { KilledByHost }` — el one-shot
    ///    garantiza que, si el watcher reportó en la carrera, gane el
    ///    reporte real.
    ///
    /// Sin kill hook instalado y sin muerte natural: `Err(Internal)`
    /// (executor incompleto — los reales SIEMPRE instalan hook).
    pub fn shutdown(&self, grace: Duration) -> Res<ExitStatus> {
        // 1. Idempotente: el Dead ya reportado, sin duplicar.
        if let Some(s) = self.inner.terminal.peek() {
            return Ok(s);
        }
        // 2. Cortesía por el bus (mejor esfuerzo: si el canal ya murió,
        //    el deathWatch decidirá igual).
        if let Err(e) = self.inner.bus.send_ctl(&ControlMsg::Shutdown {
            reason: ShutdownReason::User,
        }) {
            tracing::warn!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                error = %e,
                "shutdown: Shutdown no enviado (bus caído); se espera al deathWatch"
            );
        }
        if let Some(s) = self.inner.terminal.wait(grace) {
            return Ok(s);
        }
        // 3. Escalada por el executor.
        let Some(kill) = self.inner.kill_snapshot() else {
            tracing::warn!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                "shutdown: grace agotado sin kill hook instalado"
            );
            return Err(ArcaError::Internal(
                "exec-abi: shutdown agotó el grace sin señal de muerte",
            ));
        };
        if let Err(e) = kill() {
            tracing::warn!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                error = %e,
                "shutdown: kill hook devolvió error"
            );
            return Err(ArcaError::Internal(
                "exec-abi: kill hook falló tras el grace",
            ));
        }
        if let Some(s) = self.inner.terminal.wait(KILL_SETTLE) {
            return Ok(s);
        }
        // 4. Síntesis terminal: el hook no produjo reporte a tiempo.
        //    One-shot: si el deathWatch reportó en la carrera, ese gana.
        tracing::warn!(
            target: "arca::exec-abi::handle",
            app_id = %self.inner.app_id,
            instance = %self.inner.instance,
            "shutdown: sin reporte tras kill — se sintetiza Dead(KilledByHost)"
        );
        self.inner.emit(AppEvent::Dead {
            reason: DeathReason::KilledByHost,
            minidump: None,
        });
        self.inner.terminal.peek().ok_or(ArcaError::Internal(
            "exec-abi: estado terminal perdido tras el kill",
        ))
    }

    /// Stream de eventos de ciclo de vida/salud (canal `std::sync::mpsc`).
    ///
    /// El PRIMER llamado recibe el receiver primario — con los eventos
    /// tempranos (incluido `Spawned`) ya encolados. Llamadas posteriores
    /// reciben un receiver "tardío": canal NUEVO que solo verá eventos
    /// emitidos a partir de ese momento.
    ///
    /// Decisión documentada frente a la alternativa "rechazar con error
    /// tipado": el contrato de spec 13 fija la firma sin `Result`; el
    /// broadcast simple conserva la firma y deja rastro en el log. Si un
    /// mutex interno está envenenado se devuelve un receiver vacío (modo
    /// degenerado, registrado en el log).
    pub fn on_state_change(&self) -> Receiver<AppEvent> {
        match self.inner.primary_rx.lock() {
            Ok(mut slot) => {
                if let Some(rx) = slot.take() {
                    return rx;
                }
            }
            Err(p) => tracing::error!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                error = %p,
                "on_state_change: primario envenenado — se crea suscriptor tardío"
            ),
        }
        let (tx, rx) = std::sync::mpsc::channel();
        match self.inner.subs.lock() {
            Ok(mut subs) => subs.push(tx),
            Err(p) => tracing::error!(
                target: "arca::exec-abi::handle",
                app_id = %self.inner.app_id,
                instance = %self.inner.instance,
                error = %p,
                "on_state_change: suscriptores envenenados — receiver huérfano"
            ),
        }
        rx
    }
}

/// Lado del executor de un [`AppHandle`]: por aquí emite su deathWatch y
/// demás watchers, e instala la escalada de `shutdown`.
///
/// Sostiene un [`Weak`] del inner (ver docs del módulo): cuando el host
/// suelta todos los clones, [`emit`](Self::emit) devuelve `false` e
/// [`is_attached`](Self::is_attached) pasa a `false` — el watcher debe
/// terminar. Clonable y barato de mover a un hilo.
#[derive(Clone)]
pub struct HandleDriver {
    inner: Weak<HandleInner>,
}

impl HandleDriver {
    /// Emite un evento de ciclo de vida (ver la máquina en el docs del
    /// crate). `false` = rechazado (duplicado/fuera de orden/handle ya
    /// soltado por el host).
    pub fn emit(&self, ev: AppEvent) -> bool {
        match self.inner.upgrade() {
            Some(inner) => inner.emit(ev),
            None => false,
        }
    }

    /// Reporte del deathWatch: la instancia murió. One-shot garantizado
    /// (la segunda llamada devuelve `false` sin efectos). `minidump` viaja
    /// solo en el evento; el archivo lo escribe y posee el runtime.
    pub fn report_death(&self, reason: DeathReason, minidump: Option<PathBuf>) -> bool {
        self.emit(AppEvent::Dead { reason, minidump })
    }

    /// Instala el hook de terminación forzada que [`AppHandle::shutdown`]
    /// invoca tras agotar el grace (native: SIGKILL al pid; wasm: fin de
    /// la instancia). Debe llamarse durante `launch`, antes de devolver el
    /// handle. Sin hook, `shutdown` solo puede fallar tras el grace.
    /// Reinstalar reemplaza (gana el último).
    pub fn install_kill(&self, kill: impl Fn() -> Res<()> + Send + Sync + 'static) {
        match self.inner.upgrade() {
            Some(inner) => match inner.kill.lock() {
                Ok(mut slot) => *slot = Some(Arc::new(kill)),
                Err(p) => tracing::error!(
                    target: "arca::exec-abi::handle",
                    app_id = %inner.app_id,
                    instance = %inner.instance,
                    error = %p,
                    "install_kill: hook envenenado"
                ),
            },
            None => tracing::warn!(
                target: "arca::exec-abi::handle",
                "install_kill: handle ya soltado por el host"
            ),
        }
    }

    /// ¿El host aún sostiene el handle? `false` → el watcher debe terminar
    /// (anti-leak). No dice nada del proceso: solo del interés del host.
    pub fn is_attached(&self) -> bool {
        self.inner.strong_count() > 0
    }
}

impl fmt::Debug for AppHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppHandle")
            .field("app_id", &self.inner.app_id)
            .field("instance", &self.inner.instance)
            .field("state", &self.state())
            .finish()
    }
}

impl fmt::Debug for HandleDriver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandleDriver")
            .field("attached", &self.is_attached())
            .finish()
    }
}

#[cfg(test)]
impl AppHandle {
    /// Referencias fuertes del inner (test de leak de la spec 13 §6).
    pub(crate) fn inner_strong_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex; // alias: mocks de bus de estos tests

    use super::*;
    use crate::bus::BusTransport;
    use arca_protocol::SignalMsg;

    /// Bus cortés: registra mensajes y "muere" con Exit(0) al recibir
    /// Shutdown (app que respeta la cortesía). El driver se le inyecta
    /// después del `new_pair`, como haría un executor real.
    struct CortesBus {
        driver: StdMutex<Option<HandleDriver>>,
        ctl: StdMutex<Vec<ControlMsg>>,
    }

    impl BusTransport for CortesBus {
        fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
            self.ctl.lock().expect("ctl").push(msg.clone());
            if matches!(msg, ControlMsg::Shutdown { .. }) {
                let d = self.driver.lock().expect("driver").clone();
                if let Some(d) = d {
                    let _ = d.report_death(DeathReason::Exit { code: 0 }, None);
                }
            }
            Ok(())
        }
        fn send_signal(&mut self, _: &SignalMsg) -> Res<()> {
            Ok(())
        }
        fn set_deadline(&mut self, _: u32) {}
    }

    /// Bus sordo: registra y nunca muere (app colgada).
    struct SordoBus {
        ctl: StdMutex<Vec<ControlMsg>>,
    }

    impl BusTransport for SordoBus {
        fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
            self.ctl.lock().expect("ctl").push(msg.clone());
            Ok(())
        }
        fn send_signal(&mut self, _: &SignalMsg) -> Res<()> {
            Ok(())
        }
        fn set_deadline(&mut self, _: u32) {}
    }

    fn app_id() -> AppId {
        AppId::new("com.example.app").expect("AppId de test válida")
    }

    fn drain(rx: &Receiver<AppEvent>) -> Vec<AppEvent> {
        let mut v = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            v.push(ev);
        }
        v
    }

    fn pair(bus: BusHandle) -> (AppHandle, HandleDriver) {
        AppHandle::new_pair(app_id(), InstanceId::new(1), bus)
    }

    /// Par con bus cortés: devuelve handle, driver y el transporte (para
    /// aserciones sobre lo que llegó al "proceso").
    fn pair_cortes() -> (AppHandle, HandleDriver, Arc<StdMutex<CortesBus>>) {
        let t = Arc::new(StdMutex::new(CortesBus {
            driver: StdMutex::new(None),
            ctl: StdMutex::new(Vec::new()),
        }));
        let (handle, driver) = pair(BusHandle::new(t.clone()));
        *t.lock().expect("transporte").driver.lock().expect("driver") = Some(driver.clone());
        (handle, driver, t)
    }

    /// Ciclo completo por el par directo: estados según evento, Dead
    /// terminal one-shot, idempotencia de shutdown y "nada tras Dead".
    #[test]
    fn ciclo_completo_estados_y_eventos() {
        let (handle, driver, t) = pair_cortes();
        let rx = handle.on_state_change();

        assert_eq!(handle.id(), InstanceId::new(1));
        assert_eq!(handle.state(), AppState::Spawning);

        assert!(driver.emit(AppEvent::Hello));
        assert_eq!(handle.state(), AppState::Spawning, "Hello no muta estado");
        assert!(driver.emit(AppEvent::Ready));
        assert_eq!(handle.state(), AppState::Ready);
        assert!(driver.emit(AppEvent::Paused));
        assert_eq!(handle.state(), AppState::Paused);
        assert!(driver.emit(AppEvent::Resumed));
        assert_eq!(handle.state(), AppState::Running);
        // Salud ≠ ciclo (spec 13 §5, fila 1).
        assert!(driver.emit(AppEvent::Unhealthy { since_ms: 900 }));
        assert!(driver.emit(AppEvent::FrameStalled { frames: 3 }));
        assert_eq!(handle.state(), AppState::Running);

        let dump = PathBuf::from("/data/minidumps/x.dmp");
        assert!(driver.report_death(DeathReason::Exit { code: 3 }, Some(dump.clone())));
        // Duplicados y post-mortem: rechazados.
        assert!(!driver.report_death(DeathReason::Exit { code: 3 }, None));
        assert!(!driver.emit(AppEvent::Resumed));
        assert!(!driver.emit(AppEvent::Spawned));

        assert_eq!(
            handle.state(),
            AppState::Dead {
                reason: DeathReason::Exit { code: 3 }
            }
        );
        assert_eq!(
            drain(&rx),
            vec![
                AppEvent::Spawned,
                AppEvent::Hello,
                AppEvent::Ready,
                AppEvent::Paused,
                AppEvent::Resumed,
                AppEvent::Unhealthy { since_ms: 900 },
                AppEvent::FrameStalled { frames: 3 },
                AppEvent::Dead {
                    reason: DeathReason::Exit { code: 3 },
                    minidump: Some(dump)
                },
            ]
        );

        // shutdown tras Dead: el estado ya reportado, sin duplicar nada.
        let st = handle.shutdown(Duration::ZERO).expect("dead ya reportado");
        assert_eq!(st, ExitStatus::from(DeathReason::Exit { code: 3 }));
        assert!(drain(&rx).is_empty(), "sin eventos nuevos");
        assert!(
            t.lock()
                .expect("transporte")
                .ctl
                .lock()
                .expect("ctl")
                .is_empty(),
            "sin Shutdown: ya estaba muerto"
        );
    }

    /// La segunda llamada a on_state_change es "tardía": solo ve eventos
    /// futuros a su creación (decisión documentada).
    #[test]
    fn on_state_change_tardio_solo_ve_futuros() {
        let (handle, driver, _) = pair_cortes();
        let r1 = handle.on_state_change(); // primario: todo, desde Spawned
        assert!(driver.emit(AppEvent::Hello));
        let r2 = handle.on_state_change(); // tardío: nace AHORA
        assert!(driver.emit(AppEvent::Ready));

        assert_eq!(
            drain(&r1),
            vec![AppEvent::Spawned, AppEvent::Hello, AppEvent::Ready]
        );
        assert_eq!(drain(&r2), vec![AppEvent::Ready]);
    }

    /// Shutdown con cortesía: Shutdown por el bus, muerte natural dentro
    /// del grace, status coherente.
    #[test]
    fn shutdown_con_cortesia_termina_bien() {
        let (handle, _, t) = pair_cortes();
        let rx = handle.on_state_change();

        let st = handle
            .shutdown(Duration::from_millis(200))
            .expect("muerte natural por cortesía");
        assert_eq!(
            st,
            ExitStatus {
                code: 0,
                signal: None
            }
        );
        assert_eq!(
            handle.state(),
            AppState::Dead {
                reason: DeathReason::Exit { code: 0 }
            }
        );
        // El Shutdown viajó por el bus (invariante: todo pasa por el bus).
        let ctl = t
            .lock()
            .expect("transporte")
            .ctl
            .lock()
            .expect("ctl")
            .clone();
        assert!(matches!(ctl.first(), Some(ControlMsg::Shutdown { .. })));
        assert_eq!(ctl.len(), 1, "un solo Shutdown");
        // Dead llegó exactamente una vez, al final.
        assert_eq!(
            drain(&rx).last(),
            Some(&AppEvent::Dead {
                reason: DeathReason::Exit { code: 0 },
                minidump: None
            })
        );
    }

    /// Grace agotado sin kill hook: error tipado (executor incompleto).
    #[test]
    fn shutdown_sin_hook_falla_tras_grace() {
        let (handle, _) = pair(BusHandle::new(Arc::new(StdMutex::new(SordoBus {
            ctl: StdMutex::new(Vec::new()),
        }))));
        let t0 = std::time::Instant::now();
        let r = handle.shutdown(Duration::from_millis(20));
        assert!(
            t0.elapsed() >= Duration::from_millis(20),
            "el grace se respeta"
        );
        assert!(matches!(r, Err(ArcaError::Internal(_))));
        assert_eq!(
            handle.state(),
            AppState::Spawning,
            "no murió: sin inventar Dead"
        );
    }

    /// Grace agotado con kill hook funcional: el hook mata y el deathWatch
    /// reporta; shutdown devuelve el estado del proceso matado.
    #[test]
    fn shutdown_con_kill_hook_reporta_muerte() {
        let (handle, driver) = pair(BusHandle::new(Arc::new(StdMutex::new(SordoBus {
            ctl: StdMutex::new(Vec::new()),
        }))));
        let d = driver.clone();
        driver.install_kill(move || {
            // El executor real haría SIGKILL; el deathWatch reporta su efecto.
            let _ = d.report_death(DeathReason::Signaled { signal: 9 }, None);
            Ok(())
        });
        let rx = handle.on_state_change();
        let st = handle
            .shutdown(Duration::from_millis(20))
            .expect("kill hook produce muerte");
        assert_eq!(
            st,
            ExitStatus {
                code: 0,
                signal: Some(9)
            }
        );
        assert_eq!(
            handle.state(),
            AppState::Dead {
                reason: DeathReason::Signaled { signal: 9 }
            }
        );
        assert_eq!(
            drain(&rx).last(),
            Some(&AppEvent::Dead {
                reason: DeathReason::Signaled { signal: 9 },
                minidump: None
            })
        );
    }

    /// Kill hook sordo (proceso irremediable): shutdown sintetiza
    /// Dead(KilledByHost) — el host siempre obtiene un terminal.
    #[test]
    fn shutdown_kill_sordo_sintetiza_killed_by_host() {
        let (handle, driver) = pair(BusHandle::new(Arc::new(StdMutex::new(SordoBus {
            ctl: StdMutex::new(Vec::new()),
        }))));
        driver.install_kill(|| Ok(())); // no mata nada (watcher caído/colgado)
        let rx = handle.on_state_change();
        let st = handle
            .shutdown(Duration::from_millis(10))
            .expect("síntesis terminal");
        assert_eq!(st, ExitStatus::from(DeathReason::KilledByHost));
        assert_eq!(
            handle.state(),
            AppState::Dead {
                reason: DeathReason::KilledByHost
            }
        );
        assert_eq!(
            drain(&rx).last(),
            Some(&AppEvent::Dead {
                reason: DeathReason::KilledByHost,
                minidump: None
            })
        );
    }

    /// send() a una instancia muerta se rechaza tipado.
    #[test]
    fn send_a_instancia_muerta_rechaza() {
        let (handle, driver, _) = pair_cortes();
        assert!(handle.send(&ControlMsg::Pause).is_ok());
        assert!(driver.report_death(DeathReason::Lost, None));
        let r = handle.send(&ControlMsg::Resume);
        assert!(matches!(r, Err(ArcaError::Internal(_))));
    }

    /// send() pasa por el bus (aserción de entrega).
    #[test]
    fn send_va_por_el_bus() {
        let (handle, _, t) = pair_cortes();
        handle.send(&ControlMsg::Ping { t_ns: 7 }).expect("send");
        let ctl = t
            .lock()
            .expect("transporte")
            .ctl
            .lock()
            .expect("ctl")
            .clone();
        assert!(matches!(ctl.first(), Some(ControlMsg::Ping { t_ns: 7 })));
    }

    /// RAII: tras soltar el handle, el driver (Weak) ya no emite y reporta
    /// desapego — sin leak de inner (complemento del DoS de mock.rs).
    #[test]
    fn driver_sin_host_no_emite() {
        let (handle, driver, _) = pair_cortes();
        assert!(driver.is_attached());
        drop(handle);
        assert!(!driver.is_attached());
        assert!(!driver.emit(AppEvent::Hello));
        assert!(!driver.report_death(DeathReason::Lost, None));
        // Debug del driver refleja el desapego.
        assert!(format!("{driver:?}").contains("attached: false"));
    }

    /// Spawned y Dead son one-shot por construcción (sin depender del
    /// buen comportamiento del executor).
    #[test]
    fn spawned_y_dead_son_one_shot() {
        let (handle, driver, _) = pair_cortes();
        let rx = handle.on_state_change();
        assert!(!driver.emit(AppEvent::Spawned), "new_pair ya lo emitió");
        assert!(driver.report_death(DeathReason::Lost, None));
        assert!(!driver.report_death(DeathReason::Exit { code: 0 }, None));
        assert_eq!(
            drain(&rx),
            vec![
                AppEvent::Spawned,
                AppEvent::Dead {
                    reason: DeathReason::Lost,
                    minidump: None
                }
            ]
        );
    }

    /// El helper de locks mapea veneno a `Internal` sin unwraps.
    #[test]
    fn lock_helper_mapea_veneno() {
        let m = StdMutex::new(0u8);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = lock(&m, "test: veneno").expect("primera toma");
            panic!("envenena")
        }));
        assert!(matches!(
            lock(&m, "test: veneno"),
            Err(ArcaError::Internal(_))
        ));
    }
}
