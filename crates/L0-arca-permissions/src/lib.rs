//! `arca-permissions` — capabilities → seccomp-BPF (default-deny).
//!
//! Capa L0 · unsafe: **sí (BPF)** — satisfecho vía `seccompiler`; este crate
//! no añade bloques `unsafe` propios (ver `apply`).
//! Contrato completo: spec 07 (blueprint) · Grafo: dominio permisos.
//!
//! Dueño del **modelo de permisos** de Arca: traduce capabilities del
//! manifest a filtros seccomp-BPF concretos y a concesiones de servicios
//! (docs/07 §3-§4).
//!
//! # Modelo en una página
//!
//! 1. [`CapabilitySet::from_manifest`] recoge lo que la app PIDE.
//! 2. [`build_profile`] compila el sandbox: filtro **default-deny** (todo lo
//!    que no está en [`BASE_SYSCALLS`] mata el proceso con `KILL_PROCESS`)
//!    + concesiones del host ([`NetPolicy`], `allowed_paths`).
//! 3. [`apply_profile`] lo instala (NO_NEW_PRIVS + SECCOMP_MODE_FILTER) en
//!    el hijo de `fork`, ANTES de `exec`: la app no escapa por arranque.
//! 4. [`explain`] traduce cada capability a texto legible para el panel.
//!
//! # Invariantes (spec 07 §4)
//!
//! - **Default-deny**: la lista blanca mínima es [`BASE_SYSCALLS`]; ni
//!   `openat` ni `socket` viven en ella (archivos y red van por fds que el
//!   host entrega: patrón "socket pasado").
//! - **Determinismo**: `build_profile` es puro; el golden test de
//!   `tests/golden.rs` fija el hash blake3 del BPF por capability-set.
//! - **Las capabilities solo amplían concesiones del host**, nunca syscalls
//!   globales: `net-client` = `send`/`recv` sobre fds YA conectados.
//! - **Landlock v1 = stub honesto**: [`SandboxProfile::landlock`] es siempre
//!   `None` (kernel ≥ 5.28 en v2).
//!
//! # Ejemplo
//!
//! ```no_run
//! # use std::path::Path;
//! use arca_permissions::{build_profile, current_arch, CapabilitySet, TargetArch};
//! use arca_types::Capability;
//!
//! // 1. El manifest pidió caps; el instalador concedió estas:
//! let caps = CapabilitySet::from_iter([Capability::NetClient, Capability::FsVault]);
//!
//! // 2. Compilar el perfil para el DISPOSITIVO (aarch64 en el teléfono;
//! //    current_arch() en dev/CI x86_64):
//! let arch = current_arch().unwrap_or(TargetArch::aarch64);
//! let profile = build_profile(&caps, Path::new("/apps/demo"), Path::new("/vault/demo"), arch)?;
//! assert_eq!(profile.allowed_paths.len(), 2); // app_dir + bóveda
//!
//! // 3. En arca-launch (hijo tras fork, ANTES de exec):
//! //    apply_profile(&profile)?;
//! # Ok::<(), arca_types::ArcaError>(())
//! ```
//!
//! # Enmiendas al contrato de spec 07 (documentadas)
//!
//! - **`build_profile` toma `arch: TargetArch`** (la spec no lo incluía):
//!   el BPF de aarch64 se GENERA en PC y se APLICA en el teléfono. Decisión
//!   del arquitecto T14.
//! - **Tabla de syscalls propia** (`src/syscalls.rs`): `seccompiler::SyscallTable`
//!   es `pub(crate)` + feature `json` (inactiva), así que este crate embebe
//!   los números de syscall del kernel para x86_64/aarch64 con la misma API.
//! - `riscv64` (variante de `seccompiler::TargetArch`) se rechaza en v1:
//!   sin tabla, solo x86_64 (dev) y aarch64 (teléfono).
//! - Ayudas añadidas: [`bpf_digest`] (golden/diagnóstico), [`current_arch`]
//!   (arch del host que corre), [`BASE_SYSCALLS`] público (auditoría),
//!   [`CapabilitySet::iter`] (paneles).
//! - El helper de `fork` para tests E2E vive en `tests/e2e.rs`, no en `src/`:
//!   `nix` es dev-dependency (no disponible para el código de librería).

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

mod apply;
mod capset;
mod explain;
mod profile;
mod syscalls;

pub use apply::apply_profile;
pub use capset::CapabilitySet;
pub use explain::{explain, Decision};
pub use profile::{
    bpf_digest, build_profile, current_arch, LandlockRules, NetPolicy, SandboxProfile,
    BASE_SYSCALLS,
};

// Re-export de conveniencia: el tipo del campo `SandboxProfile::seccomp` y
// el parámetro `arch` de `build_profile` deben ser nombrables sin depender
// directamente de seccompiler.
pub use seccompiler::{BpfProgram, TargetArch};
