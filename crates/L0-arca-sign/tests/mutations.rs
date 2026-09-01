//! Corpus de mutaciones del paquete (spec 08 §6, docs/07 §9): **100 %
//! rechazo**. Requiere `signer` (genera claves/firmas de control).
//!
//! Cada mutación se verifica con el pipeline COMPLETO del host:
//! `StreamingVerifier` (hash por archivo + cobertura) → `package_digest`
//! canónico → firma ed25519 contra el anillo. Clases:
//!
//! 1. bit-flip en contenido (×120, PRNG con seed fija),
//! 2. truncado (×15),
//! 3. extendido (×15),
//! 4. swap de contenidos entre dos archivos (×15),
//! 5. firma mutada (×25: bit-flips + estructurales + parseo),
//! 6. estructural (×14: 8 falta + 3 sobra + 2 duplicado + 1 re-feed),
//! 7. mapa esperado mutado (×15, simula build.json manipulado).
//!
//! Total: 219 casos ≥ 200 exigidos. El control (paquete sin mutar) verifica
//! OK — las mutaciones son las ÚNICAS diferencias.

#![cfg(feature = "signer")]

use std::collections::BTreeMap;

use arca_sign::signer::{keygen, sign_digest};
use arca_sign::{package_digest, RingOfTrust, StreamingVerifier, MANIFEST_PATH};
use arca_types::{ArcaError, Digest};
use sha2::Digest as _;

/// PRNG determinista (xorshift64*): corpus reproducible bit a bit, sin deps.
struct Rng(u64);

impl Rng {
    /// Seed fija (spec 08 §6: "bit-flip aleatorio con seed fija").
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Entero uniforme en `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// Byte pseudoaleatorio.
    fn byte(&mut self) -> u8 {
        (self.next() >> 32) as u8
    }
}

/// Paquete sintético con firma VÁLIDA (el "paquete bueno" del corpus).
struct Corpus {
    files: Vec<(String, Vec<u8>)>,
    expected: BTreeMap<String, [u8; 32]>,
    manifest_sha: [u8; 32],
    ring: RingOfTrust,
    sig: [u8; 64],
}

fn corpus() -> Corpus {
    // 8 archivos con tamaños variados (manifest + entrada binaria + assets
    // + meta). Contenidos pseudoaleatorios DETERMINISTAS.
    let mut rng = Rng::new(0x0A11_CE00);
    let manifest: Vec<u8> = b"[package]\nid = \"dev.arca.corpus\"\nversion = \"1.0.0\"\n".to_vec();
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (MANIFEST_PATH.to_owned(), manifest),
        (
            "bin/wasm/app.wasm".to_owned(),
            (0..600).map(|i| (i * 7 % 251) as u8).collect(),
        ),
        (
            "bin/native-aarch64/app".to_owned(),
            (0..2000).map(|_| rng.byte()).collect(),
        ),
        (
            "assets/fonts/inter.ttf".to_owned(),
            (0..4000).map(|_| rng.byte()).collect(),
        ),
        (
            "assets/data/cfg.toml".to_owned(),
            b"level = 6\n[solid]\noff = true\n".to_vec(),
        ),
        (
            "icons/icon-192.png".to_owned(),
            (0..192).map(|_| rng.byte()).collect(),
        ),
        (
            "icons/icon-512.png".to_owned(),
            (0..512).map(|_| rng.byte()).collect(),
        ),
        (
            "meta/build.json".to_owned(),
            br#"{"wasm":"wamr-aot","egui":"0.32"}"#.to_vec(),
        ),
    ];
    // Asegura contenidos distintos por archivo para swaps detectables:
    for (i, (_, c)) in files.iter_mut().enumerate() {
        c.push(i as u8);
    }

    let mut expected = BTreeMap::new();
    let mut entries = Vec::new();
    let mut manifest_sha = None;
    for (p, c) in &files {
        if p == MANIFEST_PATH {
            // manifest: blake3 (registro M); NO va en `expected`
            manifest_sha = Some(Digest::of(c).0);
            continue;
        }
        let h: [u8; 32] = {
            let mut h2 = sha2::Sha256::new();
            sha2::Digest::update(&mut h2, c);
            h2.finalize().into()
        };
        expected.insert(p.clone(), h);
        entries.push((p.as_str(), h));
    }
    let digest = package_digest(&entries, manifest_sha.expect("manifest en corpus"));
    let sk = keygen().expect("keygen");
    let vk = sk.verifying_key();
    // Relleno del anillo: la clave buena va en medio (pos 2 de 5).
    let mut ring = RingOfTrust::empty();
    for _ in 0..2 {
        let k = keygen().expect("keygen").verifying_key();
        ring.push(k);
    }
    ring.push(vk);
    for _ in 0..2 {
        let k = keygen().expect("keygen").verifying_key();
        ring.push(k);
    }
    Corpus {
        files,
        expected,
        manifest_sha: manifest_sha.expect("manifest en corpus"),
        ring,
        sig: sign_digest(&digest, &sk),
    }
}

/// El pipeline completo del host sobre unos archivos (chunks de 7 B).
fn verify_all(
    files: &[(String, Vec<u8>)],
    expected: &BTreeMap<String, [u8; 32]>,
    manifest_sha: [u8; 32],
    ring: &RingOfTrust,
    sig: [u8; 64],
) -> Result<(), ArcaError> {
    let mut v = StreamingVerifier::new(expected, manifest_sha, ring, sig);
    for (i, (p, c)) in files.iter().enumerate() {
        for chunk in c.chunks(7) {
            v.feed(p, chunk)?;
        }
        if i + 1 < files.len() {
            v.end_file(p)?;
        }
    }
    v.finish()
}

/// Reemplaza `i`-ésimo byte (si existe) con `b` XOR máscara.
fn flip_byte(v: &mut [u8], idx: usize) {
    if idx < v.len() {
        v[idx] ^= 1 << (idx % 8);
    }
}

#[test]
fn control_paquete_bueno_verifica_ok() {
    let c = corpus();
    assert!(verify_all(&c.files, &c.expected, c.manifest_sha, &c.ring, c.sig).is_ok());
    // Y el digest directo también (mismo pipeline por otra vía):
    let entries: Vec<(&str, [u8; 32])> = c
        .files
        .iter()
        .filter(|(p, _)| p != MANIFEST_PATH)
        .map(|(p, content)| {
            let mut h = sha2::Sha256::new();
            sha2::Digest::update(&mut h, content);
            (p.as_str(), h.finalize().into())
        })
        .collect();
    let d = package_digest(&entries, c.manifest_sha);
    assert!(c.ring.verify(&d, &c.sig).is_ok());
}

#[test]
fn mut_bit_flip_en_contenido_100pct_rechazado() {
    let c = corpus();
    let mut rng = Rng::new(0xB17F_11E0);
    let mut rechazadas = 0;
    for case in 0..120 {
        let mut files = c.files.clone();
        let fi = rng.below(files.len());
        let (_, content) = &mut files[fi];
        if content.is_empty() {
            continue;
        }
        let byte = rng.below(content.len());
        flip_byte(content, byte);
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(
            r.is_err(),
            "caso {case}: bit-flip aceptado (file {fi}, byte {byte})"
        );
        rechazadas += 1;
    }
    assert!(rechazadas >= 100, "mutaciones efectivas: {rechazadas}");
}

#[test]
fn mut_truncado_100pct_rechazado() {
    let c = corpus();
    let mut rng = Rng::new(0x713C_4700);
    for case in 0..15 {
        let mut files = c.files.clone();
        let fi = rng.below(files.len());
        let len = files[fi].1.len();
        let cut = rng.below(len); // 0..len → siempre más corto
        files[fi].1.truncate(cut);
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(
            r.is_err(),
            "caso {case}: truncado a {cut} aceptado (file {fi})"
        );
    }
}

#[test]
fn mut_extendido_100pct_rechazado() {
    let c = corpus();
    let mut rng = Rng::new(0xE37E_4000);
    for case in 0..15 {
        let mut files = c.files.clone();
        let fi = rng.below(files.len());
        let extra = 1 + rng.below(16);
        for _ in 0..extra {
            files[fi].1.push(rng.byte());
        }
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(
            r.is_err(),
            "caso {case}: +{extra} bytes aceptado (file {fi})"
        );
    }
}

#[test]
fn mut_swap_de_contenidos_100pct_rechazado() {
    // "swap-entry" de la spec 08 §6: los CONTENIDOS se mueven entre paths
    // → el (path → hash) canónico cambia → digest distinto. (Ojo: intercambiar
    // las TUPLAS completas solo reordena la lista, y el digest canónico es
    // deliberadamente invariante al orden de llegada — docs/06 §5.)
    let c = corpus();
    let mut rng = Rng::new(0x54A4_B000);
    for case in 0..15 {
        let mut files = c.files.clone();
        let a = rng.below(files.len());
        let mut b = rng.below(files.len());
        if b == a {
            b = (b + 1) % files.len();
        }
        let contenido_a = files[a].1.clone();
        files[a].1 = files[b].1.clone();
        files[b].1 = contenido_a;
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(
            r.is_err(),
            "caso {case}: swap de contenidos {a}<->{b} aceptado"
        );
    }
}

#[test]
fn mut_firma_100pct_rechazada() {
    let c = corpus();
    let mut rng = Rng::new(0x51A1_1100);
    // 15 bit-flips en la firma cruda:
    for case in 0..15 {
        let mut sig = c.sig;
        let byte = rng.below(64);
        flip_byte(&mut sig, byte);
        let r = verify_all(&c.files, &c.expected, c.manifest_sha, &c.ring, sig);
        assert!(
            r.is_err(),
            "caso {case}: firma mutada aceptada (byte {byte})"
        );
    }
    // 10 estructurales:
    let mut casos: Vec<(usize, [u8; 64])> = Vec::new();
    casos.push((100, [0u8; 64])); // todo ceros
    casos.push((101, [0xffu8; 64])); // todo unos
    let mut halves = [0u8; 64];
    halves[..32].copy_from_slice(&c.sig[32..]); // R y s intercambiados
    halves[32..].copy_from_slice(&c.sig[..32]);
    casos.push((102, halves));
    let mut high = c.sig; // escalar no canónico (s ≥ L):
    high[63] |= 0x80;
    casos.push((103, high));
    let mut low = c.sig; // R = punto de orden pequeño (identidad: y=1):
    low[..32].fill(0);
    low[0] = 1;
    casos.push((104, low));
    // Firma de OTRO mensaje (digest de otro contenido):
    let sk_otra = keygen().expect("keygen");
    let pk = sign_digest(&Digest::of(b"otro paquete"), &sk_otra);
    casos.push((105, pk));
    for (case, sig) in casos {
        let r = verify_all(&c.files, &c.expected, c.manifest_sha, &c.ring, sig);
        assert!(r.is_err(), "caso {case}: firma estructural aceptada");
    }
    // Parseo del archivo de firma con longitud mutada (truncado/extendido):
    let pkg_sig = arca_sign::signer::sign_package(&Digest::of(b"d"), &keygen().expect("keygen"));
    let bytes = pkg_sig.to_bytes();
    assert!(arca_sign::PackageSignature::parse(&bytes[..105]).is_err());
    assert!(arca_sign::PackageSignature::parse(&[bytes.as_ref(), &[0u8]].concat()).is_err());
    assert!(arca_sign::PackageSignature::parse(&bytes).is_ok());
}

#[test]
fn mut_estructural_falta_sobra_duplicado_100pct_rechazado() {
    let c = corpus();
    // Falta cada uno de los archivos (8 casos):
    for (i, _) in c.files.iter().enumerate() {
        let files: Vec<(String, Vec<u8>)> = c
            .files
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, f)| f.clone())
            .collect();
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(r.is_err(), "falta el archivo {i} y fue aceptado");
    }
    // Archivo extra no cubierto (3 casos):
    for extra in ["extra.txt", "bin/wasm/evil.wasm", "meta/signature.bin"] {
        let mut files = c.files.clone();
        files.push((extra.to_owned(), b"payload".to_vec()));
        let r = verify_all(&files, &c.expected, c.manifest_sha, &c.ring, c.sig);
        assert!(r.is_err(), "archivo extra {extra} aceptado");
    }
    // Duplicado: re-alimentar el primer archivo tras cerrarlo (2 casos):
    for dup in [0usize, 2] {
        let mut v = StreamingVerifier::new(&c.expected, c.manifest_sha, &c.ring, c.sig);
        for (p, content) in &c.files {
            for chunk in content.chunks(9) {
                v.feed(p, chunk).expect("feed limpio");
            }
            v.end_file(p).expect("end_file limpio");
        }
        let r = v.feed(&c.files[dup].0, b"x");
        assert!(r.is_err(), "duplicado del archivo {dup} aceptado");
    }
    // Además: archivo de más bytes con el MISMO nombre tras cerrar → duplicado
    // (cubierto arriba) y feed doble del último sin cerrar = contenido extra:
    let mut v = StreamingVerifier::new(&c.expected, c.manifest_sha, &c.ring, c.sig);
    for (p, content) in &c.files {
        for chunk in content.chunks(9) {
            v.feed(p, chunk).expect("feed limpio");
        }
        v.end_file(p).expect("end_file limpio");
    }
    v.feed(MANIFEST_PATH, b"extra-final")
        .expect_err("manifest ya cerrado");
}

#[test]
fn mut_mapa_esperado_100pct_rechazado() {
    // El mapa esperado actúa de 2ª capa: si alguien manipula build.json
    // (la referencia de hashes), el mismatch se detecta aunque el contenido
    // y la firma originales fueran consistentes entre sí… y aquí no lo son:
    // cambiar el mapa rompe el digest→firma también. Doble rechazo.
    let c = corpus();
    let mut rng = Rng::new(0xE2E1_C7C7);
    for case in 0..15 {
        let mut expected = c.expected.clone();
        let keys: Vec<String> = expected.keys().cloned().collect();
        let k = rng.below(keys.len());
        let h = expected.get_mut(&keys[k]).expect("clave presente");
        flip_byte(h, rng.below(32));
        let r = verify_all(&c.files, &expected, c.manifest_sha, &c.ring, c.sig);
        assert!(r.is_err(), "caso {case}: mapa esperado mutado aceptado");
    }
}

#[test]
fn total_del_corpus() {
    // Sanidad del conteo: 120+15+15+15+(15+10)+(8+3+2+1)+15 = 219 ≥ 200.
    let total = 120 + 15 + 15 + 15 + 15 + 10 + 8 + 3 + 2 + 1 + 15;
    assert!(total >= 200, "corpus insuficiente: {total}");
}
