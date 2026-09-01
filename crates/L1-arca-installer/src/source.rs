//! Origen de bytes del paquete (spec 12 §3).

use std::io::{Read, Seek};
use std::path::PathBuf;

use arca_types::Res;

/// De dónde viene el `.arca`.
#[derive(Debug)]
#[non_exhaustive]
pub enum PackageSource {
    /// Ruta en disco (tools-dev vía adb push, o archivo descargado).
    Path(PathBuf),
    /// File descriptor ya abierto (SAF/uri en Android vía JNI).
    File(std::fs::File),
    /// Bytes en memoria (dev-mode / tests).
    Bytes(Vec<u8>),
}

/// Reader boxed listo para `Archive::open` (Read+Seek).
pub(crate) struct PackageReader(Box<dyn ReadSeek>);

/// Trait unificado (object-safe).
pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

impl Read for PackageReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for PackageReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl PackageSource {
    /// Abre el paquete para lectura con seek.
    ///
    /// # Errors
    /// `Io` al abrir/leer `Path`/`File`.
    pub(crate) fn into_reader(self) -> Res<PackageReader> {
        Ok(match self {
            Self::Path(p) => PackageReader(Box::new(std::fs::File::open(p)?)),
            Self::File(f) => PackageReader(Box::new(f)),
            Self::Bytes(b) => PackageReader(Box::new(std::io::Cursor::new(b))),
        })
    }
}
