//! Kit de fixtures: construye paquetes `.arca` REALES firmados (solo tests).

#![allow(clippy::missing_docs_in_private_items)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use arca_sign::signer::{keygen, SecretKey};
use arca_sign::{
    package_digest, PackageSignature, RingOfTrust, MANIFEST_DIGEST_PATH, MANIFEST_PATH,
    SIGNATURE_PATH,
};
use arca_types::Digest;

/// Claves del kit (la pública va al anillo para verificar).
pub(crate) struct TestRing {
    /// Clave privada (firma fixtures).
    pub(crate) sk: SecretKey,
    /// Anillo con la pública (para el Installer).
    pub(crate) ring: RingOfTrust,
}

/// Genera el par + anillo de un fixture.
pub(crate) fn test_ring() -> TestRing {
    let sk = match keygen() {
        Ok(k) => k,
        Err(e) => panic!("keygen del fixture: {e}"),
    };
    let mut ring = RingOfTrust::empty();
    ring.push(sk.verifying_key());
    TestRing { sk, ring }
}

/// hex minúsculas de 32 bytes.
fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        use std::fmt::Write as _;
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// sha256 de bytes (testkit: los manifest declaran sha256).
fn sha256(b: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(b);
    h.finalize().into()
}

/// Paquete declarable de fixture.
pub(crate) struct FixturePkg {
    /// Id de la app.
    pub(crate) id: String,
    /// Versión semver.
    pub(crate) version: String,
    /// Contenido del binario nativo (si None usa uno por defecto).
    pub(crate) bin: Option<Vec<u8>>,
    /// Archivos extra (path, bytes): se añaden SIN declarar.
    pub(crate) extra_files: Vec<(String, Vec<u8>)>,
    /// Si true, NO se firma (paquete malicioso).
    pub(crate) sin_firma: bool,
}

impl FixturePkg {
    /// Fixture estándar válido.
    pub(crate) fn new(id: &str, version: &str) -> Self {
        Self {
            id: id.to_owned(),
            version: version.to_owned(),
            bin: None,
            extra_files: Vec::new(),
            sin_firma: false,
        }
    }

    /// Binario custom.
    #[allow(dead_code)]
    pub(crate) fn with_bin(mut self, bin: Vec<u8>) -> Self {
        self.bin = Some(bin);
        self
    }

    /// Archivo extra sin declarar.
    pub(crate) fn with_extra(mut self, path: &str, data: Vec<u8>) -> Self {
        self.extra_files.push((path.to_owned(), data));
        self
    }

    fn bin_bytes(&self) -> Vec<u8> {
        self.bin.clone().unwrap_or_else(|| {
            let mut b = vec![0x7fu8, 69, 76, 70, 2, 1, 0]; // fake ELF64 LE
            b.extend((0..2048u32).map(|i| (i % 251) as u8));
            b
        })
    }

    fn manifest(&self, bin_sha_hex: &str) -> String {
        format!(
            "[package]\nid = \"{id}\"\nname = \"Fixture App\"\nversion = \"{v}\"\nmin_host = \"1.0.0\"\napi_level = 1\n\n[runtime]\nbackend_pref = \"native\"\nentry = \"app\"\nrespawn = \"never\"\n\n[artifacts.native]\npath = \"bin/native-aarch64/app\"\nsha256 = \"{sha}\"\n\n[profile]\nlaunch_budget_ms = 120\nmax_frame_mb = 2\n",
            id = self.id,
            v = self.version,
            sha = bin_sha_hex
        )
    }

    /// Empaqueta y firma el `.arca` en `out_dir/<id>-<version>.arca`.
    pub(crate) fn build(&self, out_dir: &Path, ring: &TestRing) -> PathBuf {
        let bin = self.bin_bytes();
        let bin_sha = sha256(&bin);
        let manifest = self.manifest(&hex32(&bin_sha)).into_bytes();
        let manifest_sha = Digest::of(&manifest).0;

        // digest canónico + firma ed25519 (mismo algoritmo que el host)
        let entries = vec![("bin/native-aarch64/app", bin_sha)];
        let digest = package_digest(&entries, manifest_sha);
        let sig_bytes = if self.sin_firma {
            PackageSignature {
                key_id: Digest::ZERO,
                sig: [9u8; 64],
            }
            .to_bytes()
        } else {
            PackageSignature::sign(&digest, &ring.sk).to_bytes()
        };

        // .arca = 7z; manifest primero (orden de streams, docs/06 §2)
        let out = out_dir.join(format!("{}-{}.arca", self.id, self.version));
        let file = match std::fs::File::create(&out) {
            Ok(f) => f,
            Err(e) => panic!("crear fixture: {e}"),
        };
        let mut w = match sevenz_rust2::ArchiveWriter::new(file) {
            Ok(w) => w,
            Err(e) => panic!("writer: {e}"),
        };
        let push = |w: &mut sevenz_rust2::ArchiveWriter<std::fs::File>, name: &str, data: &[u8]| {
            let entry = sevenz_rust2::ArchiveEntry {
                name: name.to_owned(),
                is_directory: false,
                has_stream: true,
                ..sevenz_rust2::ArchiveEntry::default()
            };
            if let Err(e) = w.push_archive_entry(entry, Some(Cursor::new(data.to_vec()))) {
                panic!("push {name}: {e}");
            }
        };
        push(&mut w, MANIFEST_PATH, &manifest);
        push(&mut w, "bin/native-aarch64/app", &bin);
        push(&mut w, "assets/data/blob.bin", &vec![7u8; 64 * 1024]);
        push(&mut w, "meta/graph.mmd", b"graph TD\nmain-->install\n");
        push(&mut w, "meta/build.json", br#"{"toolchain":"fixture"}"#);
        for (p, c) in &self.extra_files {
            push(&mut w, p, c);
        }
        push(
            &mut w,
            MANIFEST_DIGEST_PATH,
            hex32(&manifest_sha).as_bytes(),
        );
        push(&mut w, SIGNATURE_PATH, &sig_bytes);
        if let Err(e) = w.finish() {
            panic!("finish: {e}");
        }
        out
    }
}
