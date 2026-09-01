//! Canal de señal por eventfd (path caliente, docs/04 §4).
//!
//! **Semántica v1 (decisión de arquitecto, worklog T12):** el eventfd es
//! wakeup PURO (write 1 / read acumulado). NO transporta el payload u64 de
//! [`arca_protocol::encode_signal_wire`]: eventfd SUMA los valores y dos
//! señales acumuladas darían un valor corrupto. El payload real viaja:
//! - FrameTick t_ns → slot Vsync del ring de input (T18, docs/04 §6).
//! - FrameReady frame_seq → implícito: "haz read_latest del shm".
//!
//! Cuando el valor importa y el socket está a mano, se usa el framing de
//! señal por socket ([`crate::Conn::send_signal`]).

use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use arca_types::{ArcaError, Res};

/// Extremo de un eventfd de señal (wakeup).
#[derive(Debug)]
pub struct SignalChannel {
    fd: OwnedFd,
}

impl SignalChannel {
    /// Crea un eventfd nuevo (EFD_CLOEXEC; sin EFD_SEMAPHORE: cada read
    /// drena todo lo acumulado como un único "hay novedades").
    pub fn new() -> Res<Self> {
        let efd = nix::sys::eventfd::EventFd::from_value_and_flags(
            0,
            nix::sys::eventfd::EfdFlags::EFD_CLOEXEC,
        )
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("eventfd: {e}"))))?;
        Ok(Self { fd: efd.into() })
    }

    /// Envuelve un fd heredado YA en `OwnedFd` (rt: fds 5/6 de arca-launch).
    pub fn from_owned(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Envuelve un fd crudo heredado.
    ///
    /// # Safety
    /// `fd` debe ser un eventfd válido y el caller transfiere ownership
    /// (nadie más lo cierra).
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }

    /// Notifica al otro extremo (idempotente: acumula).
    pub fn notify(&self) -> Res<()> {
        let v = 1u64.to_le_bytes();
        let mut n = 0usize;
        while n < 8 {
            let w = nix::unistd::write(self.fd.as_fd(), &v[n..])
                .map_err(|e| ArcaError::Io(std::io::Error::other(format!("eventfd write: {e}"))))?;
            n += w;
        }
        Ok(())
    }

    /// Espera wakeup con deadline. `Ok(None)` = timeout limpio.
    pub fn wait(&self, deadline: Duration) -> Res<Option<u64>> {
        use nix::poll::{poll, PollFd, PollFlags};
        let mut pfd = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        poll(&mut pfd, ptimeout(deadline)?)
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("eventfd poll: {e}"))))?;
        if pfd[0]
            .revents()
            .is_none_or(|r| !r.contains(PollFlags::POLLIN))
        {
            return Ok(None); // timeout
        }
        self.drain()
    }

    /// Drena sin bloquear (0 si no había nada).
    pub fn try_wait(&self) -> Res<Option<u64>> {
        use nix::poll::{poll, PollFd, PollFlags};
        let mut pfd = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        poll(&mut pfd, nix::poll::PollTimeout::NONE)
            .map_err(|e| ArcaError::Io(std::io::Error::other(format!("eventfd poll: {e}"))))?;
        if pfd[0]
            .revents()
            .is_none_or(|r| !r.contains(PollFlags::POLLIN))
        {
            return Ok(None);
        }
        self.drain()
    }

    fn drain(&self) -> Res<Option<u64>> {
        let mut v = [0u8; 8];
        let mut n = 0usize;
        while n < 8 {
            let r = nix::unistd::read(self.fd.as_fd(), &mut v[n..])
                .map_err(|e| ArcaError::Io(std::io::Error::other(format!("eventfd read: {e}"))))?;
            if r == 0 {
                break;
            }
            n += r;
        }
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(u64::from_le_bytes(v)))
    }

    /// fd prestado (para passthrough al hijo / epoll).
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// fd crudo (dup2 al número fijo del hijo).
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

/// PollTimeout saturado desde Duration.
fn ptimeout(d: Duration) -> Res<nix::poll::PollTimeout> {
    use nix::poll::PollTimeout;
    PollTimeout::try_from(d).map_err(|_| ArcaError::Internal("ipc: timeout de poll fuera de rango"))
}
