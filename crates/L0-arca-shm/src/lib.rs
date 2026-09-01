//! `arca-shm` — memoria compartida y sincronización lock-free.
//!
//! Capa L0 · unsafe: **sí (unsafe-heavy)** — todo bloque unsafe lleva
//! comentario de invariante (AGENTS §2). Contrato: `specs/arca-05-*.md`.
//!
//! Piezas (docs/04 §6-§7):
//! - [`Memfd`]: crear/compartir regiones anónimas (nix `memfd_create`,
//!   falla a `syscall()` en Android).
//! - [`ShmMap`]: mmap RAII (drop = munmap).
//! - [`RingSpsc`]: ring H→C de input (single-producer single-consumer,
//!   Release/Acquire, sin futex → tolerante al freezer de Android).
//! - [`FrameSlots`]: double-buffer C→H de frames con seqlock
//!   (seq par = escribiendo/inválido, seq impar = válido).
//!
//! Invariante global de diseño (docs/07): **prohibido futex/mutex en shm**.
//! Si un lado está congelado (cgroup freezer), el otro nunca se bloquea:
//! por eso seqlock/ring, nunca locks de kernel.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod frame;
pub mod map;
pub mod memfd;
pub mod ring;

#[cfg(test)]
mod tests;

pub use frame::{region_len, FrameSlots, FrameSnap, WriteSlot};
pub use map::ShmMap;
pub use memfd::Memfd;
pub use ring::{PushResult, RingSpsc};

/// Tamaño de un slot de input (docs/04 §6): 64 B = una línea de caché.
pub const INPUT_SLOT_BYTES: usize = 64;

/// Slots por defecto del ring de input (256 ≈ 4 s de eventos a 60 Hz).
pub const INPUT_SLOTS: usize = 256;

/// Bytes máximos por frame (docs/04 §7): 4 MiB por slot.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
