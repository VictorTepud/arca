//! arca-ipc (L0): protocolo AIPC mínimo de F0-F1.
//!
//! Trama sobre el socket (little-endian):
//! ```text
//! [u32 len][u8 tag][payload]      (len = 1 + payload.len())
//! ```
//!
//! Mensajes de F0:
//! | tag | dirección | payload |
//! |-----|-----------|---------|
//! | `TAG_PING` | host→app | `[u64 nonce]` |
//! | `TAG_SHUTDOWN` | host→app | `[u8 razon]` |
//! | `TAG_HELLO` | app→host | nombre de la app (utf-8) |
//! | `TAG_PONG` | app→host | `[u64 nonce]` (eco) |
//!
//! En F2 esto evoluciona a rkyv + memfd + SO_PEERCRED (ver blueprint);
//! esta es la versión mínima que necesita el motor nativo para demostrarse.

/// Ping del supervisor hacia la sub-app: `payload = [u64 nonce]`.
pub const TAG_PING: u8 = 1;
/// Orden de apagado: `payload = [u8 razon]` (1 = User, 2 = Sistema).
pub const TAG_SHUTDOWN: u8 = 2;
/// Presentación de la sub-app: `payload = nombre de la app (utf-8)`.
pub const TAG_HELLO: u8 = 3;
/// Respuesta al ping: `payload = [u64 nonce]` (el mismo que llegó).
pub const TAG_PONG: u8 = 4;

/// Razón de apagado: la pidió el usuario.
pub const RAZON_USER: u8 = 1;
/// Razón de apagado: decisión del sistema (p.ej. memoria baja).
pub const RAZON_SISTEMA: u8 = 2;

/// Tamaño máximo aceptado de una trama (1 MiB): suficiente para F0
/// y a la vez loco suficiente para detectar protocolos rotos.
pub const LIMITE_TRAMA: usize = 1 << 20;

/// Envía una trama. `payload` es opcional (`&[]`).
pub fn enviar<W: std::io::Write>(w: &mut W, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = (1 + payload.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 1 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.push(tag);
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

/// Recibe una trama completa. Devuelve `(tag, payload)`.
///
/// Nota: cuando el par cierra el socket, el `read_exact` inicial devuelve
/// `UnexpectedEof` — esa es la señal de "canal cerrado" que usan ambos lados.
pub fn recibir<R: std::io::Read>(r: &mut R) -> std::io::Result<(u8, Vec<u8>)> {
    let mut lenb = [0u8; 4];
    r.read_exact(&mut lenb)?;
    let len = u32::from_le_bytes(lenb) as usize;
    if len == 0 || len > LIMITE_TRAMA {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "trama AIPC de longitud inválida",
        ));
    }
    let mut trama = vec![0u8; len];
    r.read_exact(&mut trama)?;
    Ok((trama[0], trama[1..].to_vec()))
}

/// Nombre legible de una razón de apagado (para logs).
pub fn nombre_razon(razon: u8) -> &'static str {
    match razon {
        RAZON_USER => "User",
        RAZON_SISTEMA => "Sistema",
        _ => "Desconocida",
    }
}
