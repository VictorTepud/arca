//! Codec hex privado (solo lo que este crate necesita: sha256 de artefactos).
//!
//! NOTA(agent): la spec 02 §2 no lista la crate `hex` entre las dependencias
//! permitidas y `arca-types` no la re-exporta, así que se implementa el
//! subconjunto mínimo (64 chars ⇄ `[u8; 32]`) en lugar de añadir la
//! dependencia. Sin `unsafe`, sin pánicos, totalmente verificable.

/// Fallo de decodificación hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hex32Error {
    /// Longitud incorrecta (≠ 64 caracteres).
    Length,
    /// Caracteres fuera del alfabeto `[0-9a-fA-F]`.
    Charset,
}

impl Hex32Error {
    /// Razón legible (para `PkgError::BadSha256`).
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Length => "se esperaban exactamente 64 caracteres hex",
            Self::Charset => "caracteres fuera del alfabeto [0-9a-fA-F]",
        }
    }
}

/// Alfabeto hex minúsculas (salida canónica).
const HEX_CHARS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Decodifica 64 caracteres hex (mayúsculas o minúsculas) en 32 bytes.
pub(crate) fn decode32(s: &str) -> Result<[u8; 32], Hex32Error> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return Err(Hex32Error::Length);
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = hex_val(pair[0]).ok_or(Hex32Error::Charset)?;
        let lo = hex_val(pair[1]).ok_or(Hex32Error::Charset)?;
        out[i] = hi << 4 | lo;
    }
    Ok(out)
}

/// Codifica 32 bytes como 64 caracteres hex minúsculas.
pub(crate) fn encode32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b.iter() {
        s.push(HEX_CHARS[usize::from(byte >> 4)]);
        s.push(HEX_CHARS[usize::from(byte & 0x0f)]);
    }
    s
}

/// Valor numérico de un dígito hex ASCII.
const fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_32_bytes() {
        let b = [
            0x00, 0x01, 0x0f, 0xff, 0xab, 0xcd, 0xef, 0x99, 0x12, 0x34, 0x56, 0x78, 0x90, 0xfe,
            0xdc, 0xba, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
            0xdd, 0xee, 0x0a, 0x0b,
        ];
        let s = encode32(&b);
        assert_eq!(s.len(), 64);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let back = decode32(&s);
        assert!(matches!(back, Ok(x) if x == b));
    }

    #[test]
    fn acepta_mayusculas() {
        let lower = "0123456789abcdef".repeat(4);
        let upper = "0123456789ABCDEF".repeat(4);
        assert!(decode32(&lower).is_ok());
        assert!(decode32(&upper).is_ok());
        assert!(matches!((decode32(&lower), decode32(&upper)), (Ok(a), Ok(b)) if a == b));
    }

    #[test]
    fn rechaza_longitud_y_charset() {
        assert!(matches!(decode32(""), Err(Hex32Error::Length)));
        assert!(matches!(decode32("ab"), Err(Hex32Error::Length)));
        assert!(matches!(decode32(&"a".repeat(63)), Err(Hex32Error::Length)));
        assert!(matches!(
            decode32(&"z".repeat(64)),
            Err(Hex32Error::Charset)
        ));
        assert!(matches!(
            decode32(&format!("{}g", "0".repeat(63))),
            Err(Hex32Error::Charset)
        ));
    }
}
