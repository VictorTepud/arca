//! Tests de parseo del manifest (spec 02 §4/§6): golden completo, mínimos
//! válidos, BOM, límite de tamaño, y tabla de fixtures malformados (≥ 25
//! clases de error distintas, exigencia de la spec y de TASKS.json T04).

mod common;

use std::collections::BTreeMap;

use arca_pkg_model::{Manifest, MAX_MANIFEST_BYTES};
use arca_types::{ArcaError, Capability};
use common::{read_fixture, read_malformed, GOLDEN, MINIMAL_NATIVE, MINIMAL_WASM};
use semver::Version;

/// (fixture, clase esperada). La cobertura de clases es exhaustiva del
/// universo de errores de parse v1: 56 fixtures ≥ 25 exigidos.
const MALFORMED: &[(&str, &str)] = &[
    // secciones/campos ausentes
    ("m01_empty.toml", "MissingSection"),
    ("m02_no_package_section.toml", "MissingSection"),
    ("m03_pkg_missing_id.toml", "MissingField"),
    ("m07_pkg_missing_name.toml", "MissingField"),
    ("m10_pkg_missing_version.toml", "MissingField"),
    ("m13_pkg_missing_min_host.toml", "MissingField"),
    ("m15_pkg_missing_api_level.toml", "MissingField"),
    ("m19_missing_runtime.toml", "MissingSection"),
    ("m23_missing_profile.toml", "MissingSection"),
    ("m26_no_artifacts.toml", "NoArtifacts"),
    ("m28_artifact_missing_path.toml", "MissingField"),
    ("m33_artifact_missing_sha256.toml", "MissingField"),
    // package
    ("m04_id_uppercase.toml", "BadAppId"),
    ("m05_id_too_short.toml", "BadAppId"),
    ("m06_id_dash.toml", "BadAppId"),
    ("m08_name_empty.toml", "BadName"),
    ("m09_name_decomposed.toml", "BadName"),
    ("m11_version_not_semver.toml", "BadSemver"),
    ("m12_version_leading_zero.toml", "BadSemver"),
    ("m14_min_host_future.toml", "HostTooOld"),
    ("m16_api_level_zero.toml", "UnsupportedApiLevel"),
    ("m17_api_level_future.toml", "UnsupportedApiLevel"),
    ("m18_api_level_string.toml", "TomlType"),
    // runtime
    ("m20_bad_backend_pref.toml", "BadEnum"),
    ("m21_entry_empty.toml", "BadEntry"),
    ("m22_bad_respawn.toml", "BadEnum"),
    ("m39_unknown_capability.toml", "BadCapability"),
    ("m40_capability_kebab_style.toml", "BadCapability"),
    ("m41_ui_windows_invalid.toml", "BadEnum"),
    ("m42_ui_atlas_not_pow2.toml", "OutOfRange"),
    ("m43_font_path_dotdot.toml", "BadFont"),
    ("m44_font_outside_assets.toml", "BadFont"),
    ("m53_sync_string.toml", "TomlType"),
    ("m54_perms_string_not_array.toml", "TomlType"),
    // artifacts
    ("m27_artifact_bad_key.toml", "BadArtifact"),
    ("m29_artifact_path_absolute.toml", "BadPath"),
    ("m30_artifact_path_dotdot.toml", "BadPath"),
    ("m31_artifact_path_outside_bin.toml", "BadArtifact"),
    ("m32_artifact_path_backslash.toml", "BadPath"),
    ("m34_sha256_not_hex.toml", "BadSha256"),
    ("m35_sha256_short.toml", "BadSha256"),
    ("m36_wasm_wrong_suffix.toml", "BadArtifact"),
    ("m37_native_wrong_dir.toml", "BadArtifact"),
    ("m38_duplicate_artifact_path.toml", "DuplicateArtifactPath"),
    ("m51_wasm_runtime_invalid.toml", "BadEnum"),
    ("m52_aot_wrong_location.toml", "BadArtifact"),
    // profile
    ("m24_budget_zero.toml", "OutOfRange"),
    ("m25_frame_huge.toml", "OutOfRange"),
    // metadatos
    ("m50_tag_uppercase.toml", "BadTag"),
    ("m56_authors_number.toml", "TomlType"),
    ("m59_description_too_long.toml", "BadDescription"),
    ("m60_author_too_long.toml", "BadAuthor"),
    // campos desconocidos (manifest de api_level futura, spec 02 §5)
    ("m45_unknown_root_field.toml", "UnknownField"),
    ("m46_unknown_package_field.toml", "UnknownField"),
    ("m47_unknown_runtime_field.toml", "UnknownField"),
    ("m48_unknown_profile_field.toml", "UnknownField"),
    // sintaxis
    ("m49_duplicate_key.toml", "TomlSyntax"),
    ("m55_not_toml_at_all.toml", "TomlSyntax"),
    // tamaño / encoding
    ("m57_oversize.toml", "TooLarge"),
    ("m58_not_utf8.toml", "NotUtf8"),
];

#[test]
fn golden_completo_parsea_y_tipa() {
    let m = common::parse_golden();

    // [package]
    assert_eq!(m.package.id.as_str(), "dev.misapps.teclado");
    assert_eq!(m.package.name, "Mi Teclado Pro");
    assert_eq!(m.package.version, Version::new(1, 2, 0));
    assert_eq!(m.package.min_host, Version::new(1, 0, 0));
    assert_eq!(m.package.api_level, 1);
    assert_eq!(m.package.authors, ["tú <tu@correo>"]);
    assert_eq!(m.package.description, "Un teclado estadístico");
    assert_eq!(m.package.tags, ["tools"]);

    // [runtime]
    use arca_pkg_model::{BackendPref, RespawnPolicy, WindowsMode};
    assert_eq!(m.runtime.backend_pref, BackendPref::Any);
    assert_eq!(m.runtime.entry, "app");
    assert_eq!(m.runtime.respawn, RespawnPolicy::OnCrash);
    assert!(!m.runtime.ui.sync);
    assert_eq!(m.runtime.ui.windows, WindowsMode::Single);
    assert_eq!(m.runtime.ui.atlas, 2048);
    assert_eq!(m.runtime.ui.fonts.len(), 1);
    assert_eq!(m.runtime.ui.fonts[0].as_str(), "assets/fonts/inter.ttf");
    assert_eq!(
        m.requested_caps(),
        &[Capability::NetClient, Capability::ClipboardWrite]
    );

    // [artifacts]: dual + extras
    assert_eq!(m.artifacts.len(), 2);
    let native = m.native().expect("golden trae native");
    assert_eq!(native.path.as_str(), "bin/native-aarch64/app");
    assert_eq!(native.sha256_hex(), "ab".repeat(32));
    assert!(native.extra.is_empty());
    let wasm = m.wasm().expect("golden trae wasm");
    assert_eq!(wasm.path.as_str(), "bin/wasm/app.wasm");
    assert_eq!(wasm.sha256_hex(), "cd".repeat(32));
    assert_eq!(
        wasm.extra.get("aot").map(String::as_str),
        Some("bin/wasm/app.aot")
    );
    assert_eq!(
        wasm.aot_path().map(|p| p.as_str().to_owned()),
        Some("bin/wasm/app.aot".to_owned())
    );
    assert_eq!(
        wasm.extra.get("wasm_runtime").map(String::as_str),
        Some("wamr-aot")
    );

    // [profile]
    assert_eq!(m.profile.launch_budget_ms, 120);
    assert_eq!(m.profile.max_frame_mb, 2);

    // Equivalencia con el tipo de la spec: BTreeMap<String, Artifact>.
    let _: &BTreeMap<String, arca_pkg_model::Artifact> = &m.artifacts;
}

#[test]
fn minimales_validos_parsean() {
    let n = Manifest::parse_detailed(MINIMAL_NATIVE.as_bytes()).expect("minimal native válido");
    assert!(n.native().is_some());
    assert!(n.wasm().is_none());
    // defaults de ui cuando falta la sección entera:
    assert!(!n.runtime.ui.sync);
    assert_eq!(n.runtime.ui.atlas, 2048);
    assert!(n.runtime.ui.fonts.is_empty());
    assert!(n.requested_caps().is_empty()); // fail-closed: sin perms ⇒ sin caps

    let w = Manifest::parse_detailed(MINIMAL_WASM.as_bytes()).expect("minimal wasm válido");
    assert!(w.native().is_none());
    assert!(w.wasm().is_some());
    assert!(w.wasm().expect("wasm").aot_path().is_none());
}

#[test]
fn bom_utf8_se_descarta() {
    let bytes = read_fixture("golden_bom.toml");
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    let m = Manifest::parse_detailed(&bytes).expect("BOM + golden válido");
    assert_eq!(m.package.name, "Mi Teclado Pro");
}

#[test]
fn manifest_no_utf8_se_rechaza() {
    // 0xFF 0xFE parece un BOM UTF-16 (síntoma real de editores de Windows).
    let bad = [
        0xFF, 0xFE, b'[', b'p', b'a', b'c', b'k', b'a', b'g', b'e', b']',
    ];
    let e = Manifest::parse_detailed(&bad).expect_err("no es UTF-8");
    assert_eq!(e.kind(), "NotUtf8");
}

#[test]
fn manifest_oversize_se_rechaza_como_frame_overflow() {
    let mut big = GOLDEN.as_bytes().to_vec();
    big.extend(std::iter::repeat_n(b'\n', MAX_MANIFEST_BYTES));
    assert!(big.len() > MAX_MANIFEST_BYTES);
    let e = Manifest::parse_detailed(&big).expect_err("demasiado grande");
    assert_eq!(e.kind(), "TooLarge");
    // La versión canónica (Res) mapea al FrameOverflow del ecosistema:
    let canonical = Manifest::parse(&big).expect_err("parse() también falla");
    assert!(matches!(
        canonical,
        ArcaError::FrameOverflow {
            bytes,
            limit: MAX_MANIFEST_BYTES
        } if bytes == big.len()
    ));
}

#[test]
fn manifests_malformados_son_rechazados_por_clase() {
    assert!(
        MALFORMED.len() >= 25,
        "spec exige ≥25, hay {}",
        MALFORMED.len()
    );
    for (file, want) in MALFORMED {
        let bytes = read_malformed(file);
        let e = Manifest::parse_detailed(&bytes)
            .expect_err(&format!("{file} debía ser rechazado (parseó bien)"));
        assert_eq!(e.kind(), *want, "{file}: {e}");
    }
}

#[test]
fn clases_de_error_son_distintas() {
    // ≥ 25 CLASES distintas (no solo 25 archivos): requisito literal de la spec.
    let clases: std::collections::HashSet<&str> = MALFORMED.iter().map(|(_, k)| *k).collect();
    assert!(clases.len() >= 25, "clases distintas: {}", clases.len());
}

#[test]
fn parse_es_total_sobre_entradas_grotescas() {
    // Espec 02 §4: cualquier input → ArcaError descriptivo, jamás pánico.
    let entradas: &[&[u8]] = &[
        b"",
        b"\x00\x00\x00",
        b"[",
        b"=",
        b"[[[[[[",
        b"\xFF\xFF\xFF\xFF",
        b"[package]\n",
        b"\xEF\xBB\xBF",
        &[0xC3, 0x28], // UTF-8 roto
        b"package = { id = 1 }",
    ];
    for input in entradas {
        let r = Manifest::parse(input);
        assert!(r.is_err(), "{input:?} debía fallar, no parsear");
    }
}
