//! Loopback real de transporte: framing 10k + SCM_RIGHTS, handshake con
//! memfd compartidos, deadlines, abstract-namespace y latencia de señal.

use std::os::fd::{AsFd as _, OwnedFd};
use std::time::{Duration, Instant};

use arca_ipc::{
    handshake_client, handshake_server, Client, Conn, HelloExpect, Server, SignalChannel,
};
use arca_protocol::{ControlMsg, Hello, ShmLayout, WindowSpec};
use arca_shm::{FrameSlots, Memfd, RingSpsc, ShmMap};
use arca_types::{AppId, Capability, Digest, InstanceId, ProtoVersion, WinId};

fn tmpdir(nombre: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("arca-ipc-{nombre}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

/// socketpair UDS real para tests rápidos (sin bind al fs).
/// Los `OwnedFd` se consumen POR VALOR (único owner por fd: sin doble close).
fn pair() -> (Conn, Conn) {
    use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
    let (a, b) = socketpair(
        AddressFamily::Unix,
        SockType::Stream,
        None,
        SockFlag::SOCK_CLOEXEC,
    )
    .expect("socketpair");
    let ca = Conn::from_fd(a).expect("conn a");
    let cb = Conn::from_fd(b).expect("conn b");
    (ca, cb)
}

#[test]
fn loopback_10k_mensajes_con_fds() {
    let (mut a, mut b) = pair();
    a.set_deadline(2_000).expect("deadline a");
    b.set_deadline(2_000).expect("deadline b");
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let h = std::thread::spawn(move || {
        // lado b: recibe 10k; cada 500 llega un fd (memfd) que valida y cierra.
        let mut buf = Vec::new();
        let mut fds: Vec<OwnedFd> = Vec::new();
        let mut fds_recibidos = 0usize;
        for k in 0u64..10_000 {
            let (hdr, archived) = b.recv_ctl(&mut buf, &mut fds).expect("recv");
            assert_eq!(hdr.seq, k, "orden del canal único");
            let msg =
                rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(archived).expect("deser");
            match msg {
                ControlMsg::Ping { t_ns } => assert_eq!(t_ns, k),
                other => panic!("esperaba Ping, vino {other:?}"),
            }
            for fd in fds.drain(..) {
                // el memfd llegó VIVO: fstat funciona (lo dropeamos tras validar)
                let _st = nix::sys::stat::fstat(fd.as_fd()).expect("fstat del fd recibido");
                fds_recibidos += 1;
            }
        }
        assert_eq!(fds_recibidos, 20, "20 fds (k=499..9999 cada 500)");
        tx.send(()).expect("tx");
    });
    // lado a: envía 10k Ping; cada 500 crea un memfd y lo manda con el msg 499.
    let mut fd = Memfd::create("arca-loop", 4096).expect("memfd");
    for k in 0u64..10_000 {
        if k % 500 == 0 && k > 0 {
            fd = Memfd::create("arca-loop", 4096).expect("memfd");
        }
        let fds = if k % 500 == 499 {
            vec![fd.raw_fd()]
        } else {
            vec![]
        };
        a.send_ctl(&ControlMsg::Ping { t_ns: k }, k, &fds)
            .expect("send");
    }
    rx.recv_timeout(Duration::from_secs(60)).expect("hilo b ok");
    h.join().expect("join");
}

#[test]
fn recv_sin_deadline_rechazado() {
    let (mut a, _b) = pair();
    let mut buf = Vec::new();
    let mut fds = Vec::new();
    // sin set_deadline → error tipado (spec 06 §6: nunca bloqueos infinitos)
    match a.recv_frame(&mut buf, &mut fds) {
        Err(arca_types::ArcaError::Internal(msg)) => assert!(msg.contains("deadline")),
        other => panic!("esperaba error de deadline, vino {other:?}"),
    }
}

#[test]
fn timeout_de_accept_limpio() {
    let dir = tmpdir("accept-to");
    let server = Server::bind(&dir.join("app.sock")).expect("bind");
    let t0 = Instant::now();
    match server.accept(Duration::from_millis(80)) {
        Err(arca_types::ArcaError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock, "kind: {e:?}");
        }
        other => panic!("esperaba timeout, vino {other:?}"),
    }
    assert!(
        t0.elapsed() >= Duration::from_millis(70),
        "esperó el deadline"
    );
}

#[test]
fn abstract_y_paths_invalidos_rechazados() {
    use std::os::unix::ffi::OsStrExt as _;
    let abstracto = std::ffi::OsStr::from_bytes(b"\0abstracto");
    assert!(Server::bind(std::path::Path::new(abstracto)).is_err());
    assert!(Client::connect(std::path::Path::new(abstracto), Duration::from_millis(50)).is_err());
    assert!(Server::bind(std::path::Path::new("")).is_err());
    assert!(Client::connect(std::path::Path::new("@x"), Duration::from_millis(50)).is_err());
}

#[test]
fn peer_cred_verificado() {
    let (a, b) = pair();
    // mismo proceso: uid coincide (SO_PEERCRED)
    let mine = nix::unistd::Uid::current().as_raw();
    assert_eq!(a.peer_uid(), mine);
    assert_eq!(b.peer_uid(), mine);
    assert!(a.peer_pid() > 0);
}

/// Handshake completo + 2 memfd reales compartidos (SCM_RIGHTS) y usados de
/// punta a punta: server escribe en la shm, client lee por su fd recibido.
#[test]
fn handshake_completo_con_memfd() {
    let frames = Memfd::create("arca-frames", arca_shm::region_len(1024)).expect("memfd frames");
    let input = Memfd::create("arca-input", 64 + 64 * 16).expect("memfd input");
    // init regiones ANTES de compartir
    {
        let mut m = ShmMap::from_fd(frames.as_fd(), arca_shm::region_len(1024)).expect("map");
        FrameSlots::init(m.as_mut_slice(), 1024).expect("init frames");
    }
    {
        let mut m = ShmMap::from_fd(input.as_fd(), 64 + 64 * 16).expect("map");
        RingSpsc::init(m.as_mut_slice(), 64, 16).expect("init ring");
    }
    let layout = ShmLayout {
        frame_slot_bytes: 1024,
        atlas_bytes: 0,
        input_slots: 16,
        input_slot_bytes: 64,
    };
    let expect = HelloExpect {
        app_id: AppId::new("dev.arca.test").expect("appid"),
        instance: InstanceId::new(7),
        artifact_hash: Digest::of(b"artefacto"),
    };
    let hello = Hello {
        proto: ProtoVersion::new(1, 0),
        app_id: expect.app_id.clone(),
        instance: expect.instance,
        artifact_hash: expect.artifact_hash,
        nonce: [0xAB; 16],
    };
    let (mut host_conn, app_conn) = pair();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let app = std::thread::spawn(move || {
        let out = handshake_client(app_conn, &hello).expect("client handshake");
        // los 2 memfd llegaron por SCM_RIGHTS y son USABLES:
        let fm = ShmMap::from_fd(out.memfds[0].as_fd(), arca_shm::region_len(1024))
            .expect("map frames por fd recibido");
        let slots = unsafe { FrameSlots::from_bytes(fm.as_slice()).expect("attach") };
        let mut snap_buf = vec![0u8; 1024];
        let snap = slots
            .read_latest_into(&mut snap_buf)
            .expect("frame legible");
        let frame_val = u64::from_le_bytes(snap_buf[..8].try_into().expect("8"));
        tx.send(format!(
            "{}:{}:{}",
            out.welcome.layout.frame_slot_bytes, snap.seq, frame_val
        ))
        .expect("tx");
    });
    // el host escribe un frame ANTES del handshake para que el hijo lo lea
    {
        let m = ShmMap::from_fd(frames.as_fd(), arca_shm::region_len(1024)).expect("map");
        let slots = unsafe { FrameSlots::from_bytes(m.as_slice()).expect("attach") };
        let mut w = slots.begin_write(0).expect("write");
        w.payload().fill(0);
        w.payload()[..8].copy_from_slice(&42u64.to_le_bytes());
        w.publish().expect("publish");
    }
    let windows = [WindowSpec {
        win_id: WinId::new(1),
        size: arca_protocol::Size { w: 108, h: 240 },
        scale: 1000,
        vsync_hz: 60,
        mode: arca_protocol::WindowMode::Full,
    }];
    let ready = handshake_server(
        &mut host_conn,
        &expect,
        &[frames.raw_fd(), input.raw_fd()],
        layout,
        &[Capability::NetClient],
        &windows,
    )
    .expect("server handshake");
    assert_eq!(ready.rt_version, 1);
    let report = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("reporte hijo");
    // "1024:seq:frame42"
    let partes: Vec<&str> = report.split(':').collect();
    assert_eq!(partes[0], "1024", "layout viaja intacto");
    assert_eq!(partes[2], "42", "frame leído por el fd recibido");
    app.join().expect("join app");
}

/// Handshake con identidad maliciosa (app_id distinto) → rechazo limpio.
#[test]
fn handshake_identidad_rechazada() {
    let (mut host_conn, app_conn) = pair();
    let expect = HelloExpect {
        app_id: AppId::new("dev.arca.esperada").expect("appid"),
        instance: InstanceId::new(1),
        artifact_hash: Digest::of(b"x"),
    };
    let hello_mal = Hello {
        proto: ProtoVersion::new(1, 0),
        app_id: AppId::new("dev.arca.maliciosa").expect("appid"),
        instance: InstanceId::new(1),
        artifact_hash: Digest::of(b"x"),
        nonce: [0; 16],
    };
    let h = std::thread::spawn(move || {
        let r = handshake_client(app_conn, &hello_mal);
        assert!(r.is_err(), "el cliente debe recibir el rechazo/timeout");
    });
    let layout = ShmLayout {
        frame_slot_bytes: 1,
        atlas_bytes: 0,
        input_slots: 1,
        input_slot_bytes: 64,
    };
    let r = handshake_server(&mut host_conn, &expect, &[], layout, &[], &[]);
    assert!(r.is_err(), "el server debe rechazar app_id incorrecto");
    h.join().expect("join");
}

/// Latencia de la señal eventfd: p99 < 0.2 ms (aceptación spec 06 §6).
#[test]
fn senal_eventfd_latencia_p99() {
    let ch = SignalChannel::new().expect("eventfd");
    // mismo hilo para medir el costo del PATH (el cross-thread añade
    // scheduling del SO, que no es parte del presupuesto de syscall).
    let mut latencias = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let t0 = Instant::now();
        ch.notify().expect("notify");
        ch.wait(Duration::from_millis(100))
            .expect("wait")
            .expect("valor");
        latencias.push(t0.elapsed().as_nanos() as u64);
    }
    latencias.sort_unstable();
    let p99 = latencias[(latencias.len() as f64 * 0.99) as usize];
    assert!(p99 < 200_000, "p99 de señal: {p99} ns (presupuesto 200k)");
}

/// Frames de señal por socket (canal señal vía UDS).
#[test]
fn senal_por_socket() {
    use arca_protocol::SignalMsg;
    let (mut a, mut b) = pair();
    a.set_deadline(1_000).expect("dl a");
    b.set_deadline(1_000).expect("dl b");
    for k in 0u64..100 {
        a.send_signal(&SignalMsg::FrameTick { t_ns: k }, k)
            .expect("send sig");
    }
    let mut buf = Vec::new();
    for k in 0u64..100 {
        let (hdr, archived) = b.recv_signal(&mut buf).expect("recv sig");
        assert_eq!(hdr.seq, k);
        let s = rkyv::deserialize::<SignalMsg, rkyv::rancor::Error>(archived).expect("deser");
        assert_eq!(s, SignalMsg::FrameTick { t_ns: k });
    }
}

/// Bind + connect reales con archivo de socket en filesystem (0700).
#[test]
fn server_bind_connect_fs() {
    let dir = tmpdir("fs");
    let sock_path = dir.join("app.sock");
    let server = Server::bind(&sock_path).expect("bind");
    // permisos 0700
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&sock_path)
        .expect("stat")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700, "socket 0700: {mode:o}");
    let (tx, rx) = std::sync::mpsc::channel::<Conn>();
    let st = std::thread::spawn(move || {
        let c = server.accept(Duration::from_secs(5)).expect("accept");
        tx.send(c).expect("tx");
    });
    let mut client = Client::connect(&sock_path, Duration::from_secs(5)).expect("connect");
    let mut server_conn = rx.recv_timeout(Duration::from_secs(5)).expect("conn");
    // ping-pong por el par real
    client.set_deadline(2_000).expect("dl");
    server_conn.set_deadline(2_000).expect("dl");
    client
        .send_ctl(&ControlMsg::Ping { t_ns: 5 }, 0, &[])
        .expect("send");
    let mut buf = Vec::new();
    let mut fds = Vec::new();
    let (_, archived) = server_conn.recv_ctl(&mut buf, &mut fds).expect("recv");
    let msg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(archived).expect("deser");
    assert_eq!(msg, ControlMsg::Ping { t_ns: 5 });
    st.join().expect("join");
}
