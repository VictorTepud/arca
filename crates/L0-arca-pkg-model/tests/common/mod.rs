//! Helpers compartidos por los tests de integración de `arca-pkg-model`.
#![allow(dead_code)] // se compila en cada binario de test; no todo se usa en todos

use std::fs;
use std::path::PathBuf;

use arca_pkg_model::{ArchiveEntries, EntryKind, Manifest};

/// Manifest golden completo (docs/06 §3), embebido en compile-time.
pub const GOLDEN: &str = include_str!("../fixtures/golden_manifest.toml");
/// Manifest golden mínimo solo-native.
pub const MINIMAL_NATIVE: &str = include_str!("../fixtures/minimal_native.toml");
/// Manifest golden mínimo solo-wasm.
pub const MINIMAL_WASM: &str = include_str!("../fixtures/minimal_wasm.toml");

/// Parsea el golden (falla el test si el golden deja de ser válido).
pub fn parse_golden() -> Manifest {
    Manifest::parse_detailed(GOLDEN.as_bytes()).expect("el golden debe parsear")
}

/// Raíz de fixtures (para leer en runtime los malformados, que son muchos).
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Lee un fixture de `tests/fixtures/` (bytes crudos).
pub fn read_fixture(rel: &str) -> Vec<u8> {
    let p = fixtures_dir().join(rel);
    fs::read(&p).unwrap_or_else(|e| panic!("leyendo fixture {p:?}: {e}"))
}

/// Lee un fixture malformado de `tests/fixtures/malformed/`.
pub fn read_malformed(name: &str) -> Vec<u8> {
    let p = fixtures_dir().join("malformed").join(name);
    fs::read(&p).unwrap_or_else(|e| panic!("leyendo fixture {p:?}: {e}"))
}

/// Listing del golden completo (docs/06 §2): layout dual con dirs, assets,
/// icons y meta.
pub fn golden_entries() -> ArchiveEntries {
    let mut e = ArchiveEntries::new();
    e.push("manifest.toml", EntryKind::File);
    e.push("bin/", EntryKind::Dir);
    e.push("bin/native-aarch64/", EntryKind::Dir);
    e.push("bin/native-aarch64/app", EntryKind::File);
    e.push("bin/wasm/", EntryKind::Dir);
    e.push("bin/wasm/app.wasm", EntryKind::File);
    e.push("bin/wasm/app.aot", EntryKind::File);
    e.push("assets/", EntryKind::Dir);
    e.push("assets/fonts/", EntryKind::Dir);
    e.push("assets/fonts/inter.ttf", EntryKind::File);
    e.push("icons/", EntryKind::Dir);
    e.push("icons/icon-192.png", EntryKind::File);
    e.push("icons/icon-512.png", EntryKind::File);
    e.push("meta/", EntryKind::Dir);
    e.push("meta/graph.mmd", EntryKind::File);
    e.push("meta/signature.bin", EntryKind::File);
    e.push("meta/manifest.digest", EntryKind::File);
    e.push("meta/build.json", EntryKind::File);
    e
}

/// `golden_entries()` sin las entradas cuyo path contiene `needle`.
pub fn golden_entries_without(needle: &str) -> ArchiveEntries {
    let filtered = golden_entries()
        .iter()
        .filter(|e| !e.path().contains(needle))
        .cloned()
        .collect::<Vec<_>>();
    ArchiveEntries::from_entries(filtered)
}
