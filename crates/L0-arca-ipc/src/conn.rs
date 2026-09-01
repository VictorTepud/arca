//! Server (bind/accept) + Client (connect con backoff) + Conn (framing con
//! SCM_RIGHTS y deadlines).

use std::cell::Cell;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

use arca_protocol::{
    decode, decode_signal, encode_into, encode_signal_into, MsgHeader, MAX_FDS_PER_SEND,
};
use arca_types::{ArcaError, Res};
use nix::cmsg_space;
use nix::sys::socket::{
    bind, connect, getsockopt, listen, recvmsg, sendmsg, setsockopt, socket, sockopt, Backlog,
    ControlMessage, ControlMessageOwned, MsgFlags, RecvMsg, SockFlag, SockType, UnixAddr,
    UnixCredentials,
};
use tracing::warn;

use crate::uds::ensure_filesystem_path;

/// Timeout limpio (deadline vencido).
fn timeout_err(ctx: &'static str) -> ArcaError {
    ArcaError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!("ipc: timeout ({ctx})"),
    ))
}

/// Servidor UDS en filesystem (el host crea uno por instancia en
/// `runtime/<inst>/app.sock`).
#[derive(Debug)]
pub struct Server {
    fd: OwnedFd,
    path: PathBuf,
}

impl Server {
    /// bind + listen en `path` (0700). Si existe un socket stale del mismo
    /// host se retira (relaunch idempotente); si existe un archivo que NO
    /// es socket → error (no se pisa nada ajeno).
    pub fn bind(path: &Path) -> Res<Self> {
        ensure_filesystem_path(path)?;
        // socket stale?
        if path.exists() {
            use std::os::unix::fs::FileTypeExt as _;
            let meta = std::fs::symlink_metadata(path).map_err(ArcaError::from)?;
            let is_sock = meta.file_type().is_socket();
            if is_sock {
                std::fs::remove_file(path).map_err(ArcaError::from)?;
            } else {
                return Err(ArcaError::Internal(
                    "ipc: el path del socket existe y no es un socket",
                ));
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(ArcaError::from)?;
            // 0700 al directorio del runtime (solo el UID del host entra).
            apply_mode(dir, 0o700)?;
        }
        let fd = socket(
            nix::sys::socket::AddressFamily::Unix,
            SockType::Stream,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
        .map_err(io_err("socket"))?;
        let addr = UnixAddr::new(path).map_err(io_err("unixaddr"))?;
        bind(fd.as_raw_fd(), &addr).map_err(io_err("bind"))?;
        listen(&fd, Backlog::MAXCONN).map_err(io_err("listen"))?;
        apply_mode(path, 0o700)?;
        Ok(Self {
            fd,
            path: path.to_owned(),
        })
    }

    /// Acepta UNA conexión con deadline. Verifica SO_PEERCRED SIEMPRE
    /// (spec 06 §4): UID ≠ propio → cierre + error.
    pub fn accept(&self, deadline: Duration) -> Res<Conn> {
        use nix::poll::{poll, PollFd, PollFlags};
        let mut pfd = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];
        let ms = deadline.as_millis().min(u32::MAX as u128);
        poll(&mut pfd, poll_timeout(ms)?).map_err(io_err("poll accept"))?;
        if pfd[0]
            .revents()
            .is_none_or(|r| !r.contains(PollFlags::POLLIN))
        {
            return Err(timeout_err("accept"));
        }
        let cfd = nix::sys::socket::accept4(self.fd.as_raw_fd(), SockFlag::SOCK_CLOEXEC)
            .map_err(io_err("accept4"))?;
        let cfd = unsafe { OwnedFd::from_raw_fd(cfd) };
        Conn::from_fd(cfd)
    }

    /// Path del socket (para logs / cleanup externo).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // unlink del archivo de socket (idempotente; el dir lo limpia sweep)
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Cliente: connect con backoff 5/20/100 ms (máx 6) contra la carrera de
/// spawn (spec 06 §5).
pub struct Client;

impl Client {
    /// Conecta a `path` con deadline total. El peer cred se verifica en la
    /// [`Conn`] resultante (mismo UID).
    pub fn connect(path: &Path, deadline: Duration) -> Res<Conn> {
        ensure_filesystem_path(path)?;
        let start = std::time::Instant::now();
        let mut intento = 0usize;
        loop {
            let fd = socket(
                nix::sys::socket::AddressFamily::Unix,
                SockType::Stream,
                SockFlag::SOCK_CLOEXEC,
                None,
            )
            .map_err(io_err("socket"))?;
            let addr = UnixAddr::new(path).map_err(io_err("unixaddr"))?;
            match connect(fd.as_raw_fd(), &addr) {
                Ok(()) => return Conn::from_fd(fd),
                Err(nix::errno::Errno::ECONNREFUSED) | Err(nix::errno::Errno::ENOENT) => {
                    if start.elapsed() >= deadline || intento >= CONNECT_BACKOFF_MS.len() {
                        return Err(ArcaError::Internal(
                            "ipc: connect agotado (server no escucha tras backoffs)",
                        ));
                    }
                    let ms = CONNECT_BACKOFF_MS[intento];
                    intento += 1;
                    std::thread::sleep(Duration::from_millis(ms));
                }
                Err(e) => return Err(io_err("connect")(e)),
            }
        }
    }
}

const CONNECT_BACKOFF_MS: [u64; 6] = [5, 20, 100, 100, 100, 100];

/// Conexión UDS con peer verificado + framing AIPC + fds.
#[derive(Debug)]
pub struct Conn {
    fd: OwnedFd,
    peer: UnixCredentials,
    deadline_ms: Cell<u32>,
    /// fds recibidos en recvs parciales de la trama en curso.
    pending_fds: Vec<OwnedFd>,
}

impl Conn {
    /// Envuelve un fd YA conectado (p. ej. extremo de socketpair heredado
    /// por el hijo) verificando SO_PEERCRED.
    pub fn from_fd(fd: OwnedFd) -> Res<Self> {
        let peer: UnixCredentials =
            getsockopt(&fd, sockopt::PeerCredentials).map_err(io_err("peercred"))?;
        Self::verify_uid(peer)?;
        Ok(Self {
            fd,
            peer,
            deadline_ms: Cell::new(0),
            pending_fds: Vec::new(),
        })
    }

    /// UID del par (SO_PEERCRED — kernel, no confiable del mensaje).
    #[must_use]
    pub fn peer_uid(&self) -> u32 {
        self.peer.uid()
    }

    /// PID del par (watchdog/lifecycle).
    #[must_use]
    pub fn peer_pid(&self) -> i32 {
        self.peer.pid()
    }

    fn verify_uid(peer: UnixCredentials) -> Res<()> {
        let mine = nix::unistd::Uid::current().as_raw();
        if peer.uid() != mine {
            warn!(
                target: "arca::ipc::conn",
                uid_peer = peer.uid(),
                uid_host = mine,
                "peer uid mismatch: conexión rechazada"
            );
            return Err(ArcaError::Internal(
                "ipc: peer uid no coincide (SO_PEERCRED) — conexión rechazada",
            ));
        }
        Ok(())
    }

    /// Deadline (ms) para send/recv siguientes. OBLIGATORIO antes de
    /// `recv_frame` (spec 06 §6: elegimos runtime-check con error tipado).
    pub fn set_deadline(&self, ms: u32) -> Res<()> {
        use nix::sys::socket::sockopt::{ReceiveTimeout, SendTimeout};
        use nix::sys::time::TimeValLike as _;
        let tv = nix::sys::time::TimeVal::milliseconds(ms as i64);
        setsockopt(&self.fd, ReceiveTimeout, &tv).map_err(io_err("SO_RCVTIMEO"))?;
        setsockopt(&self.fd, SendTimeout, &tv).map_err(io_err("SO_SNDTIMEO"))?;
        self.deadline_ms.set(ms);
        Ok(())
    }

    /// Deadline activo (0 = sin configurar).
    #[must_use]
    pub fn deadline_ms(&self) -> u32 {
        self.deadline_ms.get()
    }

    fn ensure_deadline(&self) -> Res<()> {
        if self.deadline_ms.get() == 0 {
            return Err(ArcaError::Internal(
                "ipc: recv_frame sin set_deadline previo (prohibido: spec 06 §6)",
            ));
        }
        Ok(())
    }

    /// Envía UNA trama completa (header+payload ya serializados por
    /// arca-protocol) con hasta MAX_FDS_PER_SEND fds (SCM_RIGHTS en el
    /// PRIMER sendmsg; los parciales van sin cmsgs).
    pub fn send_frame(&mut self, bytes: &[u8], fds: &[RawFd]) -> Res<()> {
        if fds.len() > MAX_FDS_PER_SEND {
            return Err(ArcaError::Internal("ipc: >8 fds en un solo send"));
        }
        let mut off = 0usize;
        let mut first = true;
        while off < bytes.len() {
            let iov = [std::io::IoSlice::new(&bytes[off..])];
            let cmsg = if first && !fds.is_empty() {
                vec![ControlMessage::ScmRights(fds)]
            } else {
                vec![]
            };
            // MSG_NOSIGNAL: un peer muerto no nos mata con SIGPIPE.
            let n = sendmsg(
                self.fd.as_raw_fd(),
                &iov,
                &cmsg,
                MsgFlags::MSG_NOSIGNAL,
                None::<&UnixAddr>,
            )
            .map_err(io_err("sendmsg"))?;
            first = false;
            if n == 0 {
                return Err(ArcaError::Internal("ipc: sendmsg devolvió 0"));
            }
            off += n;
        }
        Ok(())
    }

    /// Recibe UNA trama completa: llena `buf` (header+payload) y `fds_out`
    /// (ownership RAII: drop cierra). Deadline requerido.
    pub fn recv_frame(&mut self, buf: &mut Vec<u8>, fds_out: &mut Vec<OwnedFd>) -> Res<MsgHeader> {
        self.ensure_deadline()?;
        buf.clear();
        buf.resize(arca_protocol::HEADER_LEN, 0);
        self.read_exact(buf)?;
        let hdr = MsgHeader::parse(buf)?;
        let total = arca_protocol::HEADER_LEN + hdr.length as usize;
        buf.resize(total, 0);
        self.read_exact(&mut buf[arca_protocol::HEADER_LEN..])?;
        fds_out.append(&mut self.pending_fds);
        Ok(hdr)
    }

    /// Lee hasta llenar `buf` acumulando los cmsgs (fds) de la trama en
    /// curso en `pending_fds` (entregados al completar `recv_frame`).
    fn read_exact(&mut self, buf: &mut [u8]) -> Res<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let mut iov = [std::io::IoSliceMut::new(&mut buf[done..])];
            let mut cspace = cmsg_space!([RawFd; MAX_FDS_PER_SEND]);
            let r: RecvMsg<'_, '_, UnixAddr> = recvmsg::<UnixAddr>(
                self.fd.as_raw_fd(),
                &mut iov,
                Some(&mut cspace),
                MsgFlags::empty(),
            )
            .map_err(io_err("recvmsg"))?;
            if r.bytes == 0 {
                return Err(ArcaError::Internal("ipc: peer cerró la conexión"));
            }
            // Invariante: los fds del ancillary SON del receptor: cerrar si
            // la trama se aborta (RAII con OwnedFd en pending_fds).
            // cmsgs() valida truncado del control (ENOBUFS) — fail-closed.
            for cmsg in r.cmsgs().map_err(io_err("cmsgs"))? {
                if let ControlMessageOwned::ScmRights(recibidos) = cmsg {
                    for &fd in &recibidos {
                        // Invariante: dup implícito del kernel → ownership.
                        self.pending_fds.push(unsafe { OwnedFd::from_raw_fd(fd) });
                    }
                }
            }
            done += r.bytes;
        }
        Ok(())
    }

    /// Envía un mensaje de control (framing completo + optional fds).
    pub fn send_ctl(
        &mut self,
        msg: &arca_protocol::ControlMsg,
        seq: u64,
        fds: &[RawFd],
    ) -> Res<()> {
        let mut buf = Vec::with_capacity(128);
        encode_into(msg, seq, &mut buf)?;
        self.send_frame(&buf, fds)
    }

    /// Recibe un mensaje de control YA deserializado (ergonomía de host).
    pub fn recv_ctl_msg(&mut self) -> Res<arca_protocol::ControlMsg> {
        let mut buf = Vec::new();
        let mut fds = Vec::new();
        let (_, archived) = self.recv_ctl(&mut buf, &mut fds)?;
        // los fds de este recv son del caller: devueltos via drop RAII
        drop(fds);
        rkyv::deserialize::<arca_protocol::ControlMsg, rkyv::rancor::Error>(archived)
            .map_err(|_| ArcaError::InvalidFrame("ctl: deserialize"))
    }

    /// Recibe un mensaje de control (devuelve header + Archived prestado
    /// del buffer `buf` que el caller retiene).
    pub fn recv_ctl<'a>(
        &mut self,
        buf: &'a mut Vec<u8>,
        fds_out: &mut Vec<OwnedFd>,
    ) -> Res<(MsgHeader, &'a rkyv::Archived<arca_protocol::ControlMsg>)> {
        let hdr = self.recv_frame(buf, fds_out)?;
        let (h2, archived) = decode(buf)?;
        debug_assert_eq!(hdr.seq, h2.seq);
        Ok((hdr, archived))
    }

    /// Envía una señal por el canal socket (no eventfd).
    pub fn send_signal(&mut self, s: &arca_protocol::SignalMsg, seq: u64) -> Res<()> {
        let mut buf = Vec::with_capacity(64);
        encode_signal_into(s, seq, &mut buf)?;
        self.send_frame(&buf, &[])
    }

    /// Recibe una señal del canal socket.
    pub fn recv_signal<'a>(
        &mut self,
        buf: &'a mut Vec<u8>,
    ) -> Res<(MsgHeader, &'a rkyv::Archived<arca_protocol::SignalMsg>)> {
        let mut fds = Vec::new();
        let hdr = self.recv_frame(buf, &mut fds)?;
        if !fds.is_empty() {
            warn!(target: "arca::ipc::conn", n = fds.len(), "señal con fds inesperados (ignorados)");
        }
        let (h2, archived) = decode_signal(buf)?;
        debug_assert_eq!(hdr.seq, h2.seq);
        Ok((hdr, archived))
    }

    /// fd crudo (para epoll del host-core / passthrough al hijo).
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// fd prestado (para poll/epoll registration).
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Cierra la conexión (drain explícito de pending fds).
    pub fn shutdown(&mut self) {
        self.pending_fds.clear();
        let _ = nix::sys::socket::shutdown(self.fd.as_raw_fd(), nix::sys::socket::Shutdown::Both);
    }
}

fn io_err(ctx: &'static str) -> impl Fn(nix::errno::Errno) -> ArcaError {
    move |e: nix::errno::Errno| {
        if e == nix::errno::Errno::EAGAIN {
            timeout_err(ctx)
        } else {
            ArcaError::Io(std::io::Error::other(format!("{ctx}: {e}")))
        }
    }
}

/// Convierte ms a PollTimeout (saturando).
fn poll_timeout(ms: u128) -> Res<nix::poll::PollTimeout> {
    use nix::poll::PollTimeout;
    PollTimeout::try_from(ms as i32)
        .map_err(|_| ArcaError::Internal("ipc: timeout de poll fuera de rango"))
}

fn apply_mode(p: &Path, mode: u32) -> Res<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).map_err(ArcaError::from)
}
