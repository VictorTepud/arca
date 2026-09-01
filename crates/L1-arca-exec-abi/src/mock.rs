//! Mock de [`Executor`] + bus para los tests de este crate (spec 13 §6):
//! ciclo completo de estados/eventos, property tests del contrato y DoS
//! de 1000 launch/shutdown.
//!
//! El mock simula spawn inmediato (Hello+Ready al lanzar) y un "proceso"
//! cuya reacción al `Shutdown` decide [`ShutdownPolicy`]; el test inyecta
//! eventos arbitrarios con [`MockExecutor::emit`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arca_protocol::{ControlMsg, ShutdownReason, SignalMsg};
use arca_types::{AppId, ArcaError, Capability, Digest, InstanceId, Res};

use crate::bus::{BusHandle, BusTransport};
use crate::handle::{AppHandle, HandleDriver};
use crate::spec::{AppDirs, AppSpec, ArtifactRef, RespawnPolicy};
use crate::state::{AppEvent, AppState, DeathReason, ExitStatus};
use crate::Executor;

/// Reacción del "proceso" simulado al `Shutdown` del host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShutdownPolicy {
    /// Respeta la cortesía: muere con `Exit(0)` apenas recibe `Shutdown`.
    ExitInmediato,
    /// La ignora: solo muere por el kill hook del executor (el grace vence
    /// y el host escala).
    IgnoraGrace,
    /// Muere por señal (SIGTERM=15) al recibir `Shutdown`.
    MuereConSenal,
}

/// El "proceso" simulado: comparte estado entre el bus (entrada) y el
/// executor (salida: driver del handle).
struct MockProc {
    policy: ShutdownPolicy,
    driver: Mutex<Option<HandleDriver>>,
    ctl: Mutex<Vec<ControlMsg>>,
    sig: Mutex<Vec<SignalMsg>>,
    deadline_ms: AtomicU32,
    dead: AtomicBool,
}

impl MockProc {
    fn new(policy: ShutdownPolicy) -> Self {
        Self {
            policy,
            driver: Mutex::new(None),
            ctl: Mutex::new(Vec::new()),
            sig: Mutex::new(Vec::new()),
            deadline_ms: AtomicU32::new(0),
            dead: AtomicBool::new(false),
        }
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// Muere una sola vez y lo reporta por el driver (deathWatch simulado).
    fn die(&self, reason: DeathReason) -> bool {
        if self.dead.swap(true, Ordering::SeqCst) {
            return false;
        }
        match self.driver.lock().expect("driver").clone() {
            Some(d) => d.report_death(reason, None),
            None => false,
        }
    }

    /// Reacción al `Shutdown` según la política.
    fn on_shutdown(&self) {
        match self.policy {
            ShutdownPolicy::ExitInmediato => {
                let _ = self.die(DeathReason::Exit { code: 0 });
            }
            ShutdownPolicy::MuereConSenal => {
                let _ = self.die(DeathReason::Signaled { signal: 15 });
            }
            // Sordo: el host tendrá que escalar con el kill hook.
            ShutdownPolicy::IgnoraGrace => {}
        }
    }

    /// Terminación forzada por el host (kill hook): SIGKILL simulado.
    fn kill_by_host(&self) -> Res<()> {
        let _ = self.die(DeathReason::Signaled { signal: 9 });
        Ok(())
    }
}

/// Transporte del mock: registra mensajes y dispara la política de
/// apagado cuando llega el `Shutdown` (el bus ES el punto de entrada al
/// "proceso", como en un executor real).
struct MockBus {
    proc: Arc<MockProc>,
}

impl BusTransport for MockBus {
    fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
        if self.proc.is_dead() {
            return Err(ArcaError::Internal("mock: bus cerrado (instancia muerta)"));
        }
        self.proc.ctl.lock().expect("ctl").push(msg.clone());
        if matches!(msg, ControlMsg::Shutdown { .. }) {
            self.proc.on_shutdown();
        }
        Ok(())
    }

    fn send_signal(&mut self, s: &SignalMsg) -> Res<()> {
        if self.proc.is_dead() {
            return Err(ArcaError::Internal("mock: bus cerrado (instancia muerta)"));
        }
        self.proc.sig.lock().expect("sig").push(*s);
        Ok(())
    }

    fn set_deadline(&mut self, ms: u32) {
        self.proc.deadline_ms.store(ms, Ordering::Relaxed);
    }
}

/// Executor de test: spawn inmediato + política de apagado configurable.
///
/// Uso: `new_bus(instance)` ANTES de `launch` (el "host" crea el
/// transporte, como hará host-core con el UDS real), luego
/// `launch(spec, bus)` — por el trait, idealmente vía `&dyn Executor`.
pub(super) struct MockExecutor {
    name: &'static str,
    policy: ShutdownPolicy,
    only_wasm: bool,
    /// "Procesos" por instancia (los crea `new_bus`).
    procs: Mutex<HashMap<InstanceId, Arc<MockProc>>>,
    /// Drivers por instancia (bookkeeping para inspección del test).
    drivers: Mutex<HashMap<InstanceId, HandleDriver>>,
}

impl MockExecutor {
    /// Mock "nativo" (soporta artefactos no-wasm) con la política dada.
    pub(super) fn with_policy(policy: ShutdownPolicy) -> Self {
        Self {
            name: "mock",
            policy,
            only_wasm: false,
            procs: Mutex::new(HashMap::new()),
            drivers: Mutex::new(HashMap::new()),
        }
    }

    /// Mock wasm (solo soporta `.wasm`), para discriminar `supports`.
    pub(super) fn wasm(policy: ShutdownPolicy) -> Self {
        Self {
            name: "mock-wasm",
            policy,
            only_wasm: true,
            procs: Mutex::new(HashMap::new()),
            drivers: Mutex::new(HashMap::new()),
        }
    }

    /// Crea el bus del "proceso" ANTES de `launch` (rol host-core).
    pub(super) fn new_bus(&self, instance: InstanceId) -> BusHandle {
        let proc = Arc::new(MockProc::new(self.policy));
        self.procs
            .lock()
            .expect("procs")
            .insert(instance, proc.clone());
        BusHandle::new(Arc::new(Mutex::new(MockBus { proc })))
    }

    /// Inyección controlada de eventos (avanza estados desde el test).
    pub(super) fn emit(&self, instance: InstanceId, ev: AppEvent) -> bool {
        self.drivers
            .lock()
            .expect("drivers")
            .get(&instance)
            .is_some_and(|d| d.emit(ev))
    }

    /// Mensajes de control que el "proceso" recibió (aserciones).
    pub(super) fn ctl_sent(&self, instance: InstanceId) -> Vec<ControlMsg> {
        self.proc(instance)
            .map(|p| p.ctl.lock().expect("ctl").clone())
            .unwrap_or_default()
    }

    /// Señales que el "proceso" recibió.
    pub(super) fn signal_sent(&self, instance: InstanceId) -> Vec<SignalMsg> {
        self.proc(instance)
            .map(|p| p.sig.lock().expect("sig").clone())
            .unwrap_or_default()
    }

    /// Último deadline fijado en el transporte.
    pub(super) fn deadline_ms(&self, instance: InstanceId) -> u32 {
        self.proc(instance)
            .map(|p| p.deadline_ms.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// ¿El "proceso" sigue vivo?
    pub(super) fn proc_alive(&self, instance: InstanceId) -> bool {
        self.proc(instance).is_some_and(|p| !p.is_dead())
    }

    /// ¿El host aún sostiene el handle? (detección de leaks post-drop).
    pub(super) fn handle_attached(&self, instance: InstanceId) -> bool {
        self.drivers
            .lock()
            .expect("drivers")
            .get(&instance)
            .is_some_and(|d| d.is_attached())
    }

    /// Instancias lanzadas (bookkeeping del mock).
    pub(super) fn instance_count(&self) -> usize {
        self.drivers.lock().expect("drivers").len()
    }

    fn proc(&self, instance: InstanceId) -> Option<Arc<MockProc>> {
        self.procs.lock().expect("procs").get(&instance).cloned()
    }
}

impl Executor for MockExecutor {
    fn name(&self) -> &'static str {
        self.name
    }

    fn supports(&self, spec: &AppSpec) -> Res<bool> {
        let es_wasm = spec.artifact.path.extension() == Some(std::ffi::OsStr::new("wasm"));
        Ok(if self.only_wasm { es_wasm } else { !es_wasm })
    }

    fn launch(&self, spec: AppSpec, bus: BusHandle) -> Res<AppHandle> {
        // Defensa en profundidad (spec 13 §5, fila 3): aunque el host debe
        // llamar supports() antes, re-chequeamos aquí.
        if !self.supports(&spec)? {
            return Err(ArcaError::Internal("mock: launch de spec no soportado"));
        }
        let proc = self.proc(spec.instance).ok_or(ArcaError::Internal(
            "mock: new_bus no llamado antes de launch",
        ))?;
        let (handle, driver) = AppHandle::new_pair(spec.app_id.clone(), spec.instance, bus);
        // Conecta bus ↔ driver (en un executor real: watcher del proceso).
        *proc.driver.lock().expect("driver") = Some(driver.clone());
        self.drivers
            .lock()
            .expect("drivers")
            .insert(spec.instance, driver.clone());
        // Spawn inmediato: el handshake ocurre al instante (decisión del
        // mock; el driver del test puede inyectar más eventos luego).
        let _ = driver.emit(AppEvent::Hello);
        let _ = driver.emit(AppEvent::Ready);
        // Kill hook, como un executor real: SIGKILL simulado.
        let p = proc;
        driver.install_kill(move || p.kill_by_host());
        Ok(handle)
    }
}

// ─── helpers de test ───────────────────────────────────────────────────

fn app_id() -> AppId {
    AppId::new("com.example.app").expect("AppId de test válida")
}

fn spec_for(instance: u64, wasm: bool) -> AppSpec {
    let ext = if wasm { "wasm" } else { "so" };
    AppSpec {
        app_id: app_id(),
        instance: InstanceId::new(instance),
        artifact: ArtifactRef {
            path: PathBuf::from(format!("/data/apps/x/bin/app.{ext}")),
            hash: Digest::of(b"artefacto del mock"),
            size_bytes: 19,
        },
        caps: vec![Capability::Notify],
        dirs: AppDirs {
            app_dir: PathBuf::from("/data/apps/x"),
            vault_dir: PathBuf::from("/data/vault/x"),
        },
        respawn: RespawnPolicy::OnCrash,
        sync_ui: false,
    }
}

fn drain(rx: &Receiver<AppEvent>) -> Vec<AppEvent> {
    let mut v = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        v.push(ev);
    }
    v
}

/// Bus nulo para los property tests del par handle/driver.
fn bus_nulo() -> BusHandle {
    struct Nulo;
    impl BusTransport for Nulo {
        fn send_ctl(&mut self, _: &ControlMsg) -> Res<()> {
            Ok(())
        }
        fn send_signal(&mut self, _: &SignalMsg) -> Res<()> {
            Ok(())
        }
        fn set_deadline(&mut self, _: u32) {}
    }
    BusHandle::new(Arc::new(Mutex::new(Nulo)))
}

fn pair_nuevo() -> (AppHandle, HandleDriver) {
    AppHandle::new_pair(app_id(), InstanceId::new(42), bus_nulo())
}

// ─── tests del mock (spec 13 §6) ───────────────────────────────────────

mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Ciclo completo POR EL TRAIT (como lo usará host-core): spawn
    /// inmediato, shutdown con cortesía, stream Spawned→Hello→Ready→Dead,
    /// idempotencia y aserciones del bus.
    #[test]
    fn mock_ciclo_completo_por_el_trait() -> Res<()> {
        let exec = MockExecutor::with_policy(ShutdownPolicy::ExitInmediato);
        let spec = spec_for(7, false);
        let bus = exec.new_bus(spec.instance);

        // Dinámico: así lo consumirá host-core (trait objeto).
        let exec_dyn: &dyn Executor = &exec;
        assert_eq!(exec_dyn.name(), "mock");
        assert!(exec_dyn.supports(&spec)?);
        let handle = exec_dyn.launch(spec, bus)?;

        let rx = handle.on_state_change();
        assert_eq!(handle.id(), InstanceId::new(7));
        // launch es síncrono hasta spawn aceptado; el handshake del mock
        // ya ocurrió, así que el estado ya avanzó a Ready.
        assert_eq!(handle.state(), AppState::Ready);

        let status = handle.shutdown(Duration::from_millis(100))?;
        assert_eq!(
            status,
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
        // Sin leaks del executor: solo el host sostiene el inner.
        assert_eq!(handle.inner_strong_count(), 1);

        // Contrato de stream: Spawned→Hello→Ready→Dead, en ese orden.
        assert_eq!(
            drain(&rx),
            vec![
                AppEvent::Spawned,
                AppEvent::Hello,
                AppEvent::Ready,
                AppEvent::Dead {
                    reason: DeathReason::Exit { code: 0 },
                    minidump: None
                },
            ]
        );

        // Invariante: todo mensaje al proceso pasa por el bus.
        assert_eq!(
            exec.ctl_sent(InstanceId::new(7)),
            vec![ControlMsg::Shutdown {
                reason: ShutdownReason::User
            }]
        );

        // Idempotencia: shutdown tras Dead = mismo estado, sin duplicar.
        let again = handle.shutdown(Duration::ZERO)?;
        assert_eq!(again, status);
        assert!(drain(&rx).is_empty(), "sin eventos nuevos");
        assert_eq!(
            exec.ctl_sent(InstanceId::new(7)).len(),
            1,
            "sin segundo Shutdown"
        );
        assert!(!exec.proc_alive(InstanceId::new(7)), "el proceso murió");
        Ok(())
    }

    /// Política IgnoraGrace: la cortesía se envía, el grace vence, el
    /// kill hook del executor produce la muerte (Signaled 9).
    #[test]
    fn mock_ignora_grace_muere_por_kill_hook() -> Res<()> {
        let exec = MockExecutor::with_policy(ShutdownPolicy::IgnoraGrace);
        let spec = spec_for(8, false);
        let bus = exec.new_bus(spec.instance);
        let handle = exec.launch(spec, bus)?;
        let rx = handle.on_state_change();

        let t0 = std::time::Instant::now();
        let status = handle.shutdown(Duration::from_millis(30))?;
        assert!(
            t0.elapsed() >= Duration::from_millis(30),
            "el grace se respeta"
        );

        assert_eq!(
            status,
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
        // La cortesía viajó igual (y fue ignorada).
        assert_eq!(
            exec.ctl_sent(InstanceId::new(8)),
            vec![ControlMsg::Shutdown {
                reason: ShutdownReason::User
            }]
        );
        // Tras la muerte, el bus rechaza y send también.
        assert!(handle.send(&ControlMsg::Pause).is_err());
        Ok(())
    }

    /// Política MuereConSenal: muere por SIGTERM al recibir Shutdown.
    #[test]
    fn mock_muere_con_senal() -> Res<()> {
        let exec = MockExecutor::with_policy(ShutdownPolicy::MuereConSenal);
        let spec = spec_for(9, false);
        let bus = exec.new_bus(spec.instance);
        let handle = exec.launch(spec, bus)?;
        let rx = handle.on_state_change();

        let status = handle.shutdown(Duration::from_millis(100))?;
        assert_eq!(
            status,
            ExitStatus {
                code: 0,
                signal: Some(15)
            }
        );
        assert_eq!(
            handle.state(),
            AppState::Dead {
                reason: DeathReason::Signaled { signal: 15 }
            }
        );
        assert_eq!(
            drain(&rx),
            vec![
                AppEvent::Spawned,
                AppEvent::Hello,
                AppEvent::Ready,
                AppEvent::Dead {
                    reason: DeathReason::Signaled { signal: 15 },
                    minidump: None
                },
            ]
        );
        Ok(())
    }

    /// supports() discrimina por artefacto y launch defiende.
    #[test]
    fn mock_supports_discrimina_extension() -> Res<()> {
        let nat = MockExecutor::with_policy(ShutdownPolicy::ExitInmediato);
        let wasm = MockExecutor::wasm(ShutdownPolicy::ExitInmediato);
        assert!(nat.supports(&spec_for(1, false))?);
        assert!(!nat.supports(&spec_for(1, true))?);
        assert!(wasm.supports(&spec_for(1, true))?);
        assert!(!wasm.supports(&spec_for(1, false))?);
        assert_eq!(wasm.name(), "mock-wasm");

        // Defensa en profundidad: launch de no-soportado → error tipado.
        let bus = wasm.new_bus(InstanceId::new(2));
        let r = wasm.launch(spec_for(2, false), bus);
        assert!(matches!(r, Err(ArcaError::Internal(_))));
        Ok(())
    }

    /// launch sin `new_bus` previo: error tipado (wiring roto del host).
    #[test]
    fn mock_launch_sin_bus_previo_falla() {
        let exec = MockExecutor::with_policy(ShutdownPolicy::ExitInmediato);
        // Bus de OTRA instancia: el wiring por la instancia 3 no existe.
        let bus = exec.new_bus(InstanceId::new(999));
        let r = exec.launch(spec_for(3, false), bus);
        assert!(matches!(r, Err(ArcaError::Internal(_))));
        assert_eq!(exec.instance_count(), 0);
    }

    /// Inyección controlada: salud no muta ciclo (spec 13 §5, fila 1).
    #[test]
    fn mock_emit_inyecta_salud_sin_mutar_ciclo() -> Res<()> {
        let exec = MockExecutor::with_policy(ShutdownPolicy::ExitInmediato);
        let spec = spec_for(4, false);
        let bus = exec.new_bus(spec.instance);
        // El host conserva su clone del bus (QoS hints van por ahí).
        let handle = exec.launch(spec, bus.clone())?;
        let rx = handle.on_state_change();

        assert!(exec.emit(InstanceId::new(4), AppEvent::FrameStalled { frames: 3 }));
        assert!(exec.emit(InstanceId::new(4), AppEvent::Unhealthy { since_ms: 1200 }));
        assert_eq!(handle.state(), AppState::Ready, "salud ≠ ciclo");
        assert!(drain(&rx).contains(&AppEvent::Unhealthy { since_ms: 1200 }));

        // set_deadline llega al transporte (hint de QoS del host).
        bus.set_deadline(77);
        assert_eq!(exec.deadline_ms(InstanceId::new(4)), 77);
        assert_eq!(
            exec.signal_sent(InstanceId::new(4)),
            Vec::<SignalMsg>::new()
        );
        Ok(())
    }

    /// DoS: 1000 launch/shutdown seguidos — un Dead por instancia, sin
    /// leaks de inner (conteo fuerte == 1 en vivo; tras drop, desapego).
    #[test]
    fn dos_1000_launch_shutdown_sin_leak() -> Res<()> {
        const N: u64 = 1000;
        let exec = MockExecutor::with_policy(ShutdownPolicy::ExitInmediato);
        let mut deads = 0usize;

        for i in 1..=N {
            let spec = spec_for(i, false);
            let bus = exec.new_bus(spec.instance);
            let handle = exec.launch(spec, bus)?;
            let rx = handle.on_state_change();

            // El host es el único dueño fuerte del inner (executor va por
            // Weak): un clone temporal demuestra el conteo correcto.
            let clon = handle.clone();
            assert_eq!(handle.inner_strong_count(), 2);
            drop(clon);
            assert_eq!(
                handle.inner_strong_count(),
                1,
                "iter {i}: leak del executor"
            );

            let status = handle.shutdown(Duration::from_millis(20))?;
            assert_eq!(status.code, 0, "iter {i}");

            // Invariante deathWatch: Dead SIEMPRE llega.
            while let Ok(ev) = rx.try_recv() {
                if matches!(ev, AppEvent::Dead { .. }) {
                    deads += 1;
                }
            }
            drop(rx);
            drop(handle);
        }

        assert_eq!(deads, N as usize, "exactamente un Dead por instancia");
        assert_eq!(exec.instance_count(), N as usize);
        // Tras soltarlos todos: ningún inner sobrevive (anti-leak RAII).
        for i in 1..=N {
            assert!(
                !exec.handle_attached(InstanceId::new(i)),
                "inner {i} sigue vivo tras drop: leak"
            );
        }
        Ok(())
    }

    // ─── property tests (spec 13 §6: contrato SIEMPRE consistente) ──────

    fn arb_death() -> BoxedStrategy<DeathReason> {
        prop_oneof![
            Just(DeathReason::Lost),
            Just(DeathReason::KilledByHost),
            (-2000i32..2000).prop_map(|code| DeathReason::Exit { code }),
            (1i32..64).prop_map(|signal| DeathReason::Signaled { signal }),
        ]
        .boxed()
    }

    fn arb_minidump() -> BoxedStrategy<Option<PathBuf>> {
        proptest::option::of(Just(PathBuf::from("/data/minidumps/x.dmp"))).boxed()
    }

    /// Cualquier evento (incluye duplicados y Dead con razón arbitraria).
    fn arb_event() -> BoxedStrategy<AppEvent> {
        prop_oneof![
            Just(AppEvent::Spawned),
            Just(AppEvent::Hello),
            Just(AppEvent::Ready),
            Just(AppEvent::Paused),
            Just(AppEvent::Resumed),
            (0u64..u64::MAX / 4).prop_map(|since_ms| AppEvent::Unhealthy { since_ms }),
            (0u64..u64::MAX / 4).prop_map(|frames| AppEvent::FrameStalled { frames }),
            (arb_death(), arb_minidump())
                .prop_map(|(reason, minidump)| AppEvent::Dead { reason, minidump }),
        ]
        .boxed()
    }

    /// Eventos intermedios del contrato (sin Spawned/Dead).
    fn arb_middle() -> BoxedStrategy<AppEvent> {
        prop_oneof![
            Just(AppEvent::Hello),
            Just(AppEvent::Ready),
            Just(AppEvent::Paused),
            Just(AppEvent::Resumed),
            (0u64..u64::MAX / 4).prop_map(|since_ms| AppEvent::Unhealthy { since_ms }),
            (0u64..u64::MAX / 4).prop_map(|frames| AppEvent::FrameStalled { frames }),
        ]
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// P1 — Máquina dura: ante secuencias ARBITRARIAS (Spawned
        /// duplicados, Dead duplicados, eventos tras la muerte), el stream
        /// entregado SIEMPRE cumple: arranca en Spawned, Dead ≤ 1 y es el
        /// último; tras Dead no se acepta nada.
        #[test]
        fn prop_maquina_filtra_secuencias_arbitrarias(seq in prop::collection::vec(arb_event(), 0..24)) {
            let (handle, driver) = pair_nuevo();
            let rx = handle.on_state_change();

            for ev in seq {
                let _ = driver.emit(ev);
            }

            let out = drain(&rx);
            prop_assert!(
                matches!(out.first(), Some(AppEvent::Spawned)),
                "el stream siempre arranca en Spawned: {out:?}"
            );
            let deads = out.iter().filter(|e| matches!(e, AppEvent::Dead { .. })).count();
            prop_assert!(deads <= 1, "Dead es one-shot: {deads} en {out:?}");
            if deads == 1 {
                prop_assert!(
                    matches!(out.last(), Some(AppEvent::Dead { .. })),
                    "Dead es el último: {out:?}"
                );
                prop_assert!(!driver.emit(AppEvent::Hello), "nada tras Dead");
                prop_assert!(
                    !driver.report_death(DeathReason::Lost, None),
                    "Dead duplicado rechazado"
                );
                prop_assert!(matches!(handle.state(), AppState::Dead { .. }), "estado terminal");
            }
        }

        /// P2 — Contrato canónico: Spawned→Hello→Ready→(intermedios)→Dead
        /// es SIEMPRE consistente: orden exacto de entrega, Dead único al
        /// final, estado y ExitStatus coherentes, y shutdown tras Dead
        /// devuelve el estado ya reportado sin duplicar nada.
        #[test]
        fn prop_contrato_canonico_consistente(
            middles in prop::collection::vec(arb_middle(), 0..16),
            reason in arb_death(),
            minidump in arb_minidump(),
        ) {
            let (handle, driver) = pair_nuevo();
            let rx = handle.on_state_change();

            prop_assert!(driver.emit(AppEvent::Hello));
            prop_assert!(driver.emit(AppEvent::Ready));
            for m in middles.clone() {
                prop_assert!(driver.emit(m));
            }
            prop_assert!(driver.report_death(reason, minidump.clone()));
            // Duplicado y post-mortem: rechazados.
            prop_assert!(!driver.report_death(reason, None));
            prop_assert!(!driver.emit(AppEvent::Resumed));

            let mut esperado = vec![AppEvent::Spawned, AppEvent::Hello, AppEvent::Ready];
            esperado.extend(middles);
            esperado.push(AppEvent::Dead { reason, minidump });
            prop_assert_eq!(drain(&rx), esperado);

            prop_assert_eq!(handle.state(), AppState::Dead { reason });
            // shutdown tras Dead: el estado ya reportado, sin duplicar.
            let st = handle.shutdown(Duration::ZERO).expect("dead ya reportado");
            prop_assert_eq!(st, ExitStatus::from(reason));
            prop_assert!(drain(&rx).is_empty(), "sin eventos tras shutdown idempotente");
        }
    }
}
