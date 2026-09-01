//! Digest blake3-256 canónico + hex (spec 01 §3).
//!
//! SIEMPRE importar [`Digest`] de aquí (spec 01 §5 fila 3: duplicar el tipo
//! en otro crate es bug). Hex en minúsculas, 64 chars.

use crate::error::ArcaError;

/// Digest blake3 de 32 bytes, en su forma canónica.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// Digest de bytes en memoria.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// Digest streaming (archivos grandes sin cargar en RAM).
    pub fn of_reader(r: &mut dyn std::io::Read) -> std::io::Result<Self> {
        let mut h = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(Self::from_hasher(&h))
    }

    /// Digest desde un hasher ya alimentado (p. ej. verify-while-extract).
    #[must_use]
    pub fn from_hasher(h: &blake3::Hasher) -> Self {
        Self(*h.finalize().as_bytes())
    }

    /// Parse de 64 hex chars (minúsculas o mayúsculas; emite minúsculas).
    pub fn from_hex(s: &str) -> Result<Self, ArcaError> {
        let bad = || ArcaError::Internal("Digest.from_hex: 64 hex chars esperadas");
        if s.len() != 64 {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s.as_bytes(), &mut out).map_err(|_| bad())?;
        Ok(Self(out))
    }

    /// Hex minúsculas (64 chars).
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Bytes crudos.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// ¿Es el digest nulo (todas cero)? Reservado para "sin digest aún".
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        let Digest(d) = *self;
        let mut i = 0;
        while i < 32 {
            if d[i] != 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Digest nulo.
    pub const ZERO: Digest = Digest([0u8; 32]);
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Solo 8 bytes: logs compactos; el hex completo va al campo detallado.
        write!(f, "b3:{}", hex::encode(&self.0[..4]))
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_idempotente() {
        let d = Digest::of(b"payload de arca");
        let h1 = d.to_hex();
        let h2 = Digest::from_hex(&h1).map(|x| x.to_hex()).ok();
        assert_eq!(h2.as_deref(), Some(h1.as_str()));
        assert_eq!(h1.len(), 64);
        assert!(h1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn from_hex_rechaza_basura() {
        let bad: Vec<String> = [
            "".to_string(),
            "abcd".to_string(),
            "z".repeat(64),
            "0".repeat(63),
        ]
        .to_vec();
        for s in &bad {
            assert!(Digest::from_hex(s).is_err(), "{s:?}");
        }
    }

    #[test]
    fn streaming_igual_a_of() {
        let data = vec![7u8; 150_000]; // > 1 buffer de 64 KiB
        let a = Digest::of(&data);
        let mut r: &[u8] = &data;
        let b = Digest::of_reader(&mut r).ok();
        assert_eq!(b, Some(a));
    }

    #[test]
    fn zero_y_debug() {
        assert!(Digest::ZERO.is_zero());
        assert!(!Digest::of(b"x").is_zero());
        assert!(format!("{:?}", Digest::ZERO).starts_with("b3:"));
    }
}
