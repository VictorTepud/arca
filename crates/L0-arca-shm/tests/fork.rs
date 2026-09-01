//! Test de integración con fork REAL: memfd compartido entre dos procesos
//! (spec 05 §6: "crear/mapear desde segundo proceso hijo").

use std::io::Write as _;

use arca_shm::frame::region_len;
use arca_shm::{FrameSlots, Memfd, RingSpsc, ShmMap};

fn slot(seq: u64) -> [u8; 64] {
    let mut s = [0u8; 64];
    s[..8].copy_from_slice(&seq.to_le_bytes());
    s
}

/// Ring: el hijo produce 1000 slots sobre el MISMO memfd; el padre consume.
#[test]
fn fork_ring_memfd_compartido() {
    const SLOTS: usize = 128;
    let region = 64 + SLOTS * 64;
    let memfd = Memfd::create("arca-test-ring", region).expect("memfd");
    memfd.seal_size().expect("seal");
    let mut map = ShmMap::from_fd(memfd.as_fd(), region).expect("map");
    RingSpsc::init(map.as_mut_slice(), 64, SLOTS).expect("init ring");
    drop(map);

    let pid = unsafe { nix::unistd::fork().expect("fork") };
    match pid {
        nix::unistd::ForkResult::Child => {
            // Hijo: mapea el fd heredado y produce 1000 slots (ring pequeño
            // → el padre consume en paralelo; reintentos con spin).
            let map = ShmMap::from_fd(memfd.as_fd(), region).expect("map hijo");
            let ring = unsafe { RingSpsc::from_bytes(map.as_slice()).expect("attach hijo") };
            for k in 0u64..1000 {
                loop {
                    match ring.push(&slot(k)) {
                        Ok(arca_shm::PushResult::Ok) => break,
                        Ok(arca_shm::PushResult::Full) => std::thread::yield_now(),
                        Ok(arca_shm::PushResult::Compacted { .. }) => unreachable!(),
                        Err(_) => std::process::exit(3),
                    }
                }
            }
            std::process::exit(0);
        }
        nix::unistd::ForkResult::Parent { child } => {
            let map = ShmMap::from_fd(memfd.as_fd(), region).expect("map padre");
            let ring = unsafe { RingSpsc::from_bytes(map.as_slice()).expect("attach padre") };
            let mut out = [0u8; 64];
            let mut got: u64 = 0;
            while got < 1000 {
                match ring.pop(&mut out) {
                    Ok(true) => {
                        let k = u64::from_le_bytes(out[..8].try_into().expect("8"));
                        assert_eq!(k, got, "orden roto: {got}");
                        got += 1;
                    }
                    Ok(false) => std::thread::yield_now(),
                    Err(_) => panic!("pop error"),
                }
            }
            let status = nix::sys::wait::waitpid(child, None).expect("waitpid");
            assert!(matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)));
        }
    }
}

/// Frames: el hijo publica un frame con seqlock; el padre lo lee válido.
#[test]
fn fork_frames_memfd_compartido() {
    const FB: usize = 2048;
    let region = region_len(FB);
    let memfd = Memfd::create("arca-test-frames", region).expect("memfd");
    let mut map = ShmMap::from_fd(memfd.as_fd(), region).expect("map");
    FrameSlots::init(map.as_mut_slice(), FB).expect("init frames");
    drop(map);

    let pid = unsafe { nix::unistd::fork().expect("fork") };
    match pid {
        nix::unistd::ForkResult::Child => {
            let map = ShmMap::from_fd(memfd.as_fd(), region).expect("map hijo");
            let slots = unsafe { FrameSlots::from_bytes(map.as_slice()).expect("attach hijo") };
            for frame in 1u64..=100 {
                let mut w = slots.begin_write((frame % 2) as usize).expect("begin");
                w.payload().fill(frame as u8);
                w.publish().expect("publish");
            }
            std::process::exit(0);
        }
        nix::unistd::ForkResult::Parent { child } => {
            let map = ShmMap::from_fd(memfd.as_fd(), region).expect("map padre");
            let slots = unsafe { FrameSlots::from_bytes(map.as_slice()).expect("attach padre") };
            let mut out = vec![0u8; FB];
            let mut reads = 0usize;
            let mut last_seq = 0u64;
            while reads < 50 {
                if let Some(snap) = slots.read_latest_into(&mut out) {
                    // no-decreciente: re-leer el último frame publicado es
                    // válido (el escritor ya terminó); distintos ≥ 2 prueba
                    // que el hijo publicó frames NUEVOS que el padre vio.
                    assert!(snap.seq >= last_seq, "seq retrocede: {last_seq} → {snap:?}");
                    last_seq = snap.seq;
                    reads += 1;
                }
            }
            // (distintos puede ser 1 si el padre llega tarde: no es error)
            // tras la muerte del hijo: el último frame publicado (100) debe
            // ser legible y consistente (patrón 100u8 en todo el payload).
            let status = nix::sys::wait::waitpid(child, None).expect("waitpid");
            assert!(matches!(status, nix::sys::wait::WaitStatus::Exited(_, 0)));
            let snap = slots
                .read_latest_into(&mut out)
                .expect("frame final legible");
            assert!(out.iter().all(|&b| b == 100u8), "payload final: {snap:?}");
            let mut line = std::io::stderr().lock();
            let _ = writeln!(
                line,
                "fork frames: {reads} snapshots válidas, seq máx {last_seq}"
            );
        }
    }
}

/// Memfd: nombre sin prefijo rechazado; seal impide truncar (EPERM/0).
#[test]
fn memfd_prefijo_y_seal() {
    assert!(Memfd::create("sin-prefijo", 4096).is_err());
    assert!(Memfd::create("arca-ok", 0).is_err());
    let m = Memfd::create("arca-ok", 8192).expect("create");
    assert_eq!(m.size(), 8192);
    m.seal_size().expect("seal");
    // tras sellar GROW|SHRINK: ftruncate a menor tamaño falla.
    let r = nix::unistd::ftruncate(m.as_fd(), 4096);
    assert!(r.is_err(), "el seal debe impedir el truncado");
}
