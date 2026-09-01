# arca-exec-abi

El **punto de intercambio de backends** de ejecución (ADR-001, spec 13):
trait `Executor` + `AppHandle` + eventos de ciclo de vida. Define lo que
native (`arca-exec-native`) y wasm (`arca-exec-wasm`) comparten — **el
resto del sistema habla SOLO con este crate**. El mismo ABI sirve para
`arca.home` (sub-app de sistema): cero casos especiales.

- Capa: L1 (`crates/L1-arca-exec-abi/`)
- unsafe: **no**
- Spec: `specs/arca-13-exec-abi.md` (blueprint) · Dominio: exec
- Lints: `#![deny(missing_docs)]`, `deny(clippy::unwrap_used/expect_used)`
  fuera de tests

## API pública

```text
AppSpec { app_id, instance, artifact: ArtifactRef, caps: Vec<Capability>,
          dirs: AppDirs, respawn: RespawnPolicy, sync_ui }
ArtifactRef { path, hash: Digest, size_bytes }
AppDirs { app_dir, vault_dir }

trait Executor: Send + Sync {
    fn name(&self) -> &'static str;                 // "native" | "wasm-wamr" | "wasm-wasmtime"
    fn supports(&self, spec: &AppSpec) -> Res<bool>;
    fn launch(&self, spec: AppSpec, bus: BusHandle) -> Res<AppHandle>;
}
EXECUTOR_NAME_NATIVE / EXECUTOR_NAME_WASM_WAMR / EXECUTOR_NAME_WASM_WASMTIME

AppHandle { id, state, send, shutdown(grace) -> Res<ExitStatus>, on_state_change }
    + AppHandle::new_pair(app_id, instance, bus) -> (AppHandle, HandleDriver)   [executores]
HandleDriver { emit(AppEvent) -> bool, report_death(reason, minidump) -> bool,
               install_kill(Fn() -> Res<()>), is_attached() -> bool }           [executores]

trait BusTransport: Send { send_ctl, send_signal, set_deadline(ms) }
BusHandle { new(Arc<Mutex<dyn BusTransport>>), send_ctl, send_signal, set_deadline }

AppState   = Spawning | Ready | Running | Paused | Dead { reason }
DeathReason= Exit{code} | Signaled{signal} | Lost | KilledByHost
ExitStatus { code, signal: Option<i32> } + success() + From<DeathReason>
AppEvent   = Spawned | Hello | Ready | Paused | Resumed
           | Dead { reason, minidump } | Unhealthy { since_ms } | FrameStalled { frames }
```

## Reparto de responsabilidades

- **Host (host-core)**: construye el `BusTransport` real (UDS de
  `arca-ipc` o in-proc), lo envuelve en `BusHandle`, elige backend con
  `supports`, lanza con `launch`, observa `on_state_change` y finaliza
  con `shutdown`.
- **Executor (native/wasm)**: tras aceptar el spawn llama
  `AppHandle::new_pair` (que emite `Spawned`), emite el ciclo por el
  `HandleDriver` (su deathWatch reporta con `report_death`) e instala
  `install_kill` para la escalada de `shutdown`.

## Invariantes garantizadas por el handle

- `Spawned` es SIEMPRE el primer evento (lo emite la construcción del
  par, no el executor) y no puede repetirse.
- `Dead` es **one-shot y terminal**: llega exactamente una vez, es el
  último evento, y ningún evento se acepta después (spec 13 §5 "Dead
  duplicado" / "dos watchers").
- Eventos intermedios solo entre `Spawned` y `Dead`. `Hello` no muta
  estado; `Ready`/`Paused`/`Resumed` sí; `Unhealthy`/`FrameStalled` son
  salud, no ciclo.
- Anti-leak: el driver lleva `Weak` del inner; cuando el host suelta
  todos los clones, `emit` → `false` e `is_attached` → `false` (el
  watcher debe terminar). El drop del handle NO mata al proceso (como
  `std::process::Child`): para terminar, `shutdown`.
- `shutdown(grace)`: (1) idempotente si ya hay Dead; (2) `Shutdown{User}`
  por el bus + espera `grace`; (3) kill hook del executor + asentamiento
  (500 ms); (4) si nada reporta, sintetiza `Dead{KilledByHost}` (el
  one-shot hace que un reporte real en la carrera gane). Sin hook y sin
  muerte natural → `Err(ArcaError::Internal)` (executor incompleto).

## Desviaciones de la spec (decididas por el arquitecto)

1. `caps: CapabilitySet` → `caps: Vec<Capability>`: el `CapabilitySet`
   canónico vive en `arca-permissions` y docs/08 §3 lo prohíbe como
   dependencia de este crate.
2. `ArtifactRef` propio (path + digest blake3 + tamaño) en vez del
   "path bin / bytes wasm + hash" implícito.
3. `RespawnPolicy` re-exportada de `arca-pkg-model` (no duplicada).
4. `BusTransport` trait objeto-seguro + `BusHandle` clonable sobre
   `Arc<Mutex<dyn BusTransport>>` (los implementarán arca-ipc y
   arca-exec-wasm).
5. Tipos auxiliares `AppState`/`DeathReason`/`ExitStatus`/`AppDirs`
   definidos aquí con convención documentada (`From<DeathReason> for
   ExitStatus`: Signaled → señal sin code; Lost → −1; KilledByHost →
   señal 9).

## Decisiones propias (documentadas)

- `AppHandle::new_pair` + `HandleDriver`: superficie necesaria para que
  T15/T25 puedan construir handles (la spec no definía la construcción).
  El driver emite por el inner con las garantías de la máquina de
  eventos; el host jamás lo toca.
- `shutdown` envía `Shutdown { reason: User }`: v1 no parametriza el
  motivo (firma de la spec); quien necesite otro motivo puede mandar
  `send(ControlMsg::Shutdown { .. })` antes de `shutdown`.
- `on_state_change` entrega el receiver primario UNA vez (con los
  eventos tempranos encolados); llamadas posteriores crean un suscriptor
  "tardío" (canal nuevo, solo eventos futuros). Se prefirió al error
  tipado para conservar la firma del contrato.
- Extra dep `tracing` (ADR-014: logging obligatorio; mismo caso
  documentado que T07/arca-store).
- Mutex envenenado = modo degenerado fail-closed: `state()` reporta
  `Dead{Lost}`, `emit` rechaza, `shutdown` acaba en `Err(Internal)`.
  Nunca panic fuera de tests.

## Tests (25)

- `mock.rs` (cfg test): `MockExecutor` con `ShutdownPolicy`
  (`ExitInmediato`/`IgnoraGrace`/`MuereConSenal`) + `MockBus` que encola
  `ControlMsg` para aserciones y dispara la política al recibir
  `Shutdown`.
- 9 tests de handle (ciclo completo, tardíos, 4 variantes de shutdown,
  send a muerto, RAII/Weak, one-shots, veneno).
- 6 tests de mock por el trait (incl. `&dyn Executor`), 1 de spec, 1 de
  estado, 2 de bus.
- 2 property tests (proptest, 128 casos): secuencias arbitrarias
  (Spawned primero, Dead ≤ 1 y último, nada tras Dead) y contrato
  canónico Spawned→Hello→Ready→(intermedios)→Dead SIEMPRE consistente
  (+ shutdown idempotente tras Dead).
- DoS: 1000 launch/shutdown — un Dead por instancia, strong_count == 1
  en vivo, ningún inner vivo tras drop.
- 1 doctest del patrón completo de uso.

## Verificación

```sh
cargo fmt -p arca-exec-abi
cargo clippy -p arca-exec-abi --all-targets -- -D warnings
cargo test -p arca-exec-abi
```
