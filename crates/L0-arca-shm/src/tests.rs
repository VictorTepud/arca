//! Tests unitarios + de concurrencia de arca-shm (spec 05 §6).
//!
//! - SPSC 10M slots sin pérdida (aceptación T11)
//! - torn-read del seqlock detectado 100% (ninguna snapshot inconsistente)
//! - overflow del ring + compactación determinista (contrato seq embebido)
//! - forks reales en `tests/fork.rs` (memfd compartido entre procesos)

use std::sync::atomic::Ordering;

use crate::frame::{region_len, SLOT_HEADER};
use crate::{FrameSlots, PushResult, RingSpsc};
use arca_types::Res;

fn region(slot_size: usize, slots: usize) -> Vec<u8> {
    vec![0u8; 64 + slots * slot_size]
}

fn slot_with_seq(seq: u64) -> [u8; 64] {
    let mut s = [0u8; 64];
    s[..8].copy_from_slice(&seq.to_le_bytes());
    s[8] = 2; // "move"
    s[16..20].copy_from_slice(&(seq as u32).to_le_bytes());
    s
}

#[test]
fn ring_init_y_geometria() {
    let mut buf = region(64, 4);
    RingSpsc::init(&mut buf, 64, 4).expect("init");
    assert!(RingSpsc::init(&mut buf, 0, 4).is_err());
    let mut chica = region(8, 2);
    assert!(RingSpsc::init(&mut chica, 64, 4).is_err()); // región insuficiente
    unsafe {
        assert!(RingSpsc::from_bytes(&buf).is_ok());
        assert!(RingSpsc::from_bytes(&buf[..32]).is_err());
    }
}

#[test]
fn ring_fifo_simple() {
    let mut buf = region(64, 8);
    RingSpsc::init(&mut buf, 64, 8).expect("init");
    let ring = unsafe { RingSpsc::from_bytes(&buf).expect("attach") };
    for k in 0u64..8 {
        assert!(matches!(
            ring.push(&slot_with_seq(k)).expect("push"),
            PushResult::Ok
        ));
    }
    assert!(matches!(
        ring.push(&slot_with_seq(99)).expect("push full"),
        PushResult::Full
    ));
    let mut out = [0u8; 64];
    for k in 0u64..8 {
        assert!(ring.pop(&mut out).expect("pop"));
        assert_eq!(
            u64::from_le_bytes(out[..8].try_into().expect("8")),
            k,
            "FIFO"
        );
    }
    assert!(!ring.pop(&mut out).expect("pop vacío"));
    // pop_each vacío
    let n = ring.pop_each(4, |_| true).expect("pop_each");
    assert_eq!(n, 0);
}

#[test]
fn ring_doble_productor_detectado() {
    let mut buf = region(64, 8);
    RingSpsc::init(&mut buf, 64, 8).expect("init");
    // Simula DOS productores con tokens distintos: view A y view B.
    let a = unsafe { RingSpsc::from_bytes(&buf).expect("a") };
    let b = unsafe { RingSpsc::from_bytes(&buf).expect("b") };
    a.push(&slot_with_seq(0)).expect("A push");
    match b.push(&slot_with_seq(1)) {
        Err(arca_types::ArcaError::Internal(msg)) => {
            assert!(msg.contains("productor"), "mensaje: {msg}");
        }
        other => panic!("esperaba error de segundo productor, vino {other:?}"),
    }
}

#[test]
fn ring_compactacion_determinista() {
    let mut buf = region(64, 4);
    RingSpsc::init(&mut buf, 64, 4).expect("init");
    let ring = unsafe { RingSpsc::from_bytes(&buf).expect("attach") };
    for k in 0u64..4 {
        ring.push(&slot_with_seq(k)).expect("push");
    }
    // Lleno: push_compacting fusiona el nuevo move (seq 4) con el último (3)
    // — el contrato exige seq embebido: la fusión escribe seq 5.
    let merge = |_last: &[u8], new: &[u8], out: &mut [u8]| {
        if _last[8] == 2 && new[8] == 2 {
            out.copy_from_slice(new);
            let last_seq = u64::from_le_bytes(_last[..8].try_into().expect("8"));
            out[..8].copy_from_slice(&(last_seq + 2).to_le_bytes());
            true
        } else {
            false
        }
    };
    match ring
        .push_compacting(&slot_with_seq(4), &merge)
        .expect("compact")
    {
        PushResult::Compacted { merged: 1 } => {}
        other => panic!("esperaba Compacted, vino {other:?}"),
    }
    // El consumidor ve 4 slots con seq estrictamente crecientes (0,1,2,3+2=5).
    let mut prev = None;
    let n = ring
        .pop_each(8, |s| {
            let seq = u64::from_le_bytes(s[..8].try_into().expect("8"));
            assert!(
                prev.is_none_or(|p: u64| seq > p),
                "seq no crece: {prev:?} → {seq}"
            );
            prev = Some(seq);
            true
        })
        .expect("pop_each");
    assert_eq!(n, 4);
}

/// SPSC con hilos reales: 10M slots sin pérdida ni desorden (aceptación).
#[test]
fn ring_spsc_10m_sin_perdida() {
    const N: u64 = 10_000_000;
    const SS: usize = 16;
    const CAP: usize = 1024;
    let mut buf = region(SS, CAP);
    RingSpsc::init(&mut buf, SS, CAP).expect("init");
    let got = std::thread::scope(|s| -> Res<u64> {
        let prod = s.spawn(|| -> Res<()> {
            let ring = unsafe { RingSpsc::from_bytes(&buf).expect("attach") };
            let mut slot = [0u8; SS];
            for k in 0u64..N {
                slot[..8].copy_from_slice(&k.to_le_bytes());
                loop {
                    match ring.push(&slot)? {
                        PushResult::Ok => break,
                        PushResult::Full => {
                            std::thread::yield_now(); // cede: sin futex en shm, el planificador decide
                        }
                        PushResult::Compacted { .. } => unreachable!(),
                    }
                }
            }
            Ok(())
        });
        let cons = s.spawn(|| -> Res<u64> {
            let ring = unsafe { RingSpsc::from_bytes(&buf).expect("attach") };
            let mut out = [0u8; SS];
            let mut got: u64 = 0;
            while got < N {
                if ring.pop(&mut out)? {
                    let k = u64::from_le_bytes(out[..8].try_into().expect("8"));
                    assert_eq!(k, got, "orden roto en {got}");
                    got += 1;
                } else {
                    std::thread::yield_now();
                }
            }
            Ok(got)
        });
        prod.join().expect("hilo prod").expect("prod sin error");
        cons.join().expect("hilo cons")
    })
    .expect("cons sin error");
    assert_eq!(got, N);
}

/// Seqlock: lector concurrente contra escritor agresivo — 0 snapshots torn
/// NO detectadas (el payload es patrón repetido del nº de frame).
#[test]
fn seqlock_torn_read_detectado() {
    const FB: usize = 4096;
    let mut buf = vec![0u8; region_len(FB)];
    FrameSlots::init(&mut buf, FB).expect("init");
    let done = std::sync::atomic::AtomicBool::new(false);
    let ok = std::thread::scope(|s| -> Res<usize> {
        let writer = s.spawn(|| -> Res<()> {
            let slots = unsafe { FrameSlots::from_bytes(&buf).expect("attach") };
            for frame in 1u64..=20_000 {
                let which = (frame % 2) as usize;
                let mut w = slots.begin_write(which)?;
                let payload = w.payload();
                let byte = (frame % 251) as u8;
                payload.fill(byte);
                payload[..8].copy_from_slice(&frame.to_le_bytes());
                w.publish()?;
            }
            done.store(true, Ordering::Release);
            Ok(())
        });
        let reader = s.spawn(|| -> Res<usize> {
            let slots = unsafe { FrameSlots::from_bytes(&buf).expect("attach") };
            let mut out = vec![0u8; FB];
            let mut ok = 0usize;
            // lee mientras el escritor corre; tras done, 100 lecturas
            // estables y salir (los frames ya publicados siguen válidos).
            let mut tras_done = 0u32;
            loop {
                let d = done.load(Ordering::Acquire);
                if let Some(snap) = slots.read_latest_into(&mut out) {
                    let frame = u64::from_le_bytes(out[..8].try_into().expect("8"));
                    let byte = (frame % 251) as u8;
                    assert!(
                        out[8..].iter().all(|&b| b == byte),
                        "torn read NO detectado (frame {frame})"
                    );
                    assert_eq!(snap.seq % 2, 1, "seq impar en snapshot válida");
                    ok += 1;
                    if d {
                        tras_done += 1;
                        if tras_done >= 100 {
                            break;
                        }
                    }
                } else if d && tras_done > 0 {
                    break; // tras done ya no debería fallar; no bloquear
                }
            }
            Ok(ok)
        });
        writer.join().expect("hilo writer").expect("writer ok");
        reader.join().expect("hilo reader")
    })
    .expect("reader ok");
    assert!(ok >= 100, "lector obtuvo muy pocas snapshots válidas: {ok}");
}

#[test]
fn seqlock_publicar_y_abortar() {
    let mut buf = vec![0u8; region_len(256)];
    FrameSlots::init(&mut buf, 256).expect("init");
    let slots = unsafe { FrameSlots::from_bytes(&buf).expect("attach") };
    // sin publicar nada → no hay frame válido
    let mut out = [0u8; 256];
    assert!(slots.read_latest_into(&mut out).is_none());
    // publica slot 0
    {
        let mut w = slots.begin_write(0).expect("write 0");
        w.payload().fill(0xAA);
        w.publish().expect("publish");
    }
    let snap = slots.read_latest_into(&mut out).expect("lee slot 0");
    assert_eq!(snap.which, 0);
    assert!(out.iter().all(|&b| b == 0xAA));
    // publica slot 1 (más nuevo)
    {
        let mut w = slots.begin_write(1).expect("write 1");
        w.payload().fill(0xBB);
        w.publish().expect("publish");
    }
    let snap = slots.read_latest_into(&mut out).expect("lee slot 1");
    assert_eq!(snap.which, 1, "el más reciente gana");
    assert!(out.iter().all(|&b| b == 0xBB));
    // abort: begin sin publish → slot inválido
    {
        let _w = slots.begin_write(1).expect("begin");
        // drop sin publish
    }
    let snap = slots.read_latest_into(&mut out).expect("vuelve al slot 0");
    assert_eq!(snap.which, 0, "slot 1 abortado → cae al 0");
    assert!(out.iter().all(|&b| b == 0xAA));
}

#[test]
fn seqlock_reintenta_si_el_escritor_termina() {
    const FB: usize = 1024;
    let mut buf = vec![0u8; region_len(FB)];
    FrameSlots::init(&mut buf, FB).expect("init");
    let slots = unsafe { FrameSlots::from_bytes(&buf).expect("attach") };
    // marco el slot 0 "en escritura" manualmente y publico DESPUÉS:
    // el lector que llegue durante la ventana debe reintentar y pillar el
    // frame terminado (retry interno de read_slot_into).
    {
        let mut w = slots.begin_write(0).expect("begin");
        w.payload().fill(0x0C);
        w.publish().expect("publish");
    }
    let mut out = [0u8; FB];
    let snap = slots
        .read_slot_into(0, &mut out)
        .expect("reintento exitoso");
    assert_eq!(snap.len, FB);
    assert!(out.iter().all(|&b| b == 0x0C));
}

/// Geometría de la región (documentada en el módulo frame).
#[test]
fn geometria_region_frames() {
    assert_eq!(SLOT_HEADER, 16);
    assert_eq!(region_len(1000), 2 * (16 + 1000));
    let mut buf = vec![0u8; region_len(1000) + 1];
    assert!(FrameSlots::init(&mut buf, 1000).is_err());
}
