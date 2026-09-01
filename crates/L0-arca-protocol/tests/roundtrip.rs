//! Roundtrip 10k mensajes + framing en stream + señales eventfd + límites.

mod common;

use arca_protocol::{
    decode, decode_signal, decode_signal_wire, encode_into, encode_signal_into, encode_signal_wire,
    ControlMsg, MsgHeader, HEADER_LEN, MAX_CTL_PAYLOAD,
};
use arca_types::ArcaError;
use common::gen_control_msg;

/// Roundtrip de 10k mensajes pseudo-aleatorios deterministas (DoD T10).
/// encode → decode → deserialize → igualdad; y re-encode byte a byte.
#[test]
fn roundtrip_10k_mensajes() {
    let mut seed = 0x0D0D_CAFE_0000_0042_u64;
    for k in 0..10_000u64 {
        let msg = gen_control_msg(&mut seed);
        let mut buf = Vec::new();
        encode_into(&msg, k, &mut buf).expect("encode");
        let (hdr, archived) = decode(&buf).expect("decode");
        assert_eq!(hdr.seq, k, "seq debe ser round-trip");
        assert_eq!(hdr.length as usize + HEADER_LEN, buf.len());
        let back: ControlMsg =
            rkyv::deserialize::<_, rkyv::rancor::Error>(archived).expect("deserialize");
        assert_eq!(back, msg, "mensaje {k} difiere tras roundtrip");
        let mut re = Vec::new();
        encode_into(&back, k, &mut re).expect("re-encode");
        assert_eq!(re, buf, "re-encode {k} no es byte a byte");
    }
}

/// Varios mensajes concatenados en un buffer: el transporte recorta por
/// header.length y decodifica uno a uno sin pérdida de orden.
#[test]
fn framing_stream_multimensaje() {
    let mut seed = 7;
    let mut stream = Vec::new();
    let mut msgs = Vec::new();
    for k in 0..50u64 {
        let msg = gen_control_msg(&mut seed);
        msgs.push(msg);
        encode_into(msgs.last().expect("no vacío"), k, &mut stream).expect("encode");
    }
    let mut off = 0usize;
    for (k, want) in msgs.iter().enumerate() {
        let hdr = MsgHeader::parse(&stream[off..]).expect("header del frame k");
        let len = HEADER_LEN + hdr.length as usize;
        let (hdr2, archived) = decode(&stream[off..off + len]).expect("decode k");
        assert_eq!(hdr2.seq, k as u64);
        let got: ControlMsg =
            rkyv::deserialize::<_, rkyv::rancor::Error>(archived).expect("deserialize");
        assert_eq!(&got, want, "frame {k} del stream difiere");
        off += len;
    }
    assert_eq!(off, stream.len(), "el stream se consume exacto");
}

/// Payloads de control que exceden MAX_CTL_PAYLOAD → error tipado, nunca
/// truncamiento silencioso (spec 03 §4).
#[test]
fn overflow_rechazado() {
    let msg = ControlMsg::SvcRequest {
        req_id: 1,
        cap: arca_types::Capability::NetClient,
        payload: vec![0u8; MAX_CTL_PAYLOAD + 1],
    };
    match encode_into(&msg, 0, &mut Vec::new()) {
        Err(ArcaError::FrameOverflow { bytes, limit }) => {
            assert_eq!(limit, MAX_CTL_PAYLOAD);
            assert!(bytes > limit);
        }
        other => panic!("esperaba FrameOverflow, vino {other:?}"),
    }
    // length mentirosa en el header (> MAX) rechazada en parse
    let mut frame = Vec::new();
    encode_into(&ControlMsg::Pause, 0, &mut frame).expect("encode");
    let bogus_len = (MAX_CTL_PAYLOAD + 10) as u32;
    frame[10..14].copy_from_slice(&bogus_len.to_le_bytes());
    assert!(matches!(
        decode(&frame),
        Err(ArcaError::FrameOverflow { .. })
    ));
}

/// Canal cruzado: trama ctl al decodificador de señal y viceversa → error.
#[test]
fn canal_cruzado_rechazado() {
    let mut ctl = Vec::new();
    encode_into(&ControlMsg::Ping { t_ns: 1 }, 0, &mut ctl).expect("encode");
    assert!(decode_signal(&ctl).is_err());
    let mut sig = Vec::new();
    encode_signal_into(&arca_protocol::SignalMsg::Busy, 0, &mut sig).expect("encode sig");
    assert!(decode(&sig).is_err());
}

/// Señal eventfd (u64 taggeado): roundtrip de los 5 kinds + tag inválido.
#[test]
fn signal_wire_eventfd() {
    use arca_protocol::SignalMsg::*;
    // t_ns/frame_seq son valores MONOTÓNICOS realistas: 56 bits de payload
    // cubren 2^56 ns ≈ 2.28 años de uptime (mucho más que cualquier Android).
    let casos = [
        FrameReady {
            frame_seq: 123_456_789,
        },
        FrameTick {
            t_ns: 3_600_000_000_000,
        }, // 1 h de uptime
        Busy,
        Idle,
        Pong {
            t_ns: 0x00ff_ffff_ffff_ffff,
        }, // límite exacto 56 bits
    ];
    for s in casos {
        assert_eq!(
            decode_signal_wire(encode_signal_wire(&s)).expect("roundtrip"),
            s
        );
    }
    // tag desconocido (kind 0 / 6 / 255) → error tipado
    for bad in [0u64, 6 << 56, 255 << 56, u64::MAX] {
        if bad == u64::MAX && decode_signal_wire(bad).is_ok() {
            panic!("u64::MAX no puede ser tag válido"); // kind 255: inválido
        }
        assert!(decode_signal_wire(bad).is_err(), "tag {bad:#x} aceptado");
    }
    // payload truncado a 56 bits: FrameTick(t_ns enorme) se recorta determinista
    let big = FrameTick { t_ns: u64::MAX };
    let v = encode_signal_wire(&big);
    let back = decode_signal_wire(v).expect("ok");
    match back {
        FrameTick { t_ns } => assert_eq!(t_ns, 0x00ff_ffff_ffff_ffff, "recorte a 56 bits"),
        _ => panic!("kind inesperado"),
    }
}

/// Multi-ventana: Attach con N ventanas mantiene ventanas independientes.
#[test]
fn attach_multiventana() {
    use arca_protocol::{Attach, Size, WindowMode, WindowSpec};
    use arca_types::WinId;
    let specs: Vec<_> = (1..=3u32)
        .map(|k| WindowSpec {
            win_id: WinId::new(k),
            size: Size {
                w: 100 * k,
                h: 200 * k,
            },
            scale: 1000 * k,
            vsync_hz: 60,
            mode: WindowMode::Tile,
        })
        .collect();
    let msg = ControlMsg::Attach(Attach { windows: specs });
    let mut buf = Vec::new();
    encode_into(&msg, 3, &mut buf).expect("encode");
    let (_, archived) = decode(&buf).expect("decode");
    let back: ControlMsg =
        rkyv::deserialize::<_, rkyv::rancor::Error>(archived).expect("deserialize");
    match back {
        ControlMsg::Attach(a) => {
            assert_eq!(a.windows.len(), 3);
            for (i, w) in a.windows.iter().enumerate() {
                assert_eq!(w.win_id.get(), i as u32 + 1);
                assert_eq!(w.size.w, 100 * (i as u32 + 1));
            }
        }
        _ => panic!("variante inesperada"),
    }
}

/// Determinismo: mismo mensaje + mismo seq → mismos bytes (dobles runs).
#[test]
fn encode_determinista() {
    let mut seed = 99;
    for _ in 0..200 {
        let msg = gen_control_msg(&mut seed);
        let mut a = Vec::new();
        let mut b = Vec::new();
        encode_into(&msg, 5, &mut a).expect("encode a");
        encode_into(&msg, 5, &mut b).expect("encode b");
        assert_eq!(a, b, "encode no determinista");
    }
}
