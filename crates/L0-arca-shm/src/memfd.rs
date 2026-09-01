//! memfd: región anónima de RAM compartible entre procesos.
//!
//! Traspaso por socket: lo hace arca-ipc (SCM_RIGHTS); aquí solo el fd crudo.
//! Android: nix usa `syscall(SYS_memfd_create)` cuando el libc no expone el
//! símbolo (bionic < API 30) — mismo kernel, misma semántica.

use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, RawFd};

use arca_types::{ArcaError, Res};
use nix::sys::memfd::{memfd_create, MFdFlags};

/// memfd con tamaño fijo (ftruncate aplicado) y nombre con prefijo `arca-`.
///
/// El nombre es cosmético (aparece en /proc/<pid>/fdinfo) y sirve para
/// diagnóstico: NO es control de acceso.
#[derive(Debug)]
pub struct Memfd {
    fd: std::os::fd::OwnedFd,
    size: usize,
}

impl Memfd {
    /// Crea un memfd de `size` bytes (redondeado implícitamente a página por
    /// el kernel al mapear). `name` DEBE empezar con `arca-` (higiene).
    pub fn create(name: &str, size: usize) -> Res<Self> {
        if !name.starts_with("arca-") {
            return Err(ArcaError::Internal("memfd: nombre sin prefijo arca-"));
        }
        if size == 0 {
            return Err(ArcaError::Internal("memfd: tamaño 0"));
        }
        // MFD_ALLOW_SEALING: habilita seals posteriores (ver [`Self::seal`]).
        let fd = memfd_create(name, MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING)
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("memfd_create: {e}"))))?;
        nix::unistd::ftruncate(fd.as_fd(), size as i64)
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("ftruncate: {e}"))))?;
        Ok(Self { fd, size })
    }

    /// Sella GROW|SHRINK: nadie puede cambiar el tamaño después (defensa
    /// anti-SIGBUS por truncado malicioso — spec 05 §5 fila 5).
    ///
    /// NO sella WRITE: ambos lados escriben (app escribe frames, host escribe
    /// input) en regiones distintas del MISMO memfd... o de memfd distintos
    /// según el backend. El sellado completo de escritura rompería el diseño.
    pub fn seal_size(&self) -> Res<()> {
        use nix::fcntl::{FcntlArg, SealFlag};
        let flags = SealFlag::F_SEAL_GROW | SealFlag::F_SEAL_SHRINK;
        nix::fcntl::fcntl(self.fd.as_fd(), FcntlArg::F_ADD_SEALS(flags))
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("F_ADD_SEALS: {e}"))))?;
        Ok(())
    }

    /// Tamaño fijado en bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    /// fd crudo para traspasar (SCM_RIGHTS / dup2). El ownership lo retiene
    /// `Self` hasta drop.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Referencia prestada del fd (para `ShmMap::from_fd`).
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
