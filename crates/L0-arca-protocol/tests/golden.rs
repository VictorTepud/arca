//! Golden bytes versionados (spec 03 §6) — wire-compat N-1.
//!
//! Reglas:
//! - `tests/fixtures/golden_v1_0.txt` es INMUTABLE: jamás editar líneas
//!   existentes; nuevos mensajes solo se AÑADEN al final (o a un fichero
//!   golden_v1_1.txt nuevo tras un minor bump).
//! - Cada línea: `name<TAB>hex-de-trama-completa(seq fija por nombre)`.
//! - El test comprueba que la trama decodifica Y que re-serializar el mensaje
//!   produce EXACTAMENTE los mismos bytes (determinismo byte a byte).
//!
//! Regeneración (solo al añadir): `ARCA_PRINT_GOLDEN=1 cargo test -p
//! arca-protocol --test golden -- --nocapture` imprime las líneas actuales.

mod common;

use arca_protocol::{decode, decode_signal, encode_into, encode_signal_into};
use arca_types::ArcaError;

/// Lee fixtures del fichero golden.
fn load_golden() -> Vec<(String, Vec<u8>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/golden_v1_0.txt"
    );
    let text = std::fs::read_to_string(path).expect("golden_v1_0.txt legible");
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, hexs) = line.split_once('\t').expect("formato name<TAB>hex");
        let bytes = hex::decode(hexs.trim()).expect("hex válido");
        out.push((name.to_owned(), bytes));
    }
    out
}

#[test]
fn golden_decode_y_reencode_identicos() {
    let golden = load_golden();
    assert!(golden.len() >= 12, "mínimo 12 fixtures ctl");
    for (name, frame) in &golden {
        if name.starts_with("sig_") {
            let (hdr, archived) =
                decode_signal(frame).unwrap_or_else(|e| panic!("{name}: decode_signal falló: {e}"));
            let msg = rkyv::deserialize::<arca_protocol::SignalMsg, rkyv::rancor::Error>(archived)
                .unwrap_or_else(|e| panic!("{name}: deserialize falló: {e}"));
            let mut again = Vec::new();
            encode_signal_into(&msg, hdr.seq, &mut again).expect("re-encode señal");
            assert_eq!(
                &again, frame,
                "{name}: re-encode difiere del golden (¡wire roto!)"
            );
        } else {
            let (hdr, archived) =
                decode(frame).unwrap_or_else(|e| panic!("{name}: decode falló: {e}"));
            let msg = rkyv::deserialize::<arca_protocol::ControlMsg, rkyv::rancor::Error>(archived)
                .unwrap_or_else(|e| panic!("{name}: deserialize falló: {e}"));
            let mut again = Vec::new();
            encode_into(&msg, hdr.seq, &mut again).expect("re-encode ctl");
            assert_eq!(
                &again, frame,
                "{name}: re-encode difiere del golden (¡wire roto!)"
            );
        }
    }
}

/// La trama golden truncada/corrupta SIEMPRE es error tipado, nunca pánico.
#[test]
fn golden_corruptas_rechazadas() {
    for (name, frame) in load_golden() {
        if name.starts_with("sig_") {
            continue; // cubierto en fuzz_lite
        }
        // truncado a la mitad
        let cut = frame.len() / 2;
        if cut >= 1 {
            match decode(&frame[..cut]) {
                Err(ArcaError::InvalidFrame(_) | ArcaError::FrameOverflow { .. }) => {}
                Err(e) => panic!("{name}: truncado dio error inesperado {e:?}"),
                Ok(_) => panic!("{name}: truncado decodificó OK (imposible)"),
            }
        }
        // byte payload corrompido → CRC debe fallar
        if frame.len() > 30 {
            let mut bad = frame.clone();
            bad[27] ^= 0xFF;
            assert!(
                matches!(decode(&bad), Err(ArcaError::InvalidFrame(_))),
                "{name}: bit-flip aceptado sin error CRC"
            );
        }
    }
}

/// Imprime las líneas golden actuales (para regenerar/añadir al fixture).
#[test]
fn print_golden() {
    if std::env::var("ARCA_PRINT_GOLDEN").is_err() {
        return;
    }
    for i in 0..12 {
        let (name, msg) = common::representative(i);
        let mut buf = Vec::new();
        encode_into(&msg, seq_for(&name), &mut buf).expect("encode");
        println!("{name}\t{}", hex::encode(&buf));
    }
    for (name, sig) in common::representative_signals() {
        let mut buf = Vec::new();
        encode_signal_into(&sig, seq_for(name), &mut buf).expect("encode sig");
        println!("sig_{name}\t{}", hex::encode(&buf));
    }
}

/// Seq fija por nombre (determinismo del fixture).
fn seq_for(name: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h % 1000
}
