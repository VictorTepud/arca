//! Fuzz-lite de decode (adaptación PC del fuzz nocturno, spec 03 §6):
//! bytes random + mutaciones de tramas válidas → NUNCA pánico; error tipado
//! o (probabilidad ínfima) un decode válido aceptado.

mod common;

use arca_protocol::{decode, decode_signal, encode_into, encode_signal_into};

#[test]
fn fuzz_lite_decode_200k() {
    let mut seed = 0xF00D_u64;
    // 1) 100k buffers puramente random (longitudes 0..80)
    for i in 0..100_000u64 {
        let n = (common::next(&mut seed) % 80) as usize;
        let buf: Vec<u8> = (0..n).map(|_| common::next(&mut seed) as u8).collect();
        let _ = decode(&buf); // nunca debe pánico
        let _ = decode_signal(&buf);
        if i % 1000 == 0 {
            std::hint::black_box(&buf);
        }
    }
    // 2) 100k mutaciones de tramas válidas (bit-flips/truncados/extendidas)
    let mut frame = Vec::new();
    encode_into(&common::representative(3).1, 42, &mut frame).expect("encode");
    let mut sigframe = Vec::new();
    encode_signal_into(
        &arca_protocol::SignalMsg::FrameTick { t_ns: 12345 },
        42,
        &mut sigframe,
    )
    .expect("encode sig");
    for i in 0..100_000u64 {
        let r = common::next(&mut seed);
        let base = if r % 2 == 0 { &frame } else { &sigframe };
        let mut bad = base.clone();
        match r % 4 {
            0 => {
                let pos = (common::next(&mut seed) as usize) % bad.len();
                bad[pos] ^= 1u8 << (common::next(&mut seed) % 8);
            }
            1 => {
                let cut = (common::next(&mut seed) as usize) % bad.len();
                bad.truncate(cut);
            }
            2 => bad.extend_from_slice(&(common::next(&mut seed)).to_le_bytes()),
            _ => {
                let pos = (common::next(&mut seed) as usize) % bad.len();
                bad[pos] = common::next(&mut seed) as u8;
            }
        }
        let _ = decode(&bad); // nunca pánico
        let _ = decode_signal(&bad);
        if i % 1000 == 0 {
            std::hint::black_box(&bad);
        }
    }
}
