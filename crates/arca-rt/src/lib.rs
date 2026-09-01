//! arca-rt (L1): runtime mínimo de sub-apps Arca, **lado del hijo**.
//!
//! En F0-F1 solo expone el protocolo AIPC que consume `arca-ping`.
//! F2 agrega: sandbox real por sub-app (seccomp-BPF), display-list UI
//! (tessellation → rkyv → shm) y AIPC con memfd + SO_PEERCRED.

/// Protocolo AIPC compartido con el host (re-export de `arca-ipc`).
pub mod ipc {
    pub use arca_ipc::*;
}
