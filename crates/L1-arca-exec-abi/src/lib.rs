//! `arca-exec-abi` — el punto de intercambio de backends de ejecución
//! (spec 13, ADR-001).
//!
//! Capa L1 · unsafe: **no** · Dominio: exec (blueprint `exec-native.mmd` /
//! `exec-wasm.mmd`; el grafo del repositorio aún no tiene fichero propio
//! para este dominio — pendiente con T15/T25).
//!
//! ADR-001 exige que native y wasm sean **intercambiables** tras un único
//! contrato: [`Executor`] + [`AppHandle`] + eventos de ciclo de vida. El
//! resto del sistema (host-core, launcher, watchdog) habla SOLO con este
//! crate — jamás con `arca-exec-native`/`arca-exec-wasm` directamente. El
//! mismo ABI sirve para `arca.home` (sub-app de sistema): cero casos
//! especiales.
//!
//! # Reparto de responsabilidades
//!
//! - **Host (host-core)**: construye el [`BusTransport`] real (UDS de
//!   `arca-ipc` o in-proc), lo envuelve en [`BusHandle`], elige backend
//!   con [`Executor::supports`] y lanza con [`Executor::launch`]; observa
//!   [`AppHandle::on_state_change`] y coordina el final con
//!   [`AppHandle::shutdown`].
//! - **Executor (native/wasm)**: tras aceptar el spawn construye el par
//!   [`AppHandle::new_pair`] (que emite [`AppEvent::Spawned`]), emite el
//!   ciclo por el [`HandleDriver`] (su deathWatch reporta con
//!   [`HandleDriver::report_death`]) e instala
//!   [`HandleDriver::install_kill`] para la escalada de `shutdown`.
//!
//! # Invariantes (spec 13 §4)
//!
//! - **El trait es mínimo y estable**: 3 métodos + handle; si necesitas
//!   más, es un redesign del ABI.
//! - [`Executor::launch`] es síncrono **solo hasta "spawn aceptado"**: el
//!   handle nace en [`AppState::Spawning`] con [`AppEvent::Spawned`] ya
//!   emitido; ni Hello ni Ready se esperan aquí — la vida asincrónica
//!   llega por eventos.
//! - Todo mensaje al proceso pasa por [`BusHandle`]: ningún executor habla
//!   directo con shm/ipc internos.
//! - [`AppEvent::Dead`] SIEMPRE llega y es one-shot: llega exactamente
//!   una vez, es el último evento del stream y ningún evento se acepta
//!   después (garantizado por la máquina de eventos del handle; la
//!   existencia del reporte la garantiza el deathWatch del executor, con
//!   síntesis de respaldo en [`AppHandle::shutdown`]).
//!
//! # Máquina de eventos (garantías del handle)
//!
//! - [`AppEvent::Spawned`] lo emite la propia construcción del handle:
//!   es SIEMPRE el primer evento y no puede repetirse.
//! - Eventos intermedios solo se aceptan entre `Spawned` y `Dead`.
//! - `Hello` no muta estado (sigue `Spawning`); `Ready` → [`AppState::Ready`];
//!   `Paused`/`Resumed` → [`AppState::Paused`]/[`AppState::Running`].
//!   `Unhealthy`/`FrameStalled` son **salud, no ciclo**: no mutan estado
//!   (spec 13 §5, fila 1: "state confundido con salud").
//!
//! # Desviaciones de spec 13 (decididas por el arquitecto)
//!
//! 1. `caps: CapabilitySet` → `caps: Vec<Capability>`: el canónico
//!    `CapabilitySet` vive en `arca-permissions` y docs/08 §3 lo prohíbe
//!    como dependencia de este crate.
//! 2. [`ArtifactRef`] propio (path + digest blake3 + tamaño) en vez del
//!    "path bin / bytes wasm + hash" implícito del contrato.
//! 3. [`RespawnPolicy`] se re-exporta de `arca-pkg-model` (no se duplica).
//! 4. [`BusTransport`] trait objeto-seguro mínimo + [`BusHandle`]
//!    clonable sobre `Arc<Mutex<dyn BusTransport>>`; lo implementarán
//!    `arca-ipc` (UDS real, T12) y `arca-exec-wasm` (in-proc, T25).
//! 5. Tipos auxiliares [`AppState`]/[`DeathReason`]/[`ExitStatus`]/
//!    [`AppDirs`] definidos aquí con convención documentada.
//!
//! # Decisiones de diseño propias (documentadas, ver README)
//!
//! - [`AppHandle::new_pair`] + [`HandleDriver`]: superficie para
//!   implementadores de executor (el contrato de spec 13 no define cómo
//!   se construye el handle; sin esto el crate no sería utilizable por
//!   T15/T25). El driver lleva `Weak` → anti-leak RAII.
//! - `shutdown` envía `Shutdown { reason: User }` (v1 no parametriza el
//!   motivo) y escala con el kill hook tras el grace.
//! - `on_state_change` entrega el receiver primario UNA vez; llamadas
//!   posteriores crean un suscriptor "tardío" (solo eventos futuros).
//! - Extra dep `tracing` (ADR-014: logging obligatorio; mismo caso que
//!   T07/arca-store).
//!
//! # Ejemplo
//!
//! Patrón completo de uso (transporte y executor de juguete in-proc):
//!
//! ```
//! use std::path::PathBuf;
//! use std::sync::{Arc, Mutex};
//! use std::time::Duration;
//!
//! use arca_exec_abi::{
//!     AppDirs, AppEvent, AppHandle, AppSpec, AppState, ArtifactRef, BusHandle,
//!     BusTransport, DeathReason, Executor, RespawnPolicy,
//! };
//! use arca_protocol::{ControlMsg, SignalMsg};
//! use arca_types::{AppId, Capability, Digest, InstanceId, Res};
//!
//! /// Transporte in-proc de juguete (el real: arca-ipc UDS / wasm in-proc).
//! struct EcoBus;
//! impl BusTransport for EcoBus {
//!     fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
//!         let _ = msg; // un transporte real codifica y envía por AIPC
//!         Ok(())
//!     }
//!     fn send_signal(&mut self, s: &SignalMsg) -> Res<()> {
//!         let _ = s;
//!         Ok(())
//!     }
//!     fn set_deadline(&mut self, ms: u32) {
//!         let _ = ms;
//!     }
//! }
//!
//! /// Executor de juguete: muestra el patrón new_pair + driver + kill.
//! struct EcoExec;
//! impl Executor for EcoExec {
//!     fn name(&self) -> &'static str {
//!         "eco"
//!     }
//!     fn supports(&self, spec: &AppSpec) -> Res<bool> {
//!         Ok(spec.artifact.path.extension() != Some(std::ffi::OsStr::new("wasm")))
//!     }
//!     fn launch(&self, spec: AppSpec, bus: BusHandle) -> Res<AppHandle> {
//!         // (executor real: fork+exec / instancia wasm AQUÍ; fallo → Err)
//!         let (handle, driver) = AppHandle::new_pair(spec.app_id, spec.instance, bus);
//!         driver.emit(AppEvent::Hello); // handshake C→H
//!         driver.emit(AppEvent::Ready); // runtime listo
//!         let d = driver.clone();       // deathWatch (aquí sería un hilo)
//!         driver.install_kill(move || {
//!             d.report_death(DeathReason::KilledByHost, None);
//!             Ok(())
//!         });
//!         Ok(handle)
//!     }
//! }
//!
//! # fn main() -> Res<()> {
//! let app = AppId::new("com.example.app")?;
//! let spec = AppSpec {
//!     app_id: app,
//!     instance: InstanceId::new(1),
//!     artifact: ArtifactRef {
//!         path: PathBuf::from("/data/apps/com.example.app/bin/app.so"),
//!         hash: Digest::of(b"binario"),
//!         size_bytes: 7,
//!     },
//!     caps: vec![Capability::Notify],
//!     dirs: AppDirs {
//!         app_dir: PathBuf::from("/data/apps/com.example.app"),
//!         vault_dir: PathBuf::from("/data/vault/com.example.app"),
//!     },
//!     respawn: RespawnPolicy::OnCrash,
//!     sync_ui: true,
//! };
//!
//! let exec = EcoExec;
//! assert!(exec.supports(&spec)?);
//! let bus = BusHandle::new(Arc::new(Mutex::new(EcoBus)));
//! let handle = exec.launch(spec, bus)?;
//!
//! // El host observa el ciclo (Spawned ya está garantizado por el ABI).
//! let mut eventos = handle.on_state_change();
//! assert_eq!(handle.state(), AppState::Ready);
//! assert!(matches!(eventos.recv(), Ok(AppEvent::Spawned)));
//! assert!(matches!(eventos.recv(), Ok(AppEvent::Hello)));
//!
//! // Apagado con grace; sin muerte natural, el kill del executor decide.
//! let status = handle.shutdown(Duration::from_millis(20))?;
//! assert_eq!(status.signal, Some(9)); // KilledByHost → SIGKILL asumido
//! assert_eq!(
//!     handle.state(),
//!     AppState::Dead { reason: DeathReason::KilledByHost }
//! );
//! // El stream es FIFO: (Ready encolado) y luego Dead, terminal.
//! assert!(matches!(eventos.recv(), Ok(AppEvent::Ready)));
//! assert!(matches!(eventos.recv(), Ok(AppEvent::Dead { .. })));
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

mod bus;
mod handle;
mod spec;
mod state;

#[cfg(test)]
mod mock;

pub use bus::{BusHandle, BusTransport};
pub use handle::{AppHandle, HandleDriver};
pub use spec::{AppDirs, AppSpec, ArtifactRef, RespawnPolicy};
pub use state::{AppEvent, AppState, DeathReason, ExitStatus};

use arca_types::Res;

/// Nombre canónico del executor nativo (viaja a logs/store; estable).
pub const EXECUTOR_NAME_NATIVE: &str = "native";
/// Nombre canónico del executor wasm sobre WAMR.
pub const EXECUTOR_NAME_WASM_WAMR: &str = "wasm-wamr";
/// Nombre canónico del executor wasm sobre wasmtime.
pub const EXECUTOR_NAME_WASM_WASMTIME: &str = "wasm-wasmtime";

/// Punto de intercambio de backends de ejecución (ADR-001).
///
/// Toda la orquestación del host habla con este trait; las
/// implementaciones concretas (`arca-exec-native`, `arca-exec-wasm`)
/// quedan detrás de él. Es **mínimo y estable** por diseño (spec 13 §4):
/// si creces la superficie, es un redesign del ABI.
///
/// Ciclo de uso del host:
/// 1. [`supports`](Executor::supports) para elegir backend (puro).
/// 2. [`launch`](Executor::launch) — síncrono hasta spawn aceptado.
/// 3. La vida asincrónica llega por [`AppHandle::on_state_change`].
pub trait Executor: Send + Sync {
    /// Identidad del backend: uno de [`EXECUTOR_NAME_NATIVE`],
    /// [`EXECUTOR_NAME_WASM_WAMR`] o [`EXECUTOR_NAME_WASM_WASMTIME`]
    /// (los mocks de test usan el suyo). Estable entre versiones: viaja
    /// a logs y a la store.
    fn name(&self) -> &'static str;

    /// ¿Puede este executor lanzar el spec dado? Puro y sin efectos (p.
    /// ej. native mira la arquitectura del artefacto; wasm, la extensión).
    /// `Err` solo para fallos de inspección; "no es para mí" es `Ok(false)`.
    ///
    /// El host SIEMPRE lo consulta antes de `launch` (spec 13 §5, fila
    /// "launch pánico"); las implementaciones pueden re-verificar dentro
    /// de `launch` como defensa en profundidad.
    fn supports(&self, spec: &AppSpec) -> Res<bool>;

    /// Lanza una instancia. **Síncrono solo hasta "spawn aceptado"**:
    /// devuelve un [`AppHandle`] en [`AppState::Spawning`] con
    /// [`AppEvent::Spawned`] ya emitido; ni Hello ni Ready se esperan
    /// aquí (llegan como eventos).
    ///
    /// Toda comunicación posterior con el proceso va por `bus`
    /// (invariante: ningún executor habla directo con shm/ipc internos).
    /// Dos `launch` son dos instancias: no hay idempotencia.
    fn launch(&self, spec: AppSpec, bus: BusHandle) -> Res<AppHandle>;
}
