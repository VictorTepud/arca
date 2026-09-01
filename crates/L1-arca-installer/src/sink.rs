//! Sinks de instalación (spec 12 §3, puente arca-7z ↔ arca-sign).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use arca_7z::{DirSink, EntrySink, RelPath};
use arca_sign::StreamingVerifier;
use arca_types::{ArcaError, Res};

/// Límite de captura del `MemorySink` (manifest ≤ 64 KiB + firma + digest).
const MEMORY_SINK_CAP: usize = 2 * 1024 * 1024;

/// Sink que acumula archivos pequeños en memoria (pass 1: manifest +
/// signature.bin + manifest.digest). Nada toca el disco.
pub(crate) struct MemorySink {
    files: BTreeMap<String, Vec<u8>>,
    total: usize,
    fake_root: PathBuf,
}

impl MemorySink {
    /// Crea el sink.
    pub(crate) fn new() -> Self {
        Self {
            files: BTreeMap::new(),
            total: 0,
            fake_root: PathBuf::from("/nonexistent"),
        }
    }

    /// Bytes capturados de un path (`None` si nunca llegó).
    pub(crate) fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(Vec::as_slice)
    }
}

impl EntrySink for MemorySink {
    fn mkdir(&mut self, _rel: &RelPath) -> Res<()> {
        Ok(()) // los dirs no importan en memoria
    }

    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn Read) -> Res<u64> {
        let mut buf = Vec::new();
        data.read_to_end(&mut buf)?;
        if self.total + buf.len() > MEMORY_SINK_CAP {
            return Err(ArcaError::InvalidPackage(
                "pass1: archivos meta exceden 2 MiB",
            ));
        }
        self.total += buf.len();
        let n = buf.len() as u64;
        self.files.insert(rel.as_str().to_owned(), buf);
        Ok(n)
    }

    fn root(&self) -> &std::path::Path {
        &self.fake_root
    }
}

/// Sink de staging con tee al verificador (pass 2).
///
/// Enrutado por path:
/// - declarado en `expected` (sha del manifest) → write + `feed` por bloque
///   + `end_file` (abort temprano si el sha no cuadra).
/// - cualquier otro archivo del layout (assets/icons sin declarar, meta/*)
///   → write SIN verificación de sha (ver nota de cobertura en lib.rs).
pub(crate) struct StagingSink<'a> {
    inner: DirSink,
    verifier: &'a mut StreamingVerifier,
    expected: BTreeSet<String>,
}

impl<'a> StagingSink<'a> {
    /// Crea el sink sobre el DirSink de staging.
    pub(crate) fn new(
        inner: DirSink,
        verifier: &'a mut StreamingVerifier,
        expected: BTreeSet<String>,
    ) -> Self {
        Self {
            inner,
            verifier,
            expected,
        }
    }
}

/// Reader que copia al verificador mientras se lee (tee).
struct TeeReader<'v, 'p> {
    inner: &'p mut dyn Read,
    verifier: &'v mut StreamingVerifier,
    path: String,
}

impl Read for TeeReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.verifier
                .feed(&self.path, &buf[..n])
                .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }
}

impl EntrySink for StagingSink<'_> {
    fn mkdir(&mut self, rel: &RelPath) -> Res<()> {
        self.inner.mkdir(rel)
    }

    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn Read) -> Res<u64> {
        let path = rel.as_str().to_owned();
        if self.expected.contains(&path) {
            // split de borrows: verifier vs inner (campos distintos)
            let v = &mut *self.verifier;
            let inner = &mut self.inner;
            let mut tee = TeeReader {
                inner: data,
                verifier: v,
                path: path.clone(),
            };
            let n = inner.write_entry(rel, &mut tee)?;
            // El archivo terminó: cierre + sha inmediato (abort temprano).
            self.verifier.end_file(&path)?;
            Ok(n)
        } else {
            self.inner.write_entry(rel, data)
        }
    }

    fn root(&self) -> &std::path::Path {
        self.inner.root()
    }
}

/// Guard de staging: borra el dir temporal en Drop salvo que se desactive
/// (tras el rename atómico el contenido ya es la versión instalada).
#[must_use = "mantén el guard vivo hasta el commit o se borra el staging"]
pub(crate) struct StagingGuard {
    path: Option<PathBuf>,
}

impl StagingGuard {
    /// Registra un staging a limpiar si el flujo muere antes del rename.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Desactiva la limpieza (éxito: el rename se hizo dueño del dir).
    pub(crate) fn defuse(mut self) {
        self.path = None;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            // mejor esfuerzo: sweep de arranque cubre lo que quede
            let _ = std::fs::remove_dir_all(p);
        }
    }
}
