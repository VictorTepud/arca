//! backend_for(HostVariant): matriz de elección según ADR-001/ADR-003 y
//! docs/06 §3 ("el host decide según variante"; falla solo si NINGUNO aplica).

mod common;

use arca_pkg_model::{BackendPref, HostVariant, Manifest, ARTIFACT_NATIVE, ARTIFACT_WASM};
use arca_types::ArcaError;
use common::{parse_golden, MINIMAL_NATIVE, MINIMAL_WASM};

#[test]
fn golden_dual_libre_prefiere_native() {
    let m = parse_golden(); // backend_pref = "any"
    let a = m.backend_for(HostVariant::Libre).expect("dual en libre");
    assert_eq!(a.path.as_str(), "bin/native-aarch64/app");
}

#[test]
fn golden_dual_moderno_cae_a_wasm() {
    let m = parse_golden();
    let a = m
        .backend_for(HostVariant::Moderno)
        .expect("dual en moderno");
    assert_eq!(a.path.as_str(), "bin/wasm/app.wasm");
}

#[test]
fn solo_native_libre_ok_moderno_err() {
    let m = Manifest::parse_detailed(MINIMAL_NATIVE.as_bytes()).expect("válido");
    let a = m.backend_for(HostVariant::Libre).expect("native en libre");
    assert_eq!(a.path.as_str(), "bin/native-aarch64/app");
    let e = m
        .backend_for(HostVariant::Moderno)
        .expect_err("moderno no ejecuta nativo");
    assert!(matches!(e, ArcaError::Internal(msg) if msg.contains("ningún artefacto")));
}

#[test]
fn solo_wasm_funciona_en_ambas_variantes() {
    let m = Manifest::parse_detailed(MINIMAL_WASM.as_bytes()).expect("válido");
    for host in [HostVariant::Libre, HostVariant::Moderno] {
        let a = m.backend_for(host).expect("wasm corre en ambos");
        assert_eq!(a.path.as_str(), "bin/wasm/app.wasm");
    }
}

#[test]
fn pref_wasm_en_libre_con_fallback_a_native() {
    // Preferencia blanda (docs/06 §3): pref=wasm pero solo hay native.
    let mut m = parse_golden();
    m.runtime.backend_pref = BackendPref::Wasm;
    m.artifacts.remove(ARTIFACT_WASM);
    let a = m
        .backend_for(HostVariant::Libre)
        .expect("fallback a native");
    assert_eq!(a.path.as_str(), "bin/native-aarch64/app");
}

#[test]
fn pref_native_en_libre_con_fallback_a_wasm() {
    // spec 02 §5: "backend_for falla en host-libre … no: falla si NINGUNO
    // aplica" ⇒ un paquete solo-wasm NO falla en host-libre.
    let mut m = parse_golden();
    m.runtime.backend_pref = BackendPref::Native;
    m.artifacts.remove(ARTIFACT_NATIVE);
    let a = m.backend_for(HostVariant::Libre).expect("fallback a wasm");
    assert_eq!(a.path.as_str(), "bin/wasm/app.wasm");
}

#[test]
fn pref_native_en_moderno_ignora_la_preferencia() {
    // En moderno solo wasm es ejecutable: la preferencia insatisfacible se
    // ignora si hay algo aplicable.
    let mut m = parse_golden();
    m.runtime.backend_pref = BackendPref::Native;
    let a = m.backend_for(HostVariant::Moderno).expect("wasm aplica");
    assert_eq!(a.path.as_str(), "bin/wasm/app.wasm");
}

#[test]
fn pref_explicita_se_respeta_cuando_hay_material() {
    let mut m = parse_golden();
    m.runtime.backend_pref = BackendPref::Wasm;
    let a = m
        .backend_for(HostVariant::Libre)
        .expect("pref wasm satisfecha");
    assert_eq!(a.path.as_str(), "bin/wasm/app.wasm");
    let mut m2 = parse_golden();
    m2.runtime.backend_pref = BackendPref::Native;
    let a2 = m2
        .backend_for(HostVariant::Libre)
        .expect("pref native satisfecha");
    assert_eq!(a2.path.as_str(), "bin/native-aarch64/app");
}

#[test]
fn sin_artefactos_falla_en_ambas_variantes() {
    // Manifest construido a mano (los campos son pub): el modelo permite el
    // estado "sin artefactos" solo vía construcción manual, y backend_for
    // debe fallar limpio (no panic).
    let mut m = parse_golden();
    m.artifacts.clear();
    for host in [HostVariant::Libre, HostVariant::Moderno] {
        assert!(
            m.backend_for(host).is_err(),
            "sin artefactos debe fallar en {host}"
        );
    }
}

#[test]
fn host_variant_can_native() {
    assert!(HostVariant::Libre.can_native());
    assert!(!HostVariant::Moderno.can_native());
    assert_eq!(HostVariant::Libre.to_string(), "libre");
    assert_eq!(HostVariant::Moderno.to_string(), "moderno");
}
