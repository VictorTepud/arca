//! Protocolo de gráficos de Arca.
//!
//! Capa L0 · unsafe: **no** (crate pura, sin dependencias ni alocaciones).
//!
//! # Estado (F3a — probe visual)
//!
//! Define el **layout del frame de framebuffer** que viaja en cada slot del
//! double-buffer seqlock de `arca-shm` ([`FrameHeader`]): una cabecera fija
//! de [`HDR_BYTES`] seguida del bitmap RGBA8888. El comentario de
//! `arca-shm::frame` («el layout lo define gfx-protocol») se cumple aquí.
//!
//! `MeshFrame`/atlas (rkyv, spec 04) sigue pendiente de F3b: cuando el
//! pipeline real (host-core → compositor) reemplace al probe, la cabecera
//! del payload será la de mesh; este header de framebuffer NO cambia (los
//! probes de dispositivo dependen de él y debe ser estable).
//!
//! # Layout del payload de un slot (little-endian, 32 bytes de cabecera)
//!
//! ```text
//! offset  tamaño  campo
//! 0       4       magic "AFRM"
//! 4       1       version (=1)
//! 5       1       formato de píxel (1 = RGBA8888, bytes R,G,B,A)
//! 6       1       reservado (=0)
//! 7       1       flags (bit 0 = frame opaco)
//! 8       2       ancho  (u16, píxeles)
//! 10      2       alto   (u16, píxeles)
//! 12      4       frame_seq (u32, contador del ESCRITOR, creciente)
//! 16      8       ts_ms (u64, CLOCK_MONOTONIC del escritor)
//! 24      8       reservado (=0)
//! ```
//!
//! El bitmap ocupa a continuación `ancho * alto * 4` bytes, fila superior
//! primero (top-down), sin padding (stride = ancho*4), píxel = R,G,B,A.
//!
//! NOTA(agent F3a): cabecera POD de formato fijo en vez de rkyv a propósito:
//! el lector del probe es Kotlin (`DemoActivity`) y decodificar 32 bytes
//! fijos a mano es más barato y auditable que arrastrar un serializador
//! completo al host de prueba. El versionado explícito (magic+versión)
//! mantiene la regla de compatibilidad de AGENTS §2.
//!
//! `ts`/`seq` son del escritor; la coherencia lectura/escritura la garantiza
//! el seqlock del slot (par = inválido), no estos campos.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

/// Magic con la que arranca todo frame válido: `b"AFRM"`.
pub const MAGIC: [u8; 4] = *b"AFRM";

/// Versión actual del layout de [`FrameHeader`].
pub const VERSION: u8 = 1;

/// Bytes de cabecera de frame (ver layout en la doc del crate).
pub const HDR_BYTES: usize = 32;

/// Flag de cabecera: el frame cubre toda la ventana y es opaco
/// (alfa = 255 en todos los píxeles: el host puede saltarse la mezcla).
pub const FLAG_OPAQUE: u8 = 1;

/// Formatos de píxel soportados por el framebuffer de F3a.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4 bytes por píxel en orden R, G, B, A (el que `Bitmap` de Android
    /// espera en `copyPixelsFromBuffer` para ARGB_8888).
    Rgba8888,
}

impl PixelFormat {
    /// Código de 1 byte que viaja en la cabecera.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            PixelFormat::Rgba8888 => 1,
        }
    }

    /// Bytes por píxel del formato.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgba8888 => 4,
        }
    }

    /// Decodifica el código de cabecera.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(PixelFormat::Rgba8888),
            _ => None,
        }
    }
}

/// Errores de decodificación de [`FrameHeader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbError {
    /// El slice es más corto que [`HDR_BYTES`].
    TooShort,
    /// Los 4 primeros bytes no son [`MAGIC`].
    BadMagic,
    /// `version` no es [`VERSION`] (layout desconocido: descartar frame).
    BadVersion,
    /// El código de formato no corresponde a un [`PixelFormat`] conocido.
    BadFormat,
    /// Ancho o alto en 0: un frame sin píxeles no es renderizable.
    BadGeometry,
}

impl core::fmt::Display for FbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            FbError::TooShort => "frame más corto que la cabecera",
            FbError::BadMagic => "magic de frame inválido (¿slot corrupto?)",
            FbError::BadVersion => "versión de layout de frame desconocida",
            FbError::BadFormat => "formato de píxel desconocido",
            FbError::BadGeometry => "frame con dimensión 0",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for FbError {}

/// Cabecera de un frame de framebuffer (campos ver layout en la doc del
/// crate). Encodable/decodable a bytes little-endian sin alocar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Ancho del bitmap en píxeles (≠ 0).
    pub width: u16,
    /// Alto del bitmap en píxeles (≠ 0).
    pub height: u16,
    /// Formato de píxel del bitmap que sigue a la cabecera.
    pub format: PixelFormat,
    /// Flags (bit 0 = opaco; ver [`FLAG_OPAQUE`]).
    pub flags: u8,
    /// Contador de frames del escritor (creciente; informativo para el
    /// host: la validez la marca el seqlock del slot).
    pub frame_seq: u32,
    /// Marca de tiempo del escritor en ms de `CLOCK_MONOTONIC`.
    pub ts_ms: u64,
}

impl FrameHeader {
    /// Cabecera nueva con formato [`PixelFormat::Rgba8888`] y flag opaco.
    #[must_use]
    pub const fn new_opaque(width: u16, height: u16, frame_seq: u32, ts_ms: u64) -> Self {
        Self {
            width,
            height,
            format: PixelFormat::Rgba8888,
            flags: FLAG_OPAQUE,
            frame_seq,
            ts_ms,
        }
    }

    /// Serializa a `out` (little-endian, sin alocar). Infallible: el tamaño
    /// lo exige el tipo del argumento.
    pub fn encode_into(&self, out: &mut [u8; HDR_BYTES]) {
        out[..4].copy_from_slice(&MAGIC);
        out[4] = VERSION;
        out[5] = self.format.code();
        out[6] = 0;
        out[7] = self.flags;
        out[8..10].copy_from_slice(&self.width.to_le_bytes());
        out[10..12].copy_from_slice(&self.height.to_le_bytes());
        out[12..16].copy_from_slice(&self.frame_seq.to_le_bytes());
        out[16..24].copy_from_slice(&self.ts_ms.to_le_bytes());
        out[24..32].fill(0);
    }

    /// Parsea una cabecera de `bytes` (little-endian). Solo acepta el layout
    /// de [`VERSION`] y geometría no nula: ante la duda, el lector descarta
    /// el frame entero (fail-closed, como el resto del protocolo).
    pub fn decode_from(bytes: &[u8]) -> Result<Self, FbError> {
        if bytes.len() < HDR_BYTES {
            return Err(FbError::TooShort);
        }
        if bytes[..4] != MAGIC {
            return Err(FbError::BadMagic);
        }
        if bytes[4] != VERSION {
            return Err(FbError::BadVersion);
        }
        let Some(format) = PixelFormat::from_code(bytes[5]) else {
            return Err(FbError::BadFormat);
        };
        let width = u16::from_le_bytes([bytes[8], bytes[9]]);
        let height = u16::from_le_bytes([bytes[10], bytes[11]]);
        if width == 0 || height == 0 {
            return Err(FbError::BadGeometry);
        }
        Ok(Self {
            width,
            height,
            format,
            flags: bytes[7],
            frame_seq: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            ts_ms: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
        })
    }

    /// Bytes de payload de un slot para esta geometría:
    /// cabecera + `w*h*bytes_por_píxel`. Es el `frame_bytes` que dimensiona
    /// la región completa (`arca_shm::frame::region_len`).
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        HDR_BYTES + self.width as usize * self.height as usize * self.format.bytes_per_pixel()
    }
}

/// Bytes de payload de slot para `w×h` RGBA8888 (atajo para el HOST, que
/// dimensiona el archivo antes de que exista escritor).
#[must_use]
pub const fn rgba_frame_bytes(width: u16, height: u16) -> usize {
    HDR_BYTES + width as usize * height as usize * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ejemplo() -> FrameHeader {
        FrameHeader::new_opaque(540, 1170, 42, 139_333_571)
    }

    #[test]
    fn roundtrip_conserva_todos_los_campos() {
        let hdr = ejemplo();
        let mut buf = [0u8; HDR_BYTES];
        hdr.encode_into(&mut buf);
        let vuelta = FrameHeader::decode_from(&buf).expect("cabecera válida");
        assert_eq!(hdr, vuelta);
    }

    #[test]
    fn layout_es_estable_byte_a_byte() {
        // Golden del layout (F3a): si esto cambia, es un BREAK del contrato
        // con el host Kotlin y exige bump de VERSION + compat (AGENTS §2).
        let mut buf = [0u8; HDR_BYTES];
        FrameHeader::new_opaque(0x0201, 0x0403, 0x07060504, 0x1514131211100908)
            .encode_into(&mut buf);
        assert_eq!(
            &buf,
            &[
                b'A', b'F', b'R', b'M', // magic
                1, 1, 0, 1, // version, formato RGBA, reservado, opaco
                0x01, 0x02, // ancho LE
                0x03, 0x04, // alto LE
                0x04, 0x05, 0x06, 0x07, // frame_seq LE
                0x08, 0x09, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, // ts_ms LE
                0, 0, 0, 0, 0, 0, 0, 0, // reservado
            ]
        );
    }

    #[test]
    fn rechaza_slice_corto() {
        let mut buf = [0u8; HDR_BYTES];
        ejemplo().encode_into(&mut buf);
        assert_eq!(FrameHeader::decode_from(&buf[..31]), Err(FbError::TooShort));
    }

    #[test]
    fn rechaza_magic_roto() {
        let mut buf = [0u8; HDR_BYTES];
        ejemplo().encode_into(&mut buf);
        buf[0] = b'X';
        assert_eq!(FrameHeader::decode_from(&buf), Err(FbError::BadMagic));
    }

    #[test]
    fn rechaza_version_futura() {
        let mut buf = [0u8; HDR_BYTES];
        ejemplo().encode_into(&mut buf);
        buf[4] = 2;
        assert_eq!(FrameHeader::decode_from(&buf), Err(FbError::BadVersion));
    }

    #[test]
    fn rechaza_formato_desconocido() {
        let mut buf = [0u8; HDR_BYTES];
        ejemplo().encode_into(&mut buf);
        buf[5] = 9;
        assert_eq!(FrameHeader::decode_from(&buf), Err(FbError::BadFormat));
    }

    #[test]
    fn rechaza_geometria_nula() {
        let mut buf = [0u8; HDR_BYTES];
        FrameHeader::new_opaque(0, 100, 1, 1).encode_into(&mut buf);
        assert_eq!(FrameHeader::decode_from(&buf), Err(FbError::BadGeometry));
    }

    #[test]
    fn frame_bytes_cuenta_cabecera_mas_bitmap() {
        assert_eq!(ejemplo().frame_bytes(), 32 + 540 * 1170 * 4);
        assert_eq!(rgba_frame_bytes(320, 180), 32 + 320 * 180 * 4);
    }

    #[test]
    fn error_display_es_entendible() {
        assert_eq!(
            FbError::BadMagic.to_string(),
            "magic de frame inválido (¿slot corrupto?)"
        );
    }
}
