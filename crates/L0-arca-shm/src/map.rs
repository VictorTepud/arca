//! mmap RAII de una región shm.
//!
//! `ShmMap` toma ownership lógico del mapeo: drop = munmap (sin leaks de
//! mapeos, spec 05 §4). El fd original puede cerrarse: el mapeo vive por
//! sí mismo (semántica mmap de POSIX).

use std::ops::{Deref, DerefMut};
use std::os::fd::AsRawFd as _;
use std::ptr::NonNull;

use arca_types::{ArcaError, Res};
use memmap2::MmapRaw;

/// Mapeo vivo de una shm (RW). `ptr` alineada a página.
///
/// Invariante unsafe: el mapeo mide EXACTAMENTE `len` bytes y nadie más
/// trunca el backing store (memfd sellado con GROW|SHRINK en producción).
#[derive(Debug)]
pub struct ShmMap {
    inner: MmapRaw,
}

impl ShmMap {
    /// Mapea `len` bytes del fd. El tamaño del fichero debe coincidir con
    /// `len` (evita SIGBUS por mapeo más grande que el fichero — spec 05 §5).
    pub fn from_fd(fd: std::os::fd::BorrowedFd<'_>, len: usize) -> Res<Self> {
        if len == 0 {
            return Err(ArcaError::Internal("ShmMap: len 0"));
        }
        // MmapAsRawDesc: RawFd (POSIX: el mapeo vive aunque se cierre el fd).
        let inner = MmapRaw::map_raw(fd.as_raw_fd())
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("mmap: {e}"))))?;
        if inner.len() != len {
            return Err(ArcaError::Internal(
                "ShmMap: tamaño del fichero difiere del len pedido (ftruncate pendiente?)",
            ));
        }
        Ok(Self { inner })
    }

    /// Longitud del mapeo en bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// ¿Mapeo vacío? (nunca: el constructor rechaza len 0).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    /// Puntero base (alineado a página).
    #[must_use]
    pub fn as_ptr(&self) -> NonNull<u8> {
        // Invariante: MmapRaw::as_ptr devuelve puntero válido de `len` bytes
        // (no nulo tras un map exitoso).
        NonNull::new(self.inner.as_ptr() as *mut u8).unwrap_or_else(|| unreachable_checked())
    }

    /// Vista compartida (lectura).
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // Invariante: rango [ptr, ptr+len) válido y mapeado RW.
        unsafe { std::slice::from_raw_parts(self.as_ptr().as_ptr(), self.inner.len()) }
    }

    /// Vista mutable (solo el lado ESCRITOR debe usarla; ver RingSpsc/FrameSlots
    /// para sincronización correcta).
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // Invariante: rango [ptr, ptr+len) válido; `&mut self` garantiza
        // exclusividad de ESTE mapeo (no de la shm subyacente).
        unsafe { std::slice::from_raw_parts_mut(self.as_ptr().as_ptr(), self.inner.len()) }
    }

    /// Dirección del mapeo (logs/diagnóstico).
    #[must_use]
    pub fn addr_debug(&self) -> usize {
        self.as_ptr().as_ptr() as usize
    }
}

impl Deref for ShmMap {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl DerefMut for ShmMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

/// Marca `unreachable` sin clippy::unwrap_used (MmapRaw::as_ptr nunca es
/// nulo tras un map exitoso; el compilador no lo sabe).
#[cold]
fn unreachable_checked() -> ! {
    panic!("ShmMap: mmap devolvió NULL (imposible tras map exitoso)")
}
