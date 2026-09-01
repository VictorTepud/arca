//! validate_layout (spec 02 §6): entradas extra/faltantes/`..`/absolutas/
//! symlink → error. Golden completo pasa.

mod common;

use arca_pkg_model::PkgError;
use arca_pkg_model::{ArchiveEntries, ArchiveEntry, EntryKind};
use arca_types::ArcaError;
use common::{golden_entries, golden_entries_without, parse_golden, MINIMAL_NATIVE};

fn layout_err(m: &arca_pkg_model::Manifest, e: &ArchiveEntries) -> PkgError {
    m.validate_layout_detailed(e)
        .expect_err("el layout debía fallar")
}

#[test]
fn layout_golden_ok() {
    let m = parse_golden();
    let r = m.validate_layout(&golden_entries());
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn layout_minimal_native_ok() {
    let m = arca_pkg_model::Manifest::parse_detailed(MINIMAL_NATIVE.as_bytes())
        .expect("minimal válido");
    let entries = ArchiveEntries::from_paths(["manifest.toml", "bin/native-aarch64/app"]);
    assert!(m.validate_layout(&entries).is_ok());
}

#[test]
fn layout_extra_en_raiz() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("README", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutExtra");
}

#[test]
fn layout_extra_en_directorio_desconocido() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("etc/passwd", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutExtra");
}

#[test]
fn layout_bin_no_declarado() {
    // Regla "nada ejecutable de propina": bin bajo bin/ sin sha256 en el
    // manifest ⇒ rechazo.
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("bin/native-aarch64/helper", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutUndeclaredBin");
}

#[test]
fn layout_manifest_toml_ausente() {
    let m = parse_golden();
    let e = golden_entries_without("manifest.toml");
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutNoManifest");
}

#[test]
fn layout_manifest_toml_es_directorio() {
    let m = parse_golden();
    let e = golden_entries_without("manifest.toml");
    let mut e2 = e;
    e2.push("manifest.toml/", EntryKind::Dir);
    let err = layout_err(&m, &e2);
    assert_eq!(err.kind(), "LayoutNoManifest");
}

#[test]
fn layout_entradas_vacias() {
    let m = parse_golden();
    let e = ArchiveEntries::new();
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutNoManifest");
}

#[test]
fn layout_artefacto_faltante() {
    let m = parse_golden();
    let e = golden_entries_without("app.wasm");
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutMissing");
}

#[test]
fn layout_aot_faltante() {
    let m = parse_golden();
    let e = golden_entries_without("app.aot");
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutMissing");
}

#[test]
fn layout_fuente_faltante() {
    let m = parse_golden();
    let e = golden_entries_without("inter.ttf");
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutMissing");
}

#[test]
fn layout_symlink_prohibido() {
    let m = parse_golden();
    let e = golden_entries_without("app.wasm");
    let mut e2 = e;
    e2.push("bin/wasm/app.wasm", EntryKind::Symlink);
    let err = layout_err(&m, &e2);
    assert_eq!(err.kind(), "LayoutSymlink");
}

#[test]
fn layout_entry_con_dotdot() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("../evil", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutBadPath");
}

#[test]
fn layout_entry_absoluta() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("/etc/passwd", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutBadPath");
}

#[test]
fn layout_entry_backslash() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("assets\\fonts\\evil.ttf", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutBadPath");
}

#[test]
fn layout_entry_doble_slash() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("assets//evil", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutBadPath");
}

#[test]
fn layout_entry_nul() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("assets/evi\u{0}l", EntryKind::File);
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutBadPath");
}

#[test]
fn layout_entry_duplicada() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("icons/icon-192.png", EntryKind::File); // ya existe arriba
    let err = layout_err(&m, &e);
    assert_eq!(err.kind(), "LayoutDuplicate");
}

#[test]
fn layout_version_canonica_es_arcaerror() {
    let m = parse_golden();
    let mut e = golden_entries();
    e.push("../evil", EntryKind::File);
    let r = m.validate_layout(&e);
    assert!(matches!(r, Err(ArcaError::Internal(_))));
}

#[test]
fn layout_entries_mezcladas_con_dirs_normales() {
    // El listing real de un 7z trae dirs con '/' final: la normalización
    // (ArchiveEntry::new) los colapsa y el layout pasa.
    let m = parse_golden();
    let e = ArchiveEntries::from_entries([
        ArchiveEntry::new("manifest.toml", EntryKind::File),
        ArchiveEntry::new("bin/", EntryKind::Dir),
        ArchiveEntry::new("bin/native-aarch64/", EntryKind::Dir),
        ArchiveEntry::new("bin/native-aarch64/app", EntryKind::File),
        ArchiveEntry::new("bin/wasm/", EntryKind::Dir),
        ArchiveEntry::new("bin/wasm/app.wasm", EntryKind::File),
        ArchiveEntry::new("bin/wasm/app.aot", EntryKind::File),
        ArchiveEntry::new("assets/", EntryKind::Dir),
        ArchiveEntry::new("assets/fonts/", EntryKind::Dir),
        ArchiveEntry::new("assets/fonts/inter.ttf", EntryKind::File),
        ArchiveEntry::new("icons/", EntryKind::Dir),
        ArchiveEntry::new("meta/", EntryKind::Dir),
    ]);
    assert!(m.validate_layout(&e).is_ok());
}
