//! Sonda de capacidades de `sevenz-rust2` (riesgo **R-04**, spec 09 §4).
//!
//! El contenedor v1 empaqueta con LZMA2 plano (docs/06 §6); esta sonda
//! informa qué sabe **decodificar** la build actual para que `tools-pk` y
//! el host sepan si pueden subir a level 9 + filtro ARM64 (7-Zip 23+) o
//! deben reempaquetar (spec 09 §5: "unsupported filter").
//!
//! La matriz se evalúa en compile-time contra los feature flags que ESTE
//! crate activa en `sevenz-rust2` (los codecs extra son opt-in vía
//! features de `arca-7z`: `deflate`, `brotli`, `lz4`, `zstd`).

/// Soporte de un codec o filtro concreto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecSupport {
    /// Nombre canónico (igual que `sevenz_rust2::archive::EncoderMethod`).
    pub name: &'static str,
    /// ¿Se puede decodificar?
    pub decode: bool,
    /// ¿Se puede comprimir (solo relevante para tools-pk)?
    pub encode: bool,
}

/// Resultado de [`probe_features`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Features {
    /// Codecs de compresión soportados.
    pub codecs: Vec<CodecSupport>,
    /// Filtros BCJ/DELTA soportados (aplicables antes/después del codec).
    pub filters: Vec<CodecSupport>,
    /// ¿AES256-SHA256 (cifrado 7z) se puede decodificar? (v1: NO se usa,
    /// docs/06 §6: la seguridad real es la firma ed25519).
    pub aes256: bool,
    /// ¿Cifrado de cabecera soportado en decode?
    pub header_encryption: bool,
    /// ¿LZMA2 multi-hilo en decode? (nota: `Archive::extract` lo fija a 1
    /// para acotar memoria).
    pub multithreaded_lzma2: bool,
    /// Notas operativas para el registro de riesgos (R-04).
    pub notes: Vec<&'static str>,
}

const NOTA_MEMORIA: &str = "la memoria de decode LZMA/LZMA2 es \u{2248} el diccionario del paquete (level 9 = 64 MiB): paquetes v1 usan level 6 (8 MiB)";
const NOTA_BCJ2: &str = "BCJ2: decode s\u{ed}, compresi\u{f3}n no (sevenz-rust2)";
const NOTA_CORPUS: &str = "R-04: BCJ ARM64/BCJ2/RISC-V declarados por el crate pero sin corpus real en T05 (sin binario 7-Zip en el entorno; ver worklog)";
const NOTA_V1: &str = "v1 del contenedor: LZMA2 level 6, solid OFF, sin filtros (docs/06 \u{a7}6)";

/// Soporte de un método siempre presente (sin feature gate).
fn on(name: &'static str) -> CodecSupport {
    CodecSupport {
        name,
        decode: true,
        encode: true,
    }
}

/// Soporte de un método de solo decode.
fn decode_only(name: &'static str) -> CodecSupport {
    CodecSupport {
        name,
        decode: true,
        encode: false,
    }
}

/// Reporta qué codecs y filtros soporta la build actual de `sevenz-rust2`.
///
/// Es la entrada directa del riesgo R-04: si un paquete futuro usa un
/// codec/filtro con `decode: false`, el installer verá
/// `unsupported compression method` (spec 09 §5) y `tools-pk` tendrá que
/// reempaquetar.
pub fn probe_features() -> Features {
    let mut codecs = vec![on("COPY"), on("LZMA"), on("LZMA2"), on("BZIP2"), on("PPMD")];
    let filters = vec![
        on("BCJ_X86"),
        on("BCJ_PPC"),
        on("BCJ_IA64"),
        on("BCJ_ARM"),
        on("BCJ_ARM64"),
        on("BCJ_ARM_THUMB"),
        on("BCJ_SPARC"),
        on("BCJ_RISCV"),
        on("DELTA"),
        decode_only("BCJ2"),
    ];
    // Codecs opt-in (features de arca-7z passthrough a sevenz-rust2).
    for (name, flag) in [
        ("DEFLATE", cfg!(feature = "deflate")),
        ("BROTLI", cfg!(feature = "brotli")),
        ("LZ4", cfg!(feature = "lz4")),
        ("ZSTD", cfg!(feature = "zstd")),
    ] {
        codecs.push(CodecSupport {
            name,
            decode: flag,
            encode: flag,
        });
    }

    Features {
        codecs,
        filters,
        aes256: true,            // feature default de sevenz-rust2
        header_encryption: true, // decode de cabecera cifrada: soportado
        multithreaded_lzma2: true,
        notes: vec![NOTA_MEMORIA, NOTA_BCJ2, NOTA_CORPUS, NOTA_V1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nucleo_lzma2_siempre_presente() {
        let f = probe_features();
        let lzma2 = f
            .codecs
            .iter()
            .find(|c| c.name == "LZMA2")
            .unwrap_or_else(|| panic!("LZMA2 debe existir"));
        assert!(lzma2.decode && lzma2.encode);
        // BCJ2: decode sí, encode no.
        let bcj2 = f
            .filters
            .iter()
            .find(|c| c.name == "BCJ2")
            .unwrap_or_else(|| panic!("BCJ2 debe existir"));
        assert!(bcj2.decode && !bcj2.encode);
        // ARM64 decodificable (F1 decide si activarlo al empaquetar).
        assert!(f.filters.iter().any(|c| c.name == "BCJ_ARM64" && c.decode));
    }

    #[test]
    fn optin_refleja_features_de_build() {
        let f = probe_features();
        let find = |n: &str| f.codecs.iter().find(|c| c.name == n).unwrap().decode;
        assert_eq!(find("DEFLATE"), cfg!(feature = "deflate"));
        assert_eq!(find("ZSTD"), cfg!(feature = "zstd"));
        assert_eq!(find("BROTLI"), cfg!(feature = "brotli"));
        assert_eq!(find("LZ4"), cfg!(feature = "lz4"));
    }

    #[test]
    fn notas_de_riesgo_presentes() {
        let f = probe_features();
        assert!(!f.notes.is_empty());
        assert!(f.notes.iter().any(|n| n.contains("R-04")));
        assert!(f.aes256, "AES256 es feature default de sevenz-rust2");
    }
}
