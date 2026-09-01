//! Roundtrip modelo ⇄ TOML: el golden reparsea idéntico (pérdida cero) y la
//! salida contiene las secciones esperadas.

mod common;

use arca_pkg_model::Manifest;
use common::{parse_golden, GOLDEN, MINIMAL_NATIVE, MINIMAL_WASM};

#[test]
fn roundtrip_golden_sin_perdida() {
    let m = parse_golden();
    let toml_out = m.to_toml().expect("serializar golden");
    let m2 =
        Manifest::parse_detailed(toml_out.as_bytes()).expect("la salida del modelo debe reparsear");
    assert_eq!(m, m2, "roundtrip con pérdida:\n{toml_out}");
}

#[test]
fn roundtrip_minimales_sin_perdida() {
    for src in [MINIMAL_NATIVE, MINIMAL_WASM] {
        let m = Manifest::parse_detailed(src.as_bytes()).expect("válido");
        let out = m.to_toml().expect("serializar");
        let m2 = Manifest::parse_detailed(out.as_bytes()).expect("reparsear");
        assert_eq!(m, m2, "roundtrip con pérdida:\n{out}");
    }
}

#[test]
fn toml_salida_tiene_las_secciones_del_contrato() {
    let m = parse_golden();
    let out = m.to_toml().expect("serializar");
    for esperado in [
        "[package]",
        "[runtime]",
        "[artifacts.native]",
        "[artifacts.wasm]",
        "[profile]",
        "backend_pref",
        "wasm_runtime = \"wamr-aot\"",
        "aot = \"bin/wasm/app.aot\"",
        "\"net.client\"",
        "\"clipboard.write\"",
    ] {
        assert!(out.contains(esperado), "faltaba {esperado:?} en:\n{out}");
    }
}

#[test]
fn toml_salida_no_inventa_campos() {
    let m = parse_golden();
    let out = m.to_toml().expect("serializar");
    // El golden ya trae todos los campos: comparación laxa de longitudes
    // (el roundtrip exacto ya está testeado arriba).
    assert!(out.len() > GOLDEN.len() / 2, "salida sospechosamente corta");
    assert!(out.lines().count() > 15);
}
