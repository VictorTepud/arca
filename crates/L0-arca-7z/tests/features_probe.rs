//! Sonda R-04 en contexto de integración: imprime la matriz soportada por
//! la build de test (features `full`) y valida invariantes del reporte.

mod common;

use arca_7z::probe_features;

#[test]
fn reporte_de_features_r04() {
    let f = probe_features();
    let mut report = String::from("probe_features() [build de test, features full]:\n");
    for c in &f.codecs {
        report.push_str(&format!(
            "  codec  {:10} decode={} encode={}\n",
            c.name, c.decode, c.encode
        ));
    }
    for c in &f.filters {
        report.push_str(&format!(
            "  filter {:10} decode={} encode={}\n",
            c.name, c.decode, c.encode
        ));
    }
    report.push_str(&format!(
        "  aes256={} header_encryption={} mt_lzma2={}\n",
        f.aes256, f.header_encryption, f.multithreaded_lzma2
    ));
    for n in &f.notes {
        report.push_str(&format!("  nota: {n}\n"));
    }
    println!("{report}");

    // La build de test activa todos los opt-in (dev-dep selftest/full).
    for nombre in ["DEFLATE", "BROTLI", "LZ4", "ZSTD"] {
        let c = f.codecs.iter().find(|c| c.name == nombre).unwrap();
        assert!(c.decode, "{nombre} debe estar activo en la build de test");
    }
    // BCJ2 siempre decode-solo.
    let bcj2 = f.filters.iter().find(|c| c.name == "BCJ2").unwrap();
    assert!(bcj2.decode && !bcj2.encode);
    // El riesgo R-04 deja nota explícita.
    assert!(f.notes.iter().any(|n| n.contains("R-04")));
}
