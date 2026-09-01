//! Helpers compartidos por los tests de integración de `arca-7z`.
//!
//! El corpus estático (`tests/corpus/`, generado por `gen_corpus.py` con
//! py7zr 1.1.3) se describe en `manifest.txt` con un formato lineal
//! intencionadamente simple (evita meter serde_json en el crate):
//!
//! ```text
//! ARCHIVE <archivo> <ok|fail|big> # descripción
//! DIR <nombre de entrada de directorio>
//! ENTRY <arcname> <ruta src relativa> <tamaño> <sha256 del src>
//! ```

#![allow(dead_code)] // helpers compartidos: no todo se usa en cada binario

use std::fs;
use std::path::PathBuf;

/// Una entrada de archivo esperada en el manifest.
pub struct ExpectedEntry {
    /// Nombre de la entrada dentro del 7z.
    pub arcname: String,
    /// Ruta del archivo fuente (bajo `corpus/`).
    pub src: PathBuf,
    /// Tamaño esperado en bytes.
    pub size: u64,
}

/// Un archivo del corpus con sus expectativas.
pub struct CorpusArchive {
    /// Nombre del archivo .7z.
    pub file: String,
    /// Modo: `ok` (extraer y verificar), `fail` (pipeline falla), `big`
    /// (test de memoria aparte).
    pub mode: String,
    /// Directorios esperados (entradas is_dir).
    pub dirs: Vec<String>,
    /// Archivos esperados.
    pub entries: Vec<ExpectedEntry>,
}

/// Directorio del corpus (estático, junto a los tests).
pub fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
}

/// Carga y parsea `manifest.txt`.
pub fn load_manifest() -> Vec<CorpusArchive> {
    let text = fs::read_to_string(corpus_dir().join("manifest.txt")).expect("manifest");
    let mut out: Vec<CorpusArchive> = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("ARCHIVE") => {
                let file = parts.next().expect("archivo").to_string();
                let mode = parts.next().expect("modo").to_string();
                out.push(CorpusArchive {
                    file,
                    mode,
                    dirs: Vec::new(),
                    entries: Vec::new(),
                });
            }
            Some("DIR") => {
                let name = parts.next().expect("dir").to_string();
                out.last_mut()
                    .expect("ARCHIVE antes de DIR")
                    .dirs
                    .push(name);
            }
            Some("ENTRY") => {
                let arcname = parts.next().expect("arcname").to_string();
                let src = corpus_dir().join(parts.next().expect("src"));
                let size: u64 = parts.next().expect("size").parse().expect("size u64");
                out.last_mut()
                    .expect("ARCHIVE antes de ENTRY")
                    .entries
                    .push(ExpectedEntry { arcname, src, size });
            }
            _ => {}
        }
    }
    out
}

/// Digest blake3 de un archivo (streaming).
pub fn digest_of(path: &std::path::Path) -> arca_types::Digest {
    let mut f = fs::File::open(path).expect("abrir archivo");
    arca_types::Digest::of_reader(&mut f).expect("digest")
}

/// Recorre recursivamente un directorio y devuelve las rutas relativas de
/// los ARCHIVOS encontrados (ordenadas).
pub fn walk_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}
