//! `arca-protocol` — mensajes AIPC v1 (rkyv) + framing wire.
//!
//! Capa L0 · unsafe: **no**.
//! Contrato completo: `specs/arca-03-*.md` del blueprint; fuente de verdad del
//! enum: docs/04 §4.
//!
//! Reglas del crate (spec 03 §4):
//! - Wire-compat N-1: los golden fixtures de v1.x son inmutables; un campo
//!   nuevo = minor bump + fixture NUEVO (nunca editar el viejo).
//! - `decode` no aloca en exit-path: en caliente se usa `rkyv::Archived<T>`
//!   (zero-copy, validado con bytecheck).
//! - Todo mensaje de control acotado a [`MAX_CTL_PAYLOAD`] (enforced en
//!   encode y decode).
//! - CRC32 (IEEE) cubre header+payload con el campo crc a cero (spec 03 §3).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod framing;
pub mod wire;

pub use arca_types::ProtoVersion;

pub use framing::{
    decode, decode_signal, decode_signal_wire, encode_into, encode_signal_into, encode_signal_wire,
    MsgHeader, CHAN_CTL, CHAN_SIGNAL, FLAG_REPLY_REQUIRED, HEADER_LEN, MAGIC, MAX_CTL_PAYLOAD,
    MAX_FDS_PER_SEND, WIRE_VERSION,
};
pub use wire::{
    Attach, ControlMsg, FdKind, Hello, Insets, Ready, ScalePm, ShmLayout, ShutdownReason,
    SignalKind, SignalMsg, Size, SvcResult, SvcStatus, Theme, UiCaps, Welcome, WindowMode,
    WindowSpec,
};
