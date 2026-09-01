//! Digest canónico del paquete (spec 08 §3, docs/06 §5).
//!
//! Flujo de bytes hasheado (blake3-256, determinista, O(n)):
//!
//! ```text
//! para cada entrada ORDENADA por path (bytes UTF-8, orden lexicográfico):
//!   b'P' || u32_le(path_len) || path_utf8 || sha256[32]
//! después:
//!   b'M' || manifest_blake3[32]
//! ```
//!
//! La etiqueta `P`/`M` y las longitudes prefijadas evitan colisiones de
//! concatenación. El orden interno garantiza que reordenar los ARCHIVOS del
//! 7z (llegada desordenada) no cambie el digest: **firmar y verificar usan
//! esta MISMA función** (spec 08 §5 fila 1).

use arca_types::Digest;

/// Digest canónico del paquete.
///
/// `entries`: pares (path, sha256-del-contenido) de TODOS los archivos del
/// paquete excepto `meta/signature.bin` (la firma no se firma a sí misma).
/// `manifest_sha`: blake3-256 de `manifest.toml` (`meta/manifest.digest`).
///
/// La función NO confía en el orden de `entries`: los ordena internamente.
#[must_use]
pub fn package_digest(entries: &[(&str, [u8; 32])], manifest_sha: [u8; 32]) -> Digest {
    let mut ordenados: Vec<&(&str, [u8; 32])> = entries.iter().collect();
    ordenados.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut h = blake3::Hasher::new();
    for (path, sha) in ordenados {
        h.update(b"P");
        h.update(&(path.len() as u32).to_le_bytes());
        h.update(path.as_bytes());
        h.update(sha);
    }
    h.update(b"M");
    h.update(&manifest_sha);
    Digest::from_hasher(&h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha_pat(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn determinista_independiente_del_orden_de_entrada() {
        let a = [
            ("bin/native-aarch64/app", sha_pat(1)),
            ("manifest.toml", sha_pat(2)),
            ("meta/graph.mmd", sha_pat(3)),
        ];
        let b = [
            ("meta/graph.mmd", sha_pat(3)),
            ("bin/native-aarch64/app", sha_pat(1)),
            ("manifest.toml", sha_pat(2)),
        ];
        let d1 = package_digest(&a, [9; 32]);
        let d2 = package_digest(&b, [9; 32]);
        assert_eq!(d1, d2);
    }

    #[test]
    fn cambia_ante_toda_mutacion_relevante() {
        let base = [("a.txt", sha_pat(1)), ("b.bin", sha_pat(2))];
        let m = [7; 32];
        let d0 = package_digest(&base, m);
        // sha mutado
        let d1 = package_digest(&[("a.txt", sha_pat(9)), ("b.bin", sha_pat(2))], m);
        // path mutado
        let d2 = package_digest(&[("a.txt", sha_pat(1)), ("c.bin", sha_pat(2))], m);
        // entrada extra
        let d3 = package_digest(
            &[
                ("a.txt", sha_pat(1)),
                ("b.bin", sha_pat(2)),
                ("x", sha_pat(3)),
            ],
            m,
        );
        // entrada faltante
        let d4 = package_digest(&[("a.txt", sha_pat(1))], m);
        // manifest mutado
        let d5 = package_digest(&base, [8; 32]);
        for d in [d1, d2, d3, d4, d5] {
            assert_ne!(d, d0);
        }
    }

    #[test]
    fn sin_colisiones_de_concatenacion() {
        // Dos listas distintas cuyo dump ingenuo concatenado podría coincidir:
        // la etiqueta + longitud prefijada lo impide.
        let x = [("ab", sha_pat(1))];
        let y = [("a", [0x62; 32])]; // 'b' repetido = sha distinta, path distinto
        assert_ne!(package_digest(&x, [0; 32]), package_digest(&y, [0; 32]));
    }
}
