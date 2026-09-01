# arca-permissions

capabilities → seccomp-BPF (**default-deny**). Dueño del modelo de permisos
de Arca: traduce las capabilities del manifest a un filtro seccomp-BPF
concreto y a concesiones de servicios del host (spec 07, docs/07 §3-§4).

- Capa: L0 (`L0-arca-permissions/`)
- Spec: `specs/arca-07-*.md` (blueprint) · Tarea: T14 · Estado: **implementado**
- unsafe: sí por dominio (BPF) — **cero bloques `unsafe` propios**: la
  frontera FFI (`prctl`/`seccomp(2)`) vive en `seccompiler` (auditado
  upstream); el `unsafe` que queda (fork/_exit) está en los tests E2E con
  comentario de invariante.

## API pública

```rust
use arca_permissions::{
    apply_profile, bpf_digest, build_profile, current_arch, explain,
    CapabilitySet, Decision, LandlockRules, NetPolicy, SandboxProfile,
    BpfProgram, TargetArch, BASE_SYSCALLS,
};

// 1) caps concedidas (el manifest las PIDE; el instalador decide)
let caps = CapabilitySet::from_manifest(&manifest);

// 2) compilar el sandbox para la arch del DISPOSITIVO
let profile = build_profile(&caps, app_dir, vault_dir, TargetArch::aarch64)?;

// 3) en arca-launch: hijo tras fork, ANTES de exec
apply_profile(&profile)?; // NO_NEW_PRIVS + SECCOMP_MODE_FILTER

// 4) panel de diagnóstico
for d in explain(&caps) { /* d.cap, d.effect */ }
```

## Modelo de seguridad (v1)

- **Default-deny**: solo las 60 syscalls de `BASE_SYSCALLS` pasan; todo lo
  demás dispara `SECCOMP_RET_KILL_PROCESS` (muerte por `SIGSYS`, sin
  `errno` negociable).
- **Sin `openat`/`open`**: los archivos van por broker/vault sobre fds
  heredados. Sin `socket`/`connect`/`bind`/`listen`: patrón **"socket
  pasado"** — `net-client`/`net-server` compran fds YA conectados que el
  broker entrega, no la syscall.
- **Las capabilities nunca amplían el BPF** (invariante central, fijada por
  golden test): `fs-vault` añade `vault_dir` a `allowed_paths`;
  `net-*` cambia `NetPolicy::NoNet → BrokerSockets`; el resto va por
  svc-broker (sin syscalls nuevas).
- **Landlock**: stub honesto — `SandboxProfile::landlock` es SIEMPRE
  `None` en v1 (kernel ≥ 5.28 en v2).
- **Determinismo**: `build_profile` es puro; golden del BPF en
  `tests/golden.rs`.

## Golden (blake3 del BPF, 5 sets de referencia)

| arch | hash | instrucciones |
|---|---|---|
| x86_64 (5 sets) | `5589d9159b2886689e8f0ddcaab2bce384a8fb2915eae93fbc0d27280121021f` | 305 |
| aarch64 (5 sets) | `02c9a0124a88ef0492c6951bedad242c96442b4e10865ebe43e99d4d4d36dc00` | 285 |

Los 5 capability-sets comparten hash POR ARQUITECTURA (ninguna capability
amplía el filtro en v1). Si cambia un golden, hubo un cambio de política de
seguridad y debe llevar review explícito.

## Tests (39 verdes)

- 25 unit (capset/syscalls/profile/apply/explain) · 3 doctests.
- 7 golden (5 sets × hash, determinismo 2 builds, arch≠arch, estructura,
  invariantes).
- 4 E2E con `fork` real: `socket(AF_INET)` → muerte por `SIGSYS` con cada
  perfil (incl. `net-client`: la capability NO abre sockets crudos);
  `write(2)` a pipe → exit 0; `openat` → `SIGSYS` (default-deny).

```sh
cargo fmt -p arca-permissions
cargo clippy -p arca-permissions --all-targets -- -D warnings
cargo test -p arca-permissions
```

## Desviaciones del contrato de spec 07 (documentadas en `src/lib.rs`)

1. `build_profile` toma `arch: TargetArch` (la spec no lo incluía): el BPF
   de aarch64 se genera en PC y se aplica en el teléfono. Decisión del
   arquitecto.
2. Tabla de syscalls propia (`src/syscalls.rs`): `seccompiler::SyscallTable`
   es `pub(crate)` y vive tras la feature `json` (inactiva en el workspace);
   se embeben los números del kernel para x86_64/aarch64, verificados contra
   el fuente de seccompiler.
3. `riscv64` se rechaza en v1 (sin tabla). En aarch64 se omiten con `warn`
   las 4 syscalls legacy que la ABI no define (`dup2`, `poll`,
   `epoll_wait`, `readlink`) — en x86_64 no falta ninguna.
4. Ayudas añadidas: `bpf_digest`, `current_arch`, `BASE_SYSCALLS` (público
   para auditoría), `CapabilitySet::iter`.
5. `app_dir` va SIEMPRE en `allowed_paths` (sus propios assets, mediados
   por broker; no es ampliación de capability).
6. El helper de `fork` de los tests E2E vive en `tests/e2e.rs` (no en
   `src/`): `nix` es dev-dependency.
