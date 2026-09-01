//! Verificación streaming verify-while-extract (spec 08 §3).
//!
//! Lo usa el installer: mientras el 7z descomprime, cada chunk pasa por
//! [`StreamingVerifier::feed`]; [`StreamingVerifier::end_file`] cierra un
//! archivo y comprueba SU sha inmediatamente (abort temprano — a mitad de
//! paquete inválido → abort + limpieza, docs/06 §5); [`StreamingVerifier::finish`]
//! valida completitud, el blake3 del manifest, el digest canónico y la firma
//! ed25519 contra el anillo.
//!
//! Modelo de cobertura:
//! - `expected` = archivos declarados en el manifest (artifacts) con su
//!   **sha256**. NO incluye `manifest.toml` (auto-referencia imposible),
//!   `meta/signature.bin` ni `meta/manifest.digest`.
//! - `manifest_sha` = blake3-256 de `manifest.toml` (registro `M`).

use std::collections::BTreeMap;

use arca_types::{ArcaError, Res};
use sha2::{Digest as _, Sha256};

use crate::digest::package_digest;
use crate::ring::RingOfTrust;

/// Verificador streaming de un paquete `.arca`.
///
/// Invariante: **ningún byte se escribe en disco de destino antes de que
/// `end_file`/`finish` hayan validado el contenido** (el installer escribe en
/// staging; este tipo solo acumula estado de hash, nunca contenido).
pub struct StreamingVerifier {
    /// path → sha256 esperado (según manifest).
    expected: BTreeMap<String, [u8; 32]>,
    /// path → hasher sha256 en curso (aún sin `end_file`).
    fed: BTreeMap<String, Sha256>,
    /// path → sha256 ya verificado (post `end_file`).
    done: BTreeMap<String, [u8; 32]>,
    /// blake3 incremental de `manifest.toml`.
    manifest_hasher: blake3::Hasher,
    /// true si se alimentó al menos un chunk del manifest.
    manifest_fed: bool,
    /// true tras `end_file(MANIFEST_PATH)`: feed posterior = duplicado.
    manifest_closed: bool,
    manifest_sha: [u8; 32],
    ring: RingOfTrust,
    sig: [u8; 64],
}

impl StreamingVerifier {
    /// Construye el verificador para un paquete.
    ///
    /// - `expected`: archivos declarados (sin manifest/signature/manifest.digest).
    /// - `manifest_sha`: blake3-256 de `manifest.toml` (valida también el
    ///   contenido de `meta/manifest.digest`, lo comprueba el installer).
    /// - `ring` + `sig`: verificación final ed25519.
    pub fn new(
        expected: &BTreeMap<String, [u8; 32]>,
        manifest_sha: [u8; 32],
        ring: &RingOfTrust,
        sig: [u8; 64],
    ) -> Self {
        Self {
            expected: expected.clone(),
            fed: BTreeMap::new(),
            done: BTreeMap::new(),
            manifest_hasher: blake3::Hasher::new(),
            manifest_fed: false,
            manifest_closed: false,
            manifest_sha,
            ring: ring.clone(),
            sig,
        }
    }

    /// Alimenta un chunk de un archivo (chunks arbitrarios, orden libre
    /// dentro del mismo archivo).
    ///
    /// Errores: archivo inesperado, o feed después de `end_file` (duplicado).
    pub fn feed(&mut self, path: &str, chunk: &[u8]) -> Res<()> {
        if path == crate::MANIFEST_PATH {
            if self.manifest_closed {
                return Err(ArcaError::InvalidPackage(
                    "stream: feed de manifest tras end_file",
                ));
            }
            self.manifest_hasher.update(chunk);
            self.manifest_fed = true;
            return Ok(());
        }
        if self.done.contains_key(path) {
            return Err(ArcaError::InvalidPackage(
                "stream: feed tras end_file (duplicado)",
            ));
        }
        match self.expected.get(path) {
            Some(_) => {
                self.fed.entry(path.to_owned()).or_default().update(chunk);
                Ok(())
            }
            // TODO lo no declarado se rechaza — incluidos signature.bin y
            // manifest.digest: el installer los lee POR SEPARADO (extracción
            // selectiva) y jamás los alimenta por este stream. Un stream que
            // los contenga = paquete manipulado (duplicados).
            None => Err(ArcaError::InvalidPackage(
                "stream: entrada no listada en el manifest",
            )),
        }
    }

    /// Cierra un archivo: finaliza su sha256 y lo comprueba YA (abort temprano
    /// en el primer archivo corrupto, sin esperar al final del paquete).
    ///
    /// No es obligatorio llamarlo (retrocompat con feed→finish): `finish`
    /// finaliza los pendientes.
    pub fn end_file(&mut self, path: &str) -> Res<()> {
        if path == crate::MANIFEST_PATH {
            // El manifest no tiene sha en `expected`: su validación es el
            // blake3 contra manifest_sha en `finish`.
            self.manifest_closed = true;
            return Ok(());
        }
        if let Some(h) = self.fed.remove(path) {
            let got: [u8; 32] = h.finalize().into();
            match self.expected.get(path) {
                Some(want) if want == &got => {
                    self.done.insert(path.to_owned(), got);
                    Ok(())
                }
                Some(_) => Err(ArcaError::InvalidPackage(
                    "stream: sha256 no coincide (end_file)",
                )),
                None => Err(ArcaError::InvalidPackage(
                    "stream: end_file de archivo no listado",
                )),
            }
        } else if self.done.contains_key(path) || self.expected.contains_key(path) {
            // ya cerrado antes (duplicado benigno) o nunca alimentado:
            // "nunca alimentado" se detecta en finish; aquí solo error duro
            // para archivos que nadie listó.
            if self.expected.contains_key(path) && !self.done.contains_key(path) {
                Err(ArcaError::InvalidPackage(
                    "stream: end_file sin ningún feed previo",
                ))
            } else {
                Ok(())
            }
        } else {
            Err(ArcaError::InvalidPackage(
                "stream: end_file de archivo no listado",
            ))
        }
    }

    /// Finaliza: completa los pendientes, valida TODO (shas, manifest, digest
    /// canónico, firma). Consume el verificador.
    pub fn finish(mut self) -> Res<()> {
        // 1. cerrar pendientes (feed sin end_file)
        let pendientes: Vec<String> = self.fed.keys().cloned().collect();
        for p in pendientes {
            self.end_file(&p)?;
        }
        // 2. completitud
        if !self.manifest_fed {
            return Err(ArcaError::InvalidPackage(
                "stream: manifest.toml nunca alimentado",
            ));
        }
        for path in self.expected.keys() {
            if !self.done.contains_key(path) {
                return Err(ArcaError::InvalidPackage(
                    "stream: archivo faltante en el stream",
                ));
            }
        }
        // 3. manifest blake3
        if self.manifest_hasher.finalize().as_bytes() != &self.manifest_sha[..] {
            return Err(ArcaError::InvalidPackage("stream: manifest.toml alterado"));
        }
        // 4. digest canónico + firma del anillo
        let entries: Vec<(&str, [u8; 32])> = self
            .expected
            .iter()
            .map(|(p, s)| (p.as_str(), *s))
            .collect();
        let digest = package_digest(&entries, self.manifest_sha);
        self.ring.verify(&digest, &self.sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arca_types::Digest;

    fn sha256(b: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        sha2::Digest::update(&mut h, b);
        h.finalize().into()
    }

    /// Archivos del paquete: manifest (blake3 aparte) + 3 archivos declarados.
    type Fix = (BTreeMap<String, [u8; 32]>, [u8; 32], Vec<(String, Vec<u8>)>);

    fn fixture() -> Fix {
        let files = vec![
            (
                "manifest.toml".to_string(),
                b"[package]\nid = \"com.x\"\nversion = \"1.0.0\"\n".to_vec(),
            ),
            (
                "bin/native-aarch64/app".to_string(),
                vec![0x7fu8, 69, 70, 70, 1, 2, 3],
            ),
            ("assets/data.bin".to_string(), vec![9u8; 1000]),
            ("meta/graph.mmd".to_string(), b"graph TD\nA-->B\n".to_vec()),
        ];
        let mut expected = BTreeMap::new();
        for (p, c) in files.iter().skip(1) {
            // manifest.toml NO va en expected (auto-referencia imposible)
            if p != "manifest.toml" {
                expected.insert(p.clone(), sha256(c));
            }
        }
        let manifest_sha = Digest::of(&files[0].1).0;
        (expected, manifest_sha, files)
    }

    fn verifier() -> (StreamingVerifier, Vec<(String, Vec<u8>)>) {
        let (expected, manifest_sha, files) = fixture();
        let ring = RingOfTrust::empty();
        let v = StreamingVerifier::new(&expected, manifest_sha, &ring, [3u8; 64]);
        (v, files)
    }

    #[test]
    fn contenido_ok_firma_mala_fail_closed() {
        let (mut v, files) = verifier();
        for (p, c) in &files {
            for ch in c.chunks(7) {
                assert!(v.feed(p, ch).is_ok());
            }
            assert!(v.end_file(p).is_ok());
        }
        // anillo vacío → la firma falla DESPUÉS de pasar todo el contenido
        assert!(matches!(v.finish(), Err(ArcaError::InvalidSignature)));
    }

    #[test]
    fn end_file_aborta_tematico_en_primer_sha_malo() {
        let (mut v, files) = verifier();
        for (p, c) in files.iter().take(2) {
            for ch in c.chunks(64) {
                let _ = v.feed(p, ch);
            }
            let _ = v.end_file(p);
        }
        // tercer archivo mutado
        let mut malo = files[2].1.clone();
        malo[3] ^= 0xff;
        for ch in malo.chunks(64) {
            let _ = v.feed("assets/data.bin", ch);
        }
        let r = v.end_file("assets/data.bin");
        assert!(matches!(r, Err(ArcaError::InvalidPackage(_))));
    }

    #[test]
    fn archivo_faltante() {
        let (mut v, files) = verifier();
        for (p, c) in files.iter().take(2) {
            for ch in c.chunks(64) {
                let _ = v.feed(p, ch);
            }
        }
        assert!(matches!(v.finish(), Err(ArcaError::InvalidPackage(_))));
    }

    #[test]
    fn entrada_sobrante_rechazada_en_feed() {
        let (mut v, files) = verifier();
        for (p, c) in &files {
            for ch in c.chunks(64) {
                let _ = v.feed(p, ch);
            }
        }
        let r = v.feed("evil/extra.bin", b"x");
        assert!(matches!(r, Err(ArcaError::InvalidPackage(_))));
    }

    #[test]
    fn feed_tras_end_file_duplicado() {
        let (mut v, files) = verifier();
        for (p, c) in files.iter().take(2) {
            for ch in c.chunks(64) {
                let _ = v.feed(p, ch);
            }
            let _ = v.end_file(p);
        }
        let r = v.feed(&files[1].0, b"mas bytes");
        assert!(matches!(r, Err(ArcaError::InvalidPackage(_))));
    }

    #[test]
    fn truncado_a_la_mitad_abort_limpio() {
        // simula interrupción de red/proceso: nunca se llama finish
        let (mut v, files) = verifier();
        for ch in files[0].1.chunks(4) {
            let _ = v.feed("manifest.toml", ch);
        }
        drop(v); // sin pánico, sin efectos (nada escrito a disco)
    }

    #[test]
    fn manifest_alterado_detectado() {
        let (expected, manifest_sha, files) = fixture();
        let ring = RingOfTrust::empty();
        let mut v = StreamingVerifier::new(&expected, manifest_sha, &ring, [3u8; 64]);
        let mut m = files[0].1.clone();
        m.extend_from_slice(b"# comentario extra");
        for ch in m.chunks(8) {
            let _ = v.feed("manifest.toml", ch);
        }
        for (p, c) in files.iter().skip(1) {
            for ch in c.chunks(64) {
                let _ = v.feed(p, ch);
            }
        }
        assert!(matches!(v.finish(), Err(ArcaError::InvalidPackage(_))));
    }
}
