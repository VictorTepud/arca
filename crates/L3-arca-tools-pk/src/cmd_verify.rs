//! `verify`: el MISMO algoritmo que ejecutará el host (spec 25 §4:
//! "no re-implementar NADA": reutiliza arca-7z + arca-sign).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use arca_7z::{Archive, EntrySink, ExtractPlan, RelPath};
use arca_pkg_model::{ArchiveEntries, EntryKind, Manifest};
use arca_sign::{
    package_digest, PackageSignature, RingOfTrust, StreamingVerifier, MANIFEST_DIGEST_PATH,
    MANIFEST_PATH, SIGNATURE_PATH,
};
use arca_types::{ArcaError, Digest, Res};

use crate::read_pubkey;

/// Sink de verificación: NADA escribe; solo hashea y alimenta el verifier.
struct VerifySink<'a> {
    verifier: &'a mut StreamingVerifier,
    expected: BTreeSet<String>,
}

impl EntrySink for VerifySink<'_> {
    fn mkdir(&mut self, _rel: &RelPath) -> Res<()> {
        Ok(())
    }

    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn std::io::Read) -> Res<u64> {
        let path = rel.as_str().to_owned();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        if self.expected.contains(&path) {
            loop {
                let n = data.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                self.verifier.feed(&path, &buf[..n])?;
            }
            self.verifier.end_file(&path)?;
        } else {
            // drenar sin verificar (assets no declarados)
            loop {
                let n = data.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
            }
        }
        Ok(total)
    }

    fn root(&self) -> &std::path::Path {
        Path::new("/verify-sink")
    }
}

/// Verifica un `.arca` contra una pubkey (32 B o hex64).
pub(crate) fn run(file: &Path, pubkey: &Path) -> Res<()> {
    let pub_bytes = read_pubkey(pubkey)?;
    let mut ring = RingOfTrust::empty();
    ring.push_bytes(&pub_bytes)?;

    let mut reader = std::fs::File::open(file)?;
    let mut archive = Archive::open(&mut reader)?;
    let entries = archive.entries()?;

    // listing + layout
    let mut listing = ArchiveEntries::new();
    for e in &entries {
        listing.push(
            e.path.clone(),
            if e.is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
        );
    }

    // pass 1: manifest + firma + digest de control (a memoria)
    let plan1 = ExtractPlan::parse(&[MANIFEST_PATH, SIGNATURE_PATH, MANIFEST_DIGEST_PATH])?;
    let mut mem = MemSink::new();
    archive.extract(&plan1, &mut mem, &mut |_| {})?;
    let manifest_bytes = mem
        .get(MANIFEST_PATH)
        .ok_or(ArcaError::InvalidPackage("verify: manifest.toml ausente"))?
        .to_vec();
    let psig = PackageSignature::parse(
        mem.get(SIGNATURE_PATH)
            .ok_or(ArcaError::InvalidPackage("verify: signature.bin ausente"))?,
    )?;
    let md_hex = String::from_utf8_lossy(mem.get(MANIFEST_DIGEST_PATH).unwrap_or_default())
        .trim()
        .to_owned();
    let manifest_sha = Digest::of(&manifest_bytes).0;
    if !matches!(Digest::from_hex(&md_hex), Ok(d) if d.0 == manifest_sha) {
        return Err(ArcaError::InvalidPackage(
            "verify: manifest.digest no coincide",
        ));
    }

    let manifest = Manifest::parse(&manifest_bytes)?;
    manifest.validate_layout(&listing)?;

    let expected: BTreeMap<String, [u8; 32]> = manifest
        .artifacts
        .values()
        .map(|a| (a.path.as_str().to_owned(), a.sha256))
        .collect();

    // pass 2: hash + verify streaming (exactamente el camino del host)
    let mut verifier = StreamingVerifier::new(&expected, manifest_sha, &ring, *psig.sig_bytes());
    let skip = [MANIFEST_PATH, SIGNATURE_PATH, MANIFEST_DIGEST_PATH];
    let wanted: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|e| e.safe_path())
        .map(|p| p.as_str().to_owned())
        .filter(|p| !skip.contains(&p.as_str()))
        .collect();
    let wanted_refs: Vec<&str> = wanted.iter().map(String::as_str).collect();
    let plan2 = ExtractPlan::parse(&wanted_refs)?;
    let mut sink = VerifySink {
        verifier: &mut verifier,
        expected: expected.keys().cloned().collect(),
    };
    archive.extract(&plan2, &mut sink, &mut |_| {})?;
    verifier.feed(MANIFEST_PATH, &manifest_bytes)?;
    verifier.finish()?;

    let d = package_digest(
        &expected
            .iter()
            .map(|(p, h)| (p.as_str(), *h))
            .collect::<Vec<(&str, [u8; 32])>>(),
        manifest_sha,
    );
    println!("verify: OK — {} ({})", file.display(), manifest.package.id);
    println!("        digest {d} · key_id {}", psig.key_id);
    Ok(())
}

/// Sink en memoria para los 3 archivos meta (mismo patrón del installer).
struct MemSink {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemSink {
    fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }
    fn get(&self, p: &str) -> Option<&[u8]> {
        self.files.get(p).map(Vec::as_slice)
    }
}

impl EntrySink for MemSink {
    fn mkdir(&mut self, _rel: &RelPath) -> Res<()> {
        Ok(())
    }
    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn std::io::Read) -> Res<u64> {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        if buf.len() > 2 * 1024 * 1024 {
            return Err(ArcaError::InvalidPackage(
                "verify: meta gigante (sospechoso)",
            ));
        }
        let n = buf.len() as u64;
        self.files.insert(rel.as_str().to_owned(), buf);
        Ok(n)
    }
    fn root(&self) -> &std::path::Path {
        Path::new("/mem-sink")
    }
}
