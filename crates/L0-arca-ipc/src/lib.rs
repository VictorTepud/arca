//! `arca-ipc` — transporte AIPC: UDS en filesystem + SCM_RIGHTS + handshake.
//!
//! Capa L0 · unsafe: sí-lite (syscalls de socket; cada bloque unsafe lleva
//! invariante). Contrato: `specs/arca-06-*.md`; docs/04 §2-§3.
//!
//! Reglas duras (spec 06 §4):
//! - **Path en filesystem, JAMÁS abstract namespace** (sin control de
//!   acceso — docs/01 §4). [`uds::ensure_filesystem_path`] lo verifica en
//!   `bind`/`connect`.
//! - `SO_PEERCRED` comprobado en CADA accept: UID ≠ propio → cierre y
//!   error (otra app no puede conectarse al runtime del host).
//! - Deadlines en TODAS las ops de bloqueo: `set_deadline` es obligatorio
//!   antes de `recv_frame` (error tipado si falta — decisión documentada
//!   en spec 06 §6 "recv sin deadline").
//! - Máx [`arca_protocol::MAX_FDS_PER_SEND`] fds por send; máx 64 KB de
//!   payload por trama (enforced por arca-protocol).
//! - Sin hilos propios: la multiplexación (epoll) la hace arca-host-core.
//!
//! Nota de capas: `BusTransport` (trait de arca-exec-abi) NO se implanta
//! aquí (L0 no puede depender de L1): lo implanta arca-exec-native sobre
//! [`Conn`] (regla de huérfanos de Rust: trait y tipo en crates vecinos).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod conn;
pub mod handshake;
pub mod signal;
pub mod uds;

pub use conn::{Client, Conn, Server};
pub use handshake::{handshake_client, handshake_server, ClientHandshake, HelloExpect};
pub use signal::SignalChannel;
pub use uds::ensure_filesystem_path;

/// Deadline por defecto del handshake completo (2 s; spec 22 §5: el rt
/// hace exit(102) si el host no responde).
pub const HANDSHAKE_DEADLINE_MS: u32 = 2_000;

/// Backoffs de retry de connect (spec 06 §5: carrera contra el bind del
/// server recién lanzado): 5/20/100 ms, máx 6 intentos.
pub const CONNECT_BACKOFF_MS: [u64; 6] = [5, 20, 100, 100, 100, 100];
