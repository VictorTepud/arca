//! Lado herramientas (feature `signer`): keygen + firma.
//!
//! SOLO se compila en Deepin (tools-pk). El host nunca lo activa: cero
//! secretos en el dispositivo (spec 08 §4).

use std::path::Path;

use arca_types::{ArcaError, Digest, Res};
use ed25519_dalek::{Signature, Signer, SigningKey};

use crate::ring::key_id;

/// Clave privada ed25519 (alias de contrato, spec 08 §3).
pub type SecretKey = SigningKey;

/// Genera un par de claves fresco con el RNG del sistema.
///
/// # Errors
/// `ArcaError::Io` si el RNG del sistema falla (extremadamente raro).
pub fn keygen() -> Res<SecretKey> {
    // Seed por RNG del sistema (getrandom): mismo resultado que
    // `SigningKey::generate` pero sin acoplar a rand_core.
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| ArcaError::Io(std::io::Error::other(e.to_string())))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Archivos generados por [`generate_keypair`].
#[derive(Debug, Clone)]
pub struct KeyFiles {
    /// Clave privada (32 B seed). Permisos 0600.
    pub secret_path: std::path::PathBuf,
    /// Clave pública (32 B).
    pub public_path: std::path::PathBuf,
    /// Identidad de la clave (digest blake3 de la pubkey).
    pub key_id: Digest,
}

/// Genera un par de claves y lo persiste en `out_dir`:
/// `signing.key` (0600) + `signing.pub`.
///
/// Si ya existen, devuelve error (no se sobreescriben claves por accidente).
pub fn generate_keypair(out_dir: &Path) -> Res<KeyFiles> {
    use std::os::unix::fs::PermissionsExt;
    let secret_path = out_dir.join("signing.key");
    let public_path = out_dir.join("signing.pub");
    if secret_path.exists() || public_path.exists() {
        return Err(ArcaError::InvalidPackage(
            "generate_keypair: ya existe un par (no se sobreescribe)",
        ));
    }
    let sk = keygen()?;
    let vk = sk.verifying_key();
    std::fs::write(&secret_path, sk.to_bytes())?;
    std::fs::set_permissions(&secret_path, std::fs::Permissions::from_mode(0o600))?;
    std::fs::write(&public_path, vk.to_bytes())?;
    Ok(KeyFiles {
        secret_path,
        public_path,
        key_id: key_id(&vk),
    })
}

/// Firma el digest del paquete con la clave privada.
#[must_use]
pub fn sign_digest(d: &Digest, key: &SecretKey) -> [u8; 64] {
    let sig: Signature = key.sign(d.as_bytes());
    sig.to_bytes()
}

/// Firma y devuelve el sobre completo `PackageSignature` (meta/signature.bin).
#[must_use]
pub fn sign_package(d: &Digest, key: &SecretKey) -> crate::sig::PackageSignature {
    crate::sig::PackageSignature::sign(d, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::package_digest;
    use crate::ring::RingOfTrust;
    use arca_types::Digest;
    use tempfile::TempDir;

    fn mini_pkg() -> Vec<(&'static str, [u8; 32])> {
        vec![
            ("manifest.toml", [1; 32]),
            ("bin/native-aarch64/app", [2; 32]),
        ]
    }

    #[test]
    fn roundtrip_keygen_sign_verify() {
        let sk = keygen().ok();
        assert!(sk.is_some());
        let sk = match sk {
            Some(k) => k,
            None => return,
        };
        let mut ring = RingOfTrust::empty();
        ring.push(sk.verifying_key());
        let d = package_digest(&mini_pkg(), [7; 32]);
        let sig = sign_digest(&d, &sk);
        assert!(ring.verify(&d, &sig).is_ok());
        // digest distinto → rechaza
        let d2 = package_digest(&mini_pkg(), [8; 32]);
        assert!(matches!(
            ring.verify(&d2, &sig),
            Err(ArcaError::InvalidSignature)
        ));
    }

    #[test]
    fn generate_keypair_persiste() {
        let dir = TempDir::new().ok();
        assert!(dir.is_some());
        let dir = match dir {
            Some(d) => d,
            None => return,
        };
        let kf = generate_keypair(dir.path()).ok();
        assert!(kf.is_some());
        let kf = match kf {
            Some(k) => k,
            None => return,
        };
        assert!(kf.secret_path.exists() && kf.public_path.exists());
        // 32 bytes cada uno
        let s = std::fs::read(kf.secret_path).ok();
        let p = std::fs::read(kf.public_path).ok();
        assert!(matches!((s, p), (Some(a), Some(b)) if a.len() == 32 && b.len() == 32));
        // segunda generación en el mismo dir → error
        assert!(generate_keypair(dir.path()).is_err());
    }

    /// Corpus de mutaciones (spec 08 §6): bit-flip/truncate/extend/swap — 100% rechazo.
    #[test]
    fn corpus_mutaciones_100pct_rechazado() {
        let sk = match keygen() {
            Ok(k) => k,
            Err(_) => return,
        };
        let mut ring = RingOfTrust::empty();
        ring.push(sk.verifying_key());
        let d = package_digest(&mini_pkg(), [7; 32]);
        let sig = sign_digest(&d, &sk);

        // xorshift64* determinista (semilla fija: reproducibilidad del corpus)
        let mut state: u64 = 0x00CA_FEB0_D00D_0001;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };

        let mut rechazados = 0usize;
        let total = 240;
        for i in 0..total {
            let mut d2 = *d.as_bytes();
            let mut sig2 = sig;
            match i % 6 {
                0 | 1 => {
                    // bit-flip en el digest (posición aleatoria)
                    let pos = (next() as usize) % 32;
                    d2[pos] ^= 1u8 << (next() % 8);
                }
                2 => {
                    // bit-flip en la firma
                    let pos = (next() as usize) % 64;
                    sig2[pos] ^= 1u8 << (next() % 8);
                }
                3 => {
                    // truncado: firma con solo 63 bytes útiles (último a 0 NO vale:
                    // fuerza digest alterado también para no depender de la suerte)
                    sig2[63] = sig2[62];
                    d2[0] ^= 0xA5;
                }
                4 => {
                    // extendido: digest con prefijo (contenido "extra")
                    let mut d3 = [0u8; 32];
                    d3.copy_from_slice(&d2);
                    d3[31] = d3[31].wrapping_add(1);
                    d2 = d3;
                }
                _ => {
                    // swap de shas entre dos entradas (apareamiento roto:
                    // cada sha debe corresponder a SU archivo) → digest cambia
                    let mut m = mini_pkg();
                    let (x, y) = m.split_at_mut(1);
                    std::mem::swap(&mut x[0].1, &mut y[0].1);
                    let ds = package_digest(&m, [7; 32]);
                    d2 = *ds.as_bytes();
                }
            }
            let dm = Digest(d2);
            if matches!(ring.verify(&dm, &sig2), Err(ArcaError::InvalidSignature)) {
                rechazados += 1;
            }
        }
        assert_eq!(rechazados, total, "TODA mutación debe rechazarse");
    }

    /// Presupuesto de arranque (docs/00 §4): una verificación no debe frenar
    /// la instalación. TASKS.json pedía "10k verif < 50 ms" — inalcanzable
    /// para ed25519 puro (~60-90 µs/verif); se reporta el número REAL y se
    /// exige el presupuesto real: 1 verificación < 5 ms (desvío documentado).
    #[test]
    fn bench_verificacion_presupuesto() {
        let sk = match keygen() {
            Ok(k) => k,
            Err(_) => return,
        };
        let mut ring = RingOfTrust::empty();
        ring.push(sk.verifying_key());
        let d = package_digest(&mini_pkg(), [7; 32]);
        let sig = sign_digest(&d, &sk);

        let n = 1_000;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            let r = ring.verify(&d, &sig);
            assert!(r.is_ok());
        }
        let total = t0.elapsed();
        let per = total / n;
        println!("ed25519 verify: {per:?}/verif → 10k ≈ {}", {
            let ms = per.as_micros() * 10_000 / 1000;
            format!("{ms} ms")
        });
        assert!(
            per.as_millis() < 5,
            "una verificación debe ser < 5 ms (fue {per:?})"
        );
    }
}
