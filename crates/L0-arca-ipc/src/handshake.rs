//! Handshake AIPC-1 (docs/04 §3): HELLO → WELCOME(+fds) → READY → ATTACH.

use std::os::fd::{OwnedFd, RawFd};

use arca_protocol::{Attach, ControlMsg, Hello, Ready, ShmLayout, Welcome, WindowSpec};
use arca_types::{AppId, Capability, Digest, InstanceId, PROTO_VERSION};
use arca_types::{ArcaError, Res};
use tracing::warn;

use crate::conn::Conn;
use crate::HANDSHAKE_DEADLINE_MS;

/// Número de fds que viajan en WELCOME (orden fijo: [0] frames, [1] input).
pub const WELCOME_FDS: usize = 2;

/// Lo que el host espera del HELLO (contraste anti-sustitución).
#[derive(Debug, Clone)]
pub struct HelloExpect {
    /// Id de la app que el host va a lanzar.
    pub app_id: AppId,
    /// Instancia asignada.
    pub instance: InstanceId,
    /// Digest del artefacto instalado (verify_installed ya lo validó).
    pub artifact_hash: Digest,
}

/// Resultado del lado cliente (sub-app): welcome + fds mapeables + attach.
#[derive(Debug)]
pub struct ClientHandshake {
    /// Conexión lista (ctl + señal por socket disponible).
    pub conn: Conn,
    /// WELCOME recibido.
    pub welcome: Welcome,
    /// fds recibidos por SCM_RIGHTS, EN ORDEN: [0] frames, [1] input.
    pub memfds: Vec<OwnedFd>,
    /// ATTACH recibido.
    pub attach: Attach,
}

/// Ejecuta el handshake del lado HOST.
///
/// Flujo: recv HELLO (validar identidad + versión) → send WELCOME con los
/// `memfds` por SCM_RIGHTS → recv READY → send ATTACH. Tras volver, el
/// host puede empezar a mandar FrameTick.
#[allow(clippy::too_many_arguments)]
pub fn handshake_server(
    conn: &mut Conn,
    expect: &HelloExpect,
    memfds: &[RawFd],
    layout: ShmLayout,
    caps: &[Capability],
    windows: &[WindowSpec],
) -> Res<Ready> {
    conn.set_deadline(HANDSHAKE_DEADLINE_MS)?;
    // 1) HELLO
    let mut buf = Vec::new();
    let mut fds = Vec::new();
    let (_, archived) = conn.recv_ctl(&mut buf, &mut fds)?;
    let msg: ControlMsg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(archived)
        .map_err(|_| ArcaError::InvalidFrame("handshake: Hello ilegible"))?;
    let ControlMsg::Hello(hello) = msg else {
        return Err(ArcaError::InvalidFrame("handshake: esperaba Hello"));
    };
    if !fds.is_empty() {
        warn!(target: "arca::ipc::handshake", "HELLO con fds inesperados");
        fds.clear();
    }
    validate_hello(&hello, expect)?;
    // 2) WELCOME + fds
    let welcome = ControlMsg::Welcome(Welcome {
        proto_min: PROTO_VERSION,
        layout,
        caps_granted: caps.to_vec(),
    });
    conn.send_ctl(&welcome, 1, memfds)?;
    // 3) READY
    let mut rbuf = Vec::new();
    let mut rfds = Vec::new();
    let _ = conn.recv_ctl(&mut rbuf, &mut rfds)?;
    let ready_arch = {
        // re-decode del mismo buffer (recv_ctl ya validó)
        let (_, archived) = arca_protocol::decode(&rbuf)?;
        archived
    };
    let ready_msg: ControlMsg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(ready_arch)
        .map_err(|_| ArcaError::InvalidFrame("handshake: Ready ilegible"))?;
    let ControlMsg::Ready(ready) = ready_msg else {
        return Err(ArcaError::InvalidFrame("handshake: esperaba Ready"));
    };
    if !rfds.is_empty() {
        warn!(target: "arca::ipc::handshake", "READY con fds inesperados");
        rfds.clear();
    }
    // 4) ATTACH
    let attach = ControlMsg::Attach(Attach {
        windows: windows.to_vec(),
    });
    conn.send_ctl(&attach, 2, &[])?;
    Ok(ready)
}

/// Ejecuta el handshake del lado SUB-APP (runtime). Recibe la [`Conn`] por
/// valor (socketpair end heredado) y la devuelve dentro del resultado.
pub fn handshake_client(mut conn: Conn, hello: &Hello) -> Res<ClientHandshake> {
    conn.set_deadline(HANDSHAKE_DEADLINE_MS)?;
    // 1) HELLO
    conn.send_ctl(&ControlMsg::Hello(hello.clone()), 1, &[])?;
    // 2) WELCOME + fds
    let mut buf = Vec::new();
    let mut fds = Vec::new();
    let _ = conn.recv_ctl(&mut buf, &mut fds)?;
    let welcome_arch = {
        let (_, archived) = arca_protocol::decode(&buf)?;
        archived
    };
    let wmsg: ControlMsg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(welcome_arch)
        .map_err(|_| ArcaError::InvalidFrame("handshake: Welcome ilegible"))?;
    let ControlMsg::Welcome(welcome) = wmsg else {
        return Err(ArcaError::InvalidFrame("handshake: esperaba Welcome"));
    };
    if fds.len() != WELCOME_FDS {
        warn!(target: "arca::ipc::handshake", n = fds.len(), "WELCOME sin los 2 fds esperados");
        return Err(ArcaError::InvalidFrame(
            "handshake: WELCOME sin los 2 memfd (SCM_RIGHTS)",
        ));
    }
    // Invariante: los OwnedFd tomados del ancillary SON nuestro ownership.
    let memfds: Vec<OwnedFd> = fds;
    // 3) READY
    let ready = ControlMsg::Ready(Ready {
        rt_version: 1,
        sdk_version: 0, // F2 headless: la app aún no enlaza sdk
        ui_caps: arca_protocol::UiCaps {
            fonts_atlas_damage: false,
        },
    });
    conn.send_ctl(&ready, 2, &[])?;
    // 4) ATTACH
    let mut abuf = Vec::new();
    let mut afds = Vec::new();
    let _ = conn.recv_ctl(&mut abuf, &mut afds)?;
    let attach_arch = {
        let (_, archived) = arca_protocol::decode(&abuf)?;
        archived
    };
    let amsg: ControlMsg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(attach_arch)
        .map_err(|_| ArcaError::InvalidFrame("handshake: Attach ilegible"))?;
    let ControlMsg::Attach(attach) = amsg else {
        return Err(ArcaError::InvalidFrame("handshake: esperaba Attach"));
    };
    if !afds.is_empty() {
        warn!(target: "arca::ipc::handshake", "ATTACH con fds inesperados");
        afds.clear();
    }
    if attach.windows.is_empty() {
        return Err(ArcaError::InvalidFrame("handshake: ATTACH sin ventanas"));
    }
    Ok(ClientHandshake {
        conn,
        welcome,
        memfds,
        attach,
    })
}

/// Valida identidad/versión del HELLO contra lo esperado.
fn validate_hello(h: &Hello, expect: &HelloExpect) -> Res<()> {
    // Versión: misma major; el host puede tener minor mayor (negociación).
    let negotiated = h
        .proto
        .negotiate(PROTO_VERSION)
        .ok_or(ArcaError::ProtocolMismatch {
            have: PROTO_VERSION,
            want: h.proto,
        })?;
    let _ = negotiated;
    if h.app_id != expect.app_id {
        warn!(target: "arca::ipc::handshake", app = %h.app_id, "app_id no coincide");
        return Err(ArcaError::Internal("handshake: app_id no coincide"));
    }
    if h.instance != expect.instance {
        warn!(target: "arca::ipc::handshake", inst = h.instance.get(), "instance no coincide");
        return Err(ArcaError::Internal("handshake: instance no coincide"));
    }
    if h.artifact_hash != expect.artifact_hash {
        warn!(target: "arca::ipc::handshake", "artifact_hash no coincide (anti-sustitución)");
        return Err(ArcaError::Internal("handshake: artifact_hash no coincide"));
    }
    Ok(())
}
