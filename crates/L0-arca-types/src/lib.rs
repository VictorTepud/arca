//! `arca-types` — ids, versiones, errores base.
//!
//! Capa L0 · unsafe: solo un wrapper FFI trivial en [`now_mono_ns`].
//! Contrato completo: `specs/arca-01-types.md` del blueprint.
//!
//! Reglas del crate (spec 01 §4):
//! - Cero `unwrap` en no-tests; cero `std::sync` (no hay lógica de concurrencia aquí).
//! - Todo lo que viaje por AIPC entra a los golden tests de `arca-protocol`.
//! - Este crate NO depende de ningún crate Arca (es la raíz del grafo).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod caps;
pub mod digest;
pub mod error;
pub mod ids;
pub mod time;
pub mod version;

pub use caps::Capability;
pub use digest::Digest;
pub use error::{ArcaError, Res};
pub use ids::{AppId, InstanceId, SessionId, WinId};
pub use time::now_mono_ns;
pub use version::ProtoVersion;

/// Versión del protocolo AIPC implementada por este código (AIPC-1.0).
pub const PROTO_VERSION: ProtoVersion = ProtoVersion::new(1, 0);
