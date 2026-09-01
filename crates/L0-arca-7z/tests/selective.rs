//! Extracción selectiva (spec 09 §3: "bin/ antes que assets/" para arranque
//! más rápido) y post-condiciones del plan.

mod common;

use std::fs;

use arca_7z::{Archive, DirSink, ExtractPlan};
use common::{corpus_dir, digest_of, walk_files};

/// Abre pkg_layout.7z (layout docs/06 §2) y extrae solo bin/ + manifest.
#[test]
fn selectivo_bin_y_manifest_deja_assets_sin_tocar() {
    let path = corpus_dir().join("pkg_layout.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    // py7zr genera bloques SÓLIDOS: las entradas no deseadas se drenan
    // (y su CRC se verifica igualmente) sin escribirse.
    let plan = ExtractPlan::parse(&["bin", "manifest.toml"]).expect("plan");
    archive
        .extract(&plan, &mut sink, &mut progreso)
        .expect("extract");

    // Escritos: manifest.toml + los dos binarios bajo bin/.
    assert!(root.join("manifest.toml").is_file());
    assert!(root.join("bin/native-aarch64/app").is_file());
    assert!(root.join("bin/wasm/app.wasm").is_file());
    // NO escritos (selectiva real):
    assert!(!root.join("assets").exists(), "assets/ no debe existir");
    assert!(!root.join("icons").exists(), "icons/ no debe existir");
    assert!(!root.join("meta").exists(), "meta/ no debe existir");

    // Total: exactamente 3 archivos.
    let files = walk_files(&root);
    assert_eq!(files.len(), 3, "{files:?}");

    // Contenido correcto de lo extraído (blake3 vs fuentes del corpus).
    assert_eq!(
        digest_of(&root.join("bin/native-aarch64/app")),
        digest_of(&corpus_dir().join("src/pkg/bin/native-aarch64/app"))
    );
    assert_eq!(
        digest_of(&root.join("manifest.toml")),
        digest_of(&corpus_dir().join("src/pkg/manifest.toml"))
    );
}

/// El mismo paquete extraído COMPLETO después (instalación en dos fases).
#[test]
fn selectivo_y_luego_completo() {
    let path = corpus_dir().join("pkg_layout.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");

    let tmp = tempfile::tempdir().expect("tempdir");
    let root1 = tmp.path().join("fase1");
    let root2 = tmp.path().join("fase2");

    let mut sink = DirSink::new(root1.clone());
    let mut progreso = |_f: f64| {};
    archive
        .extract(
            &ExtractPlan::parse(&["manifest.toml"]).unwrap(),
            &mut sink,
            &mut progreso,
        )
        .expect("fase 1");
    assert!(root1.join("manifest.toml").is_file());
    assert_eq!(walk_files(&root1).len(), 1);

    let mut sink2 = DirSink::new(root2.clone());
    let mut progreso2 = |_f: f64| {};
    archive
        .extract(&ExtractPlan::all(), &mut sink2, &mut progreso2)
        .expect("fase 2");
    // El layout completo de docs/06:
    for esperado in [
        "manifest.toml",
        "bin/native-aarch64/app",
        "bin/wasm/app.wasm",
        "assets/fonts/inter.ttf",
        "icons/icon-192.png",
        "icons/icon-512.png",
        "meta/graph.mmd",
        "meta/build.json",
    ] {
        assert!(root2.join(esperado).is_file(), "falta {esperado}");
    }
}

/// wanted de un archivo concreto en un archivo sólido de 64 entradas.
#[test]
fn selectivo_un_solo_archivo_de_64() {
    let path = corpus_dir().join("many_files.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    let plan = ExtractPlan::parse(&["f07.bin"]).expect("plan");
    archive
        .extract(&plan, &mut sink, &mut progreso)
        .expect("extract");
    let files = walk_files(&root);
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        digest_of(&root.join("f07.bin")),
        digest_of(&corpus_dir().join("src/many/f07.bin"))
    );
}

/// Un wanted que no existe en el paquete → error (paquete corrupto/falso).
#[test]
fn wanted_ausente_es_error() {
    let path = corpus_dir().join("pkg_layout.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut sink = DirSink::new(tmp.path().join("out"));
    let mut progreso = |_f: f64| {};
    let plan = ExtractPlan::parse(&["no/existe.txt"]).expect("plan");
    let res = archive.extract(&plan, &mut sink, &mut progreso);
    assert!(res.is_err(), "wanted ausente debía ser error");
}

/// entries() + safe_path(): el caller puede construir planes desde el
/// listado sin confianza en los nombres crudos.
#[test]
fn entries_expone_safe_path_para_planes() {
    let path = corpus_dir().join("pkg_layout.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let archive = Archive::open(&mut file).expect("open");
    let entries = archive.entries().expect("entries");
    let bin = entries
        .iter()
        .find(|e| e.path == "bin/native-aarch64/app")
        .expect("entrada bin");
    assert_eq!(bin.safe_path().unwrap().as_str(), "bin/native-aarch64/app");
    assert_eq!(bin.size, 64_016); // pseudo-ELF del generador (16+64000)
    assert!(bin.crc.is_some());
    assert!(!bin.is_dir);
}
