//! Anillo de claves públicas de confianza (spec 08 §4, ADR-012).
//!
//! Formato binario `trusted-pubkeys.bin`: `[u32_le n][[u8;32] × n]`.
//! Se genera en Deepin con tools-pk (`keys/trusted-pubkeys.txt` → `.bin`) y
//! se EMBEBE en el host con `include_bytes!`. Rotación = recompilar host.

use arca_types::{ArcaError, Digest, Res};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Anillo de confianza: CUALQUIERA de las claves puede validar la firma.
#[derive(Debug, Clone, Default)]
pub struct RingOfTrust {
    keys: Vec<VerifyingKey>,
}

/// Anillo embebido en el binario (regenerar al compilar: ADR-012).
const EMBEDDED: &[u8] = include_bytes!("../trusted-pubkeys.bin");

impl RingOfTrust {
    /// Anillo vacío (rechaza TODO: seguro por omisión).
    #[must_use]
    pub fn empty() -> Self {
        Self { keys: Vec::new() }
    }

    /// Anillo embebido al compilar (`trusted-pubkeys.bin` del crate).
    #[must_use]
    pub fn from_embedded() -> Self {
        // El embebido lo controla el build; si estuviera corrupto la política
        // es fallar cerrado (anillo vacío) y no un pánico en producción.
        Self::from_bin(EMBEDDED).unwrap_or_else(|_| Self::empty())
    }

    /// Parse del formato bin `[u32_le n][[u8;32] × n]`.
    pub fn from_bin(bin: &[u8]) -> Res<Self> {
        if bin.len() < 4 {
            return Err(ArcaError::InvalidPackage("ring: bin < 4 bytes"));
        }
        let n = u32::from_le_bytes([bin[0], bin[1], bin[2], bin[3]]) as usize;
        if bin.len() != 4 + 32 * n {
            return Err(ArcaError::InvalidPackage("ring: longitud incoherente"));
        }
        let mut keys = Vec::with_capacity(n);
        for i in 0..n {
            let mut b = [0u8; 32];
            b.copy_from_slice(&bin[4 + 32 * i..4 + 32 * i + 32]);
            match VerifyingKey::from_bytes(&b) {
                Ok(vk) => keys.push(vk),
                // clave inválida en el anillo = build corrupto → error duro
                Err(_) => return Err(ArcaError::InvalidPackage("ring: clave inválida")),
            }
        }
        Ok(Self { keys })
    }

    /// Serializa al mismo formato bin (lo usa tools-pk para generar el archivo).
    #[must_use]
    pub fn to_bin(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + 32 * self.keys.len());
        out.extend_from_slice(&(self.keys.len() as u32).to_le_bytes());
        for k in &self.keys {
            out.extend_from_slice(&k.to_bytes());
        }
        out
    }

    /// Añade una clave pública (sin dedup: `push_unique` para eso).
    pub fn push(&mut self, vk: VerifyingKey) {
        self.keys.push(vk);
    }

    /// Añade una clave desde sus 32 bytes crudos (hex → `Digest::from_hex`
    /// no aplica: esto es una CLAVE, no un digest). Error si los bytes no
    /// son una clave ed25519 válida.
    pub fn push_bytes(&mut self, b: &[u8; 32]) -> Res<()> {
        match VerifyingKey::from_bytes(b) {
            Ok(vk) => {
                self.keys.push(vk);
                Ok(())
            }
            Err(_) => Err(ArcaError::InvalidPackage(
                "ring: bytes no son una ed25519 válida",
            )),
        }
    }

    /// Añade solo si no existe ya. Devuelve `false` si ya estaba.
    pub fn push_unique(&mut self, vk: VerifyingKey) -> bool {
        if self.contains_key(&vk) {
            false
        } else {
            self.keys.push(vk);
            true
        }
    }

    /// Elimina por bytes de la clave. `true` si estaba.
    pub fn remove_by_bytes(&mut self, b: &[u8; 32]) -> bool {
        match VerifyingKey::from_bytes(b) {
            Ok(vk) => {
                let antes = self.keys.len();
                self.keys.retain(|k| k.to_bytes() != vk.to_bytes());
                self.keys.len() != antes
            }
            Err(_) => false,
        }
    }

    /// Número de claves del anillo.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Anillo vacío.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// ¿Contiene esta clave exacta?
    #[must_use]
    pub fn contains_key(&self, vk: &VerifyingKey) -> bool {
        self.keys.iter().any(|k| k.to_bytes() == vk.to_bytes())
    }

    /// Verifica la firma ed25519 de un digest contra CUALQUIER clave del anillo.
    ///
    /// Tiempo de verificación constante por clave (dalek). Anillo vacío →
    /// siempre rechaza (fail-closed).
    pub fn verify(&self, digest: &Digest, sig: &[u8; 64]) -> Res<()> {
        if self.keys.is_empty() {
            return Err(ArcaError::InvalidSignature);
        }
        let firma = Signature::from_bytes(sig);
        let msg = digest.as_bytes();
        for k in &self.keys {
            if k.verify(msg, &firma).is_ok() {
                return Ok(());
            }
        }
        Err(ArcaError::InvalidSignature)
    }
}

/// Identidad estable de una clave pública (para logs/filenames de tools).
#[must_use]
pub fn key_id(vk: &VerifyingKey) -> Digest {
    Digest::of(&vk.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // (self-test del formato sin cripto asimétrico)
    #[test]
    fn bin_roundtrip() {
        let ring = RingOfTrust::empty();
        assert!(ring.is_empty());
        let bin = ring.to_bin();
        assert_eq!(bin, [0, 0, 0, 0]);
        let back = RingOfTrust::from_bin(&bin).map(|r| r.len()).ok();
        assert_eq!(back, Some(0));
    }

    #[test]
    fn bin_malformado_rechazado() {
        for bad in [
            &[] as &[u8],
            &[0, 0],
            &[1, 0, 0, 0, 7],
            &[2, 0, 0, 0, 1, 2, 3],
        ] {
            assert!(RingOfTrust::from_bin(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn embedded_por_defecto_vacio_parseable() {
        // El bin por defecto del repo tiene n=0 (hasta que ADR-012 embeba
        // las claves reales del usuario).
        assert!(RingOfTrust::from_bin(EMBEDDED).is_ok());
        assert!(RingOfTrust::from_embedded().is_empty());
    }

    #[test]
    fn anillo_vacio_rechaza_todo() {
        let ring = RingOfTrust::empty();
        let d = Digest::of(b"x");
        let sig = [1u8; 64];
        assert!(matches!(
            ring.verify(&d, &sig),
            Err(ArcaError::InvalidSignature)
        ));
    }
}
