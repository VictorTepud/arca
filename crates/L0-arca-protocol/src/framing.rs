//! Framing wire AIPC v1 (docs/04 §5) + codificación eventfd de señales.
//!
//! ```text
//! Frame AIPC (socket):
//!   0  magic    u32 = 0x41CA1C00   ('ARCA IPC')
//!   4  version  u16 = 1            (versión del FRAMING, no del protocolo)
//!   6  flags    u16                (bit1: respuesta requerida)
//!   8  channel  u8                 (0 ctl, 1 señal)
//!   9  rsvd     u8 = 0
//!  10 length   u32                 (bytes de payload)
//!  14 seq      u64                 (monótono por dirección; lo pone el emisor)
//!  22 crc32    u32                 (header+payload con este campo a cero)
//!  26 payload  rkyv(ControlMsg | SignalMsg)   [unaligned]
//! ```
//!
//! El CRC se calcula sobre los bytes `0..22` y `26..fin` (campo crc excluido,
//! equivalente a calcularlo con el campo a cero — evita copiar el frame).

use crate::wire::{ControlMsg, SignalKind, SignalMsg};
use arca_types::{ArcaError, Res};

/// Magic del framing (`'ARCA IPC'`). Valor de spec 03 §3 (el `0x41CA1PC0`
/// de docs/04 §5 no es literal hexadecimal válida; spec manda).
pub const MAGIC: u32 = 0x41CA1C00;
/// Versión del framing (independiente de ProtoVersion del handshake).
pub const WIRE_VERSION: u16 = 1;
/// Bytes de header (fijo; docs/04 §5).
pub const HEADER_LEN: usize = 26;
/// Máximo payload de un mensaje de control (64 KiB; spec 03 §4).
pub const MAX_CTL_PAYLOAD: usize = 64 * 1024;
/// Máximo de fds por sendmsg (defensa anti-DoS de ancillary; spec 06 §4).
pub const MAX_FDS_PER_SEND: usize = 8;
/// Canal de control.
pub const CHAN_CTL: u8 = 0;
/// Canal de señal (socket).
pub const CHAN_SIGNAL: u8 = 1;
/// Flag bit1: el emisor exige respuesta (futuro req/rep).
pub const FLAG_REPLY_REQUIRED: u16 = 0x0002;

/// Header parseado y validado de una trama AIPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgHeader {
    /// Magic (ya validado == MAGIC).
    pub magic: u32,
    /// Versión de framing (validada == WIRE_VERSION en v1).
    pub version: u16,
    /// Flags (bit1 = respuesta requerida).
    pub flags: u16,
    /// Canal (0 ctl, 1 señal).
    pub channel: u8,
    /// Bytes de payload.
    pub length: u32,
    /// Seq monótono del emisor.
    pub seq: u64,
    /// CRC32 IEEE del header+payload (campo a cero).
    pub crc32: u32,
}

impl MsgHeader {
    /// Serializa el header a bytes (little-endian, docs/04 §5).
    #[must_use]
    fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4..6].copy_from_slice(&self.version.to_le_bytes());
        b[6..8].copy_from_slice(&self.flags.to_le_bytes());
        b[8] = self.channel;
        b[9] = 0; // rsvd
        b[10..14].copy_from_slice(&self.length.to_le_bytes());
        b[14..22].copy_from_slice(&self.seq.to_le_bytes());
        b[22..26].copy_from_slice(&self.crc32.to_le_bytes());
        b
    }

    /// Parsea y valida magic/versión/longitud (sin CRC ni rkyv).
    pub fn parse(buf: &[u8]) -> Res<Self> {
        if buf.len() < HEADER_LEN {
            return Err(ArcaError::InvalidFrame("trama más corta que header"));
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != MAGIC {
            return Err(ArcaError::InvalidFrame("magic inválido"));
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != WIRE_VERSION {
            return Err(ArcaError::InvalidFrame("versión de framing no soportada"));
        }
        let length = u32::from_le_bytes([buf[10], buf[11], buf[12], buf[13]]);
        if length as usize > MAX_CTL_PAYLOAD {
            return Err(ArcaError::FrameOverflow {
                bytes: length as usize,
                limit: MAX_CTL_PAYLOAD,
            });
        }
        Ok(Self {
            magic,
            version,
            flags: u16::from_le_bytes([buf[6], buf[7]]),
            channel: buf[8],
            length,
            seq: u64::from_le_bytes([
                buf[14], buf[15], buf[16], buf[17], buf[18], buf[19], buf[20], buf[21],
            ]),
            crc32: u32::from_le_bytes([buf[22], buf[23], buf[24], buf[25]]),
        })
    }
}

/// Serializa `msg` como trama de control completa y la **añade** a `out`.
///
/// `seq` lo asigna el emisor (monótono por dirección — docs/04 §4).
/// Error tipado si el payload excede [`MAX_CTL_PAYLOAD`] (nunca truncar).
pub fn encode_into(msg: &ControlMsg, seq: u64, out: &mut Vec<u8>) -> Res<()> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(msg)
        .map_err(|_| ArcaError::Internal("aipc: rkyv no pudo serializar ControlMsg"))?;
    if payload.len() > MAX_CTL_PAYLOAD {
        return Err(ArcaError::FrameOverflow {
            bytes: payload.len(),
            limit: MAX_CTL_PAYLOAD,
        });
    }
    let hdr = MsgHeader {
        magic: MAGIC,
        version: WIRE_VERSION,
        flags: 0,
        channel: CHAN_CTL,
        length: payload.len() as u32,
        seq,
        crc32: 0, // se parcha abajo
    };
    let start = out.len();
    out.extend_from_slice(&hdr.to_bytes());
    out.extend_from_slice(&payload);
    // CRC sobre 0..22 y 26..fin (campo crc excluido — ver módulo).
    let mut h = crc32fast::Hasher::new();
    h.update(&out[start..start + 22]);
    h.update(&out[start + 26..]);
    let crc = h.finalize();
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Serializa `msg` como trama del canal de señal (socket).
pub fn encode_signal_into(msg: &SignalMsg, seq: u64, out: &mut Vec<u8>) -> Res<()> {
    let payload = rkyv::to_bytes::<rkyv::rancor::Error>(msg)
        .map_err(|_| ArcaError::Internal("aipc: rkyv no pudo serializar SignalMsg"))?;
    let hdr = MsgHeader {
        magic: MAGIC,
        version: WIRE_VERSION,
        flags: 0,
        channel: CHAN_SIGNAL,
        length: payload.len() as u32,
        seq,
        crc32: 0,
    };
    let start = out.len();
    out.extend_from_slice(&hdr.to_bytes());
    out.extend_from_slice(&payload);
    let mut h = crc32fast::Hasher::new();
    h.update(&out[start..start + 22]);
    h.update(&out[start + 26..]);
    let crc = h.finalize();
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Decodifica una trama de control completa: valida magic/versión/longitud,
/// CRC y estructura rkyv (bytecheck). Zero-alloc en exit-path: devuelve el
/// `Archived<ControlMsg>` prestado del buffer.
///
/// `buf` debe contener EXACTAMENTE la trama (header+payload) — el recorte
/// por longitud lo hace el transporte (arca-ipc).
pub fn decode(buf: &[u8]) -> Res<(MsgHeader, &'_ rkyv::Archived<ControlMsg>)> {
    let (hdr, payload) = decode_common(buf, CHAN_CTL)?;
    let archived = rkyv::access::<rkyv::Archived<ControlMsg>, rkyv::rancor::Error>(payload)
        .map_err(|_| ArcaError::InvalidFrame("payload ctl: rkyv inválido"))?;
    Ok((hdr, archived))
}

/// Decodifica una trama del canal de señal (socket).
pub fn decode_signal(buf: &[u8]) -> Res<(MsgHeader, &'_ rkyv::Archived<SignalMsg>)> {
    let (hdr, payload) = decode_common(buf, CHAN_SIGNAL)?;
    let archived = rkyv::access::<rkyv::Archived<SignalMsg>, rkyv::rancor::Error>(payload)
        .map_err(|_| ArcaError::InvalidFrame("payload señal: rkyv inválido"))?;
    Ok((hdr, archived))
}

/// Parse + CRC + split del payload para un canal dado.
fn decode_common(buf: &[u8], expected_chan: u8) -> Res<(MsgHeader, &[u8])> {
    let hdr = MsgHeader::parse(buf)?;
    if hdr.channel != expected_chan {
        return Err(ArcaError::InvalidFrame("canal no coincide con el decoder"));
    }
    let total = HEADER_LEN + hdr.length as usize;
    if buf.len() < total {
        return Err(ArcaError::InvalidFrame("trama truncada respecto de length"));
    }
    if buf.len() > total {
        return Err(ArcaError::InvalidFrame("bytes sobrantes tras la trama"));
    }
    // CRC sobre 0..22 y 26..total (campo crc excluido).
    let mut h = crc32fast::Hasher::new();
    h.update(&buf[..22]);
    h.update(&buf[26..total]);
    if h.finalize() != hdr.crc32 {
        return Err(ArcaError::InvalidFrame("crc32 no coincide"));
    }
    Ok((hdr, &buf[26..total]))
}

// ── Codificación eventfd de señales (path caliente; docs/04 §4) ──────────

/// Serializa una señal como u64 taggeado para eventfd:
/// `(kind_u8 << 56) | (payload_u56)`. Determinista, sin allocs.
#[must_use]
pub fn encode_signal_wire(s: &SignalMsg) -> u64 {
    let (kind, payload) = match s {
        SignalMsg::FrameReady { frame_seq } => (SignalKind::FrameReady, *frame_seq),
        SignalMsg::FrameTick { t_ns } => (SignalKind::FrameTick, *t_ns),
        SignalMsg::Busy => (SignalKind::Busy, 0),
        SignalMsg::Idle => (SignalKind::Idle, 0),
        SignalMsg::Pong { t_ns } => (SignalKind::Pong, *t_ns),
    };
    ((kind.to_byte() as u64) << 56) | (payload & 0x00ff_ffff_ffff_ffff)
}

/// Inverso de [`encode_signal_wire`]. Error tipado si el tag es desconocido
/// (un eventfd corrupto nunca debe pánico — spec 03 §5).
pub fn decode_signal_wire(v: u64) -> Res<SignalMsg> {
    let kind = SignalKind::from_byte((v >> 56) as u8)
        .ok_or(ArcaError::InvalidFrame("tag de señal desconocido"))?;
    let payload = v & 0x00ff_ffff_ffff_ffff;
    Ok(match kind {
        SignalKind::FrameReady => SignalMsg::FrameReady { frame_seq: payload },
        SignalKind::FrameTick => SignalMsg::FrameTick { t_ns: payload },
        SignalKind::Busy => SignalMsg::Busy,
        SignalKind::Idle => SignalMsg::Idle,
        SignalKind::Pong => SignalMsg::Pong { t_ns: payload },
    })
}
