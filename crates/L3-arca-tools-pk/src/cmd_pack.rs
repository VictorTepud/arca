//! `pack`: valida → graph → shas → 7z → firma (spec 25 §3).

use std::io::Cursor;
use std::path::Path;

use arca_pkg_model::Manifest;
use arca_sign::{
    package_digest, PackageSignature, MANIFEST_DIGEST_PATH, MANIFEST_PATH, SIGNATURE_PATH,
};
use arca_types::{ArcaError, Digest, Res};

use crate::{graph, hex32, read_secret, sha256};

/// Empaqueta `src` (dir de proyecto) en `out` (.arca) firmado con `key`.
pub(crate) fn run(src: &Path, out: &Path, key: &Path, backend: &str) -> Res<()> {
    // 1. manifest + validaciones de contenido
    let manifest_path = src.join(MANIFEST_PATH);
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("manifest.toml: {e}"))))?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    for art in manifest.artifacts.values() {
        let f = src.join(art.path.as_str());
        if !f.is_file() {
            return Err(ArcaError::InvalidPackage(
                "pack: artefacto declarado ausente en src/",
            ));
        }
    }
    for font in &manifest.runtime.ui.fonts {
        if !src.join(font.as_str()).is_file() {
            return Err(ArcaError::InvalidPackage(
                "pack: font declarada ausente en src/",
            ));
        }
    }
    // backend: qué binarios deben existir
    let tiene_native = src.join("bin/native-aarch64/app").is_file();
    let tiene_wasm = src.join("bin/wasm/app.wasm").is_file();
    match backend {
        "native" | "auto" | "dual" if !tiene_native && !tiene_wasm => {
            return Err(ArcaError::InvalidPackage(
                "pack: sin binarios (ni native ni wasm)",
            ));
        }
        "wasm" if !tiene_wasm => {
            return Err(ArcaError::InvalidPackage(
                "pack: --backend wasm sin bin/wasm/app.wasm (usa arca-pk compile-wasm en F5)",
            ));
        }
        "native" | "wasm" | "auto" | "dual" => {}
        otro => {
            let msg: &'static str = if otro.is_empty() {
                "pack: backend vacío"
            } else {
                "pack: backend desconocido (native|wasm|auto|dual)"
            };
            return Err(ArcaError::InvalidPackage(msg));
        }
    }

    // 2. graph en sincronía (auto-genera si falta)
    let mmd = src.join("meta/graph.mmd");
    if !mmd.is_file() {
        graph::cmd(src, false)?;
        println!("pack: graph generado (faltaba)");
    } else {
        graph::cmd(src, true)?;
    }

    // 3. shas de artefactos + build.json
    let mut declared: Vec<(String, [u8; 32])> = Vec::new();
    for art in manifest.artifacts.values() {
        let bytes = std::fs::read(src.join(art.path.as_str()))?;
        declared.push((art.path.as_str().to_owned(), sha256(&bytes)));
    }
    declared.sort();
    let manifest_sha = Digest::of(&manifest_bytes).0;

    let mut build_json = String::from("{\"tool\":\"arca-pk\",\"tool_version\":\"");
    build_json.push_str(env!("CARGO_PKG_VERSION"));
    build_json.push_str("\",\"backend\":\"");
    build_json.push_str(backend);
    build_json.push_str("\",\"artifacts\":{");
    let items: Vec<String> = declared
        .iter()
        .map(|(p, h)| format!("\"{p}\":\"{}\"", hex32(h)))
        .collect();
    build_json.push_str(&items.join(","));
    build_json.push_str("}}");

    // 4. recolectar archivos del paquete (orden determinista: manifest
    //    primero por docs/06, resto lexicográfico)
    let mut archivos: Vec<(String, Vec<u8>)> = Vec::new();
    archivos.push((MANIFEST_PATH.to_owned(), manifest_bytes.clone()));
    for base in ["bin", "assets", "icons", "meta"] {
        let dir = src.join(base);
        if !dir.is_dir() {
            continue;
        }
        for e in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_map(|it| it.ok())
        {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let rel = p
                .strip_prefix(src)
                .map_err(|_| ArcaError::Internal("pack: path fuera de src"))?;
            let name = rel.to_string_lossy().replace('\\', "/");
            // se regeneran: nunca empaquetar los del build anterior
            if name == SIGNATURE_PATH || name == MANIFEST_DIGEST_PATH {
                continue;
            }
            let data = std::fs::read(p)?;
            archivos.push((name, data));
        }
    }
    // build.json (regenerado siempre)
    archivos.push(("meta/build.json".to_owned(), build_json.into_bytes()));
    // sort lexicográfico del resto (manifest ya va primero)
    archivos[1..].sort_by(|a, b| a.0.cmp(&b.0));

    // 5. firma (mismo algoritmo que el host: arca-sign)
    let entries_ref: Vec<(&str, [u8; 32])> =
        declared.iter().map(|(p, h)| (p.as_str(), *h)).collect();
    let digest = package_digest(&entries_ref, manifest_sha);
    let sk = read_secret(key)?;
    let psig = PackageSignature::sign(&digest, &sk);
    #[allow(unused_mut)]
    archivos.push((
        MANIFEST_DIGEST_PATH.to_owned(),
        hex32(&manifest_sha).into_bytes(),
    ));
    archivos.push((SIGNATURE_PATH.to_owned(), psig.to_bytes().to_vec()));

    // 6. 7z LZMA2, solid OFF (un push por archivo)
    if let Some(p) = out.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    let file = std::fs::File::create(out)?;
    let mut w = sevenz_rust2::ArchiveWriter::new(file)
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("7z writer: {e}"))))?;
    for (name, data) in &archivos {
        let entry = sevenz_rust2::ArchiveEntry {
            name: name.clone(),
            is_directory: false,
            has_stream: true,
            ..sevenz_rust2::ArchiveEntry::default()
        };
        w.push_archive_entry(entry, Some(Cursor::new(data.clone())))
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("7z push {name}: {e}"))))?;
    }
    w.finish()
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("7z finish: {e}"))))?;

    println!(
        "pack: {} ({} archivos, {}) → digest {}",
        out.display(),
        archivos.len(),
        backend,
        digest
    );
    println!("      firma key_id: {}", psig.key_id);
    Ok(())
}
