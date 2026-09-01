//! Archivo de firma del paquete: [`PackageSignature`] (`ARCA-SIG1`).
//!
//! Es el contenido de `meta/signature.bin` dentro del `.arca` (docs/06 §2) y,
//! para las tools, también un `.sig` detached junto al paquete (mismo formato
//! exacto). La spec 08/docs 06 NO fijan la estructura interna del archivo
//! (solo "firma ed25519 del digest"), así que este módulo la define como
//! propuesta v1 (decision documentada en bitácora T06 + README):
//!
//! ```text
//! ARCA-SIG1 (106 bytes, exactos):
//!   offset 0   magic    "ARCA-SIG1"     9 B   (constante [`SIG_MAGIC`])
//!   offset 9   version  u8              1 B   = 1 (constante [`SIG_VERSION`])
//!   offset 10  key_id   [u8; 32]       32 B   blake3-256 de la pubkey (ver [`key_id`])
//!   offset 42  sig      [u8; 64]       64 B   ed25519 sobre el package digest
//! ```
//!
//! # Semántica de `key_id`
//!
//! - Se calcula como `blake3_256(VerifyingKey::to_bytes())` ([`key_id`]).
//! - **NO está cubierto por la firma** (la firma cubre solo el digest, spec 08):
//!   es una pista de diagnóstico para saber qué clave firmó (rotación, panel
//!   del host) y para busquedas en el anillo. La CONFIANZA emana 100 % del
//!   anillo: [`PackageSignature::verify`] exige que la firma verifique contra
//!   alguna clave de [`RingOfTrust`], sea cual sea el `key_id` declarado.
//!
//! # Ejemplo
//!
//! ```no_run
//! # use arca_sign::{PackageSignature, RingOfTrust};
//! # use arca_types::Digest;
//! # fn main() -> Result<(), arca_types::ArcaError> {
//! // meta/signature.bin extraído del .arca (o .sig detached de las tools):
//! let pkg_sig = PackageSignature::read_file(std::path::Path::new("meta/signature.bin"))?;
//! let digest = Digest::of(b"...package digest canonico (32 B)...");
//! let ring = RingOfTrust::from_embedded();
//! pkg_sig.verify(&digest, &ring)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Errores → diagnóstico
//!
//! | Síntoma | Causa | Fix |
//! |---|---|---|
//! | `Io(InvalidData)` al parsear | magic/versión/longitud distintos | el archivo no es un ARCA-SIG1 v1 |
//! | `InvalidSignature` en `verify` | digest distinto o clave fuera del anillo | re-empaquetar/re-firmar; ring desactualizado (ADR-012) |

use std::path::Path;

use arca_types::{ArcaError, Digest, Res};
use ed25519_dalek::VerifyingKey;

use crate::ring::RingOfTrust;

/// Magic del archivo de firma v1.
pub const SIG_MAGIC: &[u8; 9] = b"ARCA-SIG1";
/// Versión del formato del archivo de firma.
pub const SIG_VERSION: u8 = 1;
/// Tamaño exacto del archivo de firma v1: magic + versión + key_id + sig.
pub const SIG_FILE_BYTES: usize = SIG_MAGIC.len() + 1 + 32 + 64;

/// Firma detached de un paquete `.arca` (formato ARCA-SIG1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageSignature {
    /// Identificador de la clave firmante (blake3-256 de la pubkey). Pista de
    /// diagnóstico: NO cubierto por la firma.
    key_id: Digest,
    /// Firma ed25519 sobre el package digest (32 B) del paquete.
    sig: [u8; 64],
}

/// Identificador de una clave pública: `blake3_256(pubkey_bytes)`.
///
/// Determinista y barato; se usa en el archivo de firma y en diagnósticos
/// del anillo.
#[must_use]
pub fn key_id(vk: &VerifyingKey) -> Digest {
    Digest::of(vk.as_bytes())
}

impl PackageSignature {
    /// Construye desde partes ya validadas (tools/tests).
    #[must_use]
    pub const fn from_parts(key_id: Digest, sig: [u8; 64]) -> Self {
        Self { key_id, sig }
    }

    /// Parsea un ARCA-SIG1 completo (ver layout arriba). Estricto en
    /// magic, versión y longitud.
    ///
    /// # Errors
    /// - [`ArcaError::Io`] (`InvalidData`) si magic/versión/longitud no son v1.
    pub fn parse(bytes: &[u8]) -> Res<Self> {
        let bad = |msg: &'static str| {
            ArcaError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, msg))
        };
        if bytes.len() != SIG_FILE_BYTES {
            return Err(bad("signature: longitud != ARCA-SIG1 (106 bytes)"));
        }
        if &bytes[..SIG_MAGIC.len()] != SIG_MAGIC {
            return Err(bad("signature: magic != ARCA-SIG1"));
        }
        if bytes[SIG_MAGIC.len()] != SIG_VERSION {
            return Err(bad("signature: versión no soportada"));
        }
        let mut key_id = [0u8; 32];
        let mut sig = [0u8; 64];
        key_id.copy_from_slice(&bytes[10..42]);
        sig.copy_from_slice(&bytes[42..106]);
        Ok(Self {
            key_id: Digest(key_id),
            sig,
        })
    }

    /// Serializa a los 106 bytes exactos del formato v1 (determinista).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIG_FILE_BYTES] {
        let mut out = [0u8; SIG_FILE_BYTES];
        out[..SIG_MAGIC.len()].copy_from_slice(SIG_MAGIC);
        out[SIG_MAGIC.len()] = SIG_VERSION;
        out[10..42].copy_from_slice(&self.key_id.0);
        out[42..106].copy_from_slice(&self.sig);
        out
    }

    /// Lee y parsea el archivo de firma (`meta/signature.bin` extraído o
    /// `.sig` detached de las tools).
    ///
    /// # Errors
    /// - [`ArcaError::Io`] si falla la lectura o el parseo.
    pub fn read_file(path: &Path) -> Res<Self> {
        Self::parse(&std::fs::read(path)?)
    }

    /// Escribe el archivo de firma (datos públicos, permisos por defecto).
    ///
    /// # Errors
    /// - [`ArcaError::Io`] si falla la escritura.
    pub fn write_file(&self, path: &Path) -> Res<()> {
        std::fs::write(path, self.to_bytes())?;
        Ok(())
    }

    /// `key_id` declarado (pista, no raíz de confianza — ver módulo).
    #[must_use]
    pub const fn key_id(&self) -> &Digest {
        &self.key_id
    }

    /// Firma ed25519 cruda (64 B) sobre el package digest.
    #[must_use]
    pub const fn sig(&self) -> &[u8; 64] {
        &self.sig
    }

    /// ¿Declara este `key_id`? (coincidencia de diagnóstico, no de confianza).
    #[must_use]
    pub fn declares_key(&self, vk: &VerifyingKey) -> bool {
        self.key_id == key_id(vk)
    }

    /// Verifica la firma del package `digest` contra el anillo de confianza.
    ///
    /// La confianza emana del anillo (el `key_id` es solo pista): la firma
    /// debe verificar contra **alguna** clave del anillo.
    ///
    /// # Errors
    /// - [`ArcaError::InvalidSignature`] si ninguna clave del anillo valida.
    pub fn verify(&self, digest: &Digest, ring: &RingOfTrust) -> Res<()> {
        ring.verify(digest, &self.sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "signer")]
    use crate::signer::{keygen, sign_digest};

    #[test]
    fn parse_rechaza_no_v1() {
        // Longitud corta/larga:
        assert!(PackageSignature::parse(&[0u8; 64]).is_err());
        assert!(PackageSignature::parse(&[0u8; 107]).is_err());
        assert!(PackageSignature::parse(&[]).is_err());
        // Magic mal:
        let mut b = [0u8; 106];
        b[..9].copy_from_slice(b"ARCA-SIG2");
        b[9] = 1;
        assert!(PackageSignature::parse(&b).is_err());
        // Versión mal:
        let mut b = [0u8; 106];
        b[..9].copy_from_slice(SIG_MAGIC);
        b[9] = 2;
        assert!(PackageSignature::parse(&b).is_err());
    }

    #[test]
    fn roundtrip_bytes_y_from_parts() {
        let s = PackageSignature::from_parts(Digest::ZERO, [7u8; 64]);
        let bytes = s.to_bytes();
        assert_eq!(bytes.len(), 106);
        assert_eq!(&bytes[..9], b"ARCA-SIG1");
        assert_eq!(bytes[9], 1);
        let s2 = PackageSignature::parse(&bytes).expect("roundtrip");
        assert_eq!(s, s2);
        assert_eq!(s2.key_id(), &Digest::ZERO);
        assert_eq!(s2.sig(), &[7u8; 64]);
    }

    #[test]
    fn read_write_file_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("signature.bin");
        let s = PackageSignature::from_parts(Digest::of(b"k"), [9u8; 64]);
        s.write_file(&path).expect("write");
        let s2 = PackageSignature::read_file(&path).expect("read");
        assert_eq!(s, s2);
    }

    #[cfg(feature = "signer")]
    #[test]
    fn key_id_estable_y_declarado() {
        let (sk, vk) = keygen().expect("keygen");
        let digest = Digest::of(b"paquete");
        let pkg_sig = crate::signer::sign_package(&digest, &sk);
        assert!(pkg_sig.declares_key(&vk));
        assert_eq!(pkg_sig.key_id(), &key_id(&vk));
        // La firma cruda verifica contra la pubkey por mensaje:
        let sig = sign_digest(&digest, &sk);
        assert!(vk.verify_strict(digest.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig)).is_ok());
    }
}
