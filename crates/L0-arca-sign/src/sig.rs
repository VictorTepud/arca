//! Sobre de firma `meta/signature.bin` (docs/06 §2/§5).
//!
//! Formato v1 (106 bytes, determinista):
//!
//! ```text
//! [magic "ARCASIG1" 8B][versión u16 LE][key_id blake3 32B][firma ed25519 64B]
//! ```
//!
//! `key_id` = blake3 de la pubkey (diagnóstico: el panel del host muestra
//! QUÉ clave firmó sin exponer bytes crudos). La verificación real usa el
//! anillo completo; `key_id` es informativo y NO se confía en él para aceptar.

use arca_types::{ArcaError, Digest, Res};

/// Magic del sobre de firma.
pub const SIG_MAGIC: [u8; 8] = *b"ARCASIG1";
/// Versión actual del sobre.
pub const SIG_VERSION: u16 = 1;
/// Tamaño exacto del sobre serializado.
pub const SIG_LEN: usize = 8 + 2 + 32 + 64;

/// Firma detached de un paquete `.arca` (contenido de `meta/signature.bin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSignature {
    /// Identidad de la clave firmante (informativo).
    pub key_id: Digest,
    /// Firma ed25519 sobre el digest canónico del paquete.
    pub sig: [u8; 64],
}

impl PackageSignature {
    /// Firma un digest con la clave privada (lado tools/Deepin).
    #[cfg(feature = "signer")]
    #[must_use]
    pub fn sign(digest: &Digest, key: &crate::signer::SecretKey) -> Self {
        Self {
            key_id: crate::ring::key_id(&key.verifying_key()),
            sig: crate::signer::sign_digest(digest, key),
        }
    }

    /// Serializa al formato binario.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIG_LEN] {
        let mut out = [0u8; SIG_LEN];
        out[..8].copy_from_slice(&SIG_MAGIC);
        out[8..10].copy_from_slice(&SIG_VERSION.to_le_bytes());
        out[10..42].copy_from_slice(self.key_id.as_bytes());
        out[42..106].copy_from_slice(&self.sig);
        out
    }

    /// Parse (longitud y magic exactos; versión futura = error, no guess).
    pub fn parse(b: &[u8]) -> Res<Self> {
        if b.len() != SIG_LEN {
            return Err(ArcaError::InvalidPackage(
                "signature.bin: longitud incorrecta",
            ));
        }
        if b[..8] != SIG_MAGIC {
            return Err(ArcaError::InvalidPackage("signature.bin: magic incorrecto"));
        }
        let ver = u16::from_le_bytes([b[8], b[9]]);
        if ver != SIG_VERSION {
            return Err(ArcaError::InvalidPackage(
                "signature.bin: versión no soportada",
            ));
        }
        let mut key_id = [0u8; 32];
        key_id.copy_from_slice(&b[10..42]);
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&b[42..106]);
        Ok(Self {
            key_id: Digest(key_id),
            sig,
        })
    }

    /// Bytes de la firma ed25519 (para `RingOfTrust::verify`).
    #[must_use]
    pub const fn sig_bytes(&self) -> &[u8; 64] {
        &self.sig
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_len_tipos_basura_rechazada() {
        let buena = PackageSignature {
            key_id: Digest::of(b"k"),
            sig: [7u8; 64],
        };
        let bytes = buena.to_bytes();
        assert!(PackageSignature::parse(&bytes).is_ok());
        // truncado
        assert!(PackageSignature::parse(&bytes[..105]).is_err());
        // extendido
        assert!(PackageSignature::parse(&[bytes.as_ref(), &[0u8]].concat()).is_err());
        // magic mutado
        let mut malo = bytes;
        malo[0] ^= 1;
        assert!(PackageSignature::parse(&malo).is_err());
        // versión futura
        let mut ver = bytes;
        ver[9] = 2;
        assert!(PackageSignature::parse(&ver).is_err());
    }

    #[test]
    #[cfg(feature = "signer")]
    fn roundtrip_sign_parse() {
        let sk = match crate::signer::keygen() {
            Ok(k) => k,
            Err(_) => return,
        };
        let d = Digest::of(b"paquete");
        let ps = PackageSignature::sign(&d, &sk);
        let back = PackageSignature::parse(&ps.to_bytes());
        assert!(matches!(back, Ok(x) if x == ps));
    }
}
