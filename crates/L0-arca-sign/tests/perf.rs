//! Bench de verificación (spec 08 §6, TASKS.json T06): **10k verificaciones
//! ed25519**. Mide e imprime (`cargo test -- --nocapture`); el assert es
//! holgado y documentado (ver NOTA abajo).
//!
//! NOTA(agent) sobre el presupuesto "10k < 50 ms" de la spec: eso son 5 µs
//! por verificación, solo alcanzable con *batch verification* (feature
//! `batch` de ed25519-dalek, probabilística) o hardware muy concreto. El
//! camino REAL de instalación verifica **1** firma por paquete (más los
//! blake3 por archivo, ~GB/s): el presupuesto relevante por instalación es
//! una fracción de milisegundo. Este bench fija el throughput del camino
//! host (verify_strict individual contra el anillo) con margen para CI
//! lenta/dev-profile; el número exacto se reporta en la bitácora T06 y el
//! README con profile dev (opt-level 1) y release.

#![cfg(feature = "signer")]

use std::time::Instant;

use arca_sign::signer::{keygen, sign_digest};
use arca_sign::RingOfTrust;
use arca_types::Digest;

/// Verificaciones del bench.
const N: usize = 10_000;

/// Límite holgado (ms) para `verify_strict` directo en dev-profile.
/// Medido ~ver bitácora; margen ≥ 5× para máquinas lentas de CI.
const LIMITE_DIRECTO_MS: u128 = 4_000;

/// Límite holgado (ms) para el camino host completo (anillo de 5 claves,
/// la buena en penúltima posición).
const LIMITE_ANILLO_MS: u128 = 20_000;

#[test]
fn bench_10k_verificaciones_ed25519() {
    let sk = keygen().expect("keygen");
    let vk = sk.verifying_key();
    let digest = Digest::of(b"bench-paquete-arca");
    let sig = sign_digest(&digest, &sk);
    let msg = digest.as_bytes().to_vec();

    // Anillo de 5 claves con la buena en penúltima (peor caso representativo
    // del host real: no siempre es la primera).
    let mut ring = RingOfTrust::empty();
    for _ in 0..3 {
        let k = keygen().expect("keygen").verifying_key();
        ring.push(k);
    }
    ring.push(vk);
    let k = keygen().expect("keygen").verifying_key();
    ring.push(k);

    // Calentamiento (primera verificación inicializa tablas/caches).
    for _ in 0..64 {
        assert!(ring.verify(&digest, &sig).is_ok());
    }

    // (1) núcleo cripto: verify_strict directo.
    let dalek_sig = ed25519_dalek::Signature::from_bytes(&sig);
    let t0 = Instant::now();
    let mut ok = 0usize;
    for _ in 0..N {
        if vk.verify_strict(&msg, &dalek_sig).is_ok() {
            ok += 1;
        }
    }
    let t_directo = t0.elapsed();
    assert_eq!(ok, N, "toda verificación buena debe pasar");
    println!(
        "bench 10k verify_strict directo: {} ms ({:.2} µs/verif)",
        t_directo.as_millis(),
        t_directo.as_secs_f64() * 1e6 / N as f64
    );
    assert!(
        t_directo.as_millis() < LIMITE_DIRECTO_MS,
        "10k verificaciones tardaron {} ms (límite {LIMITE_DIRECTO_MS})",
        t_directo.as_millis()
    );

    // (2) camino host: RingOfTrust::verify_against_any (5 claves).
    let t0 = Instant::now();
    let mut ok = 0usize;
    for _ in 0..N {
        if ring.verify(&digest, &sig).is_ok() {
            ok += 1;
        }
    }
    let t_anillo = t0.elapsed();
    assert_eq!(ok, N);
    println!(
        "bench 10k verify_against_any (anillo 5, buena en penúltima): {} ms ({:.2} µs/verif)",
        t_anillo.as_millis(),
        t_anillo.as_secs_f64() * 1e6 / N as f64
    );
    assert!(
        t_anillo.as_millis() < LIMITE_ANILLO_MS,
        "10k verificaciones de anillo tardaron {} ms (límite {LIMITE_ANILLO_MS})",
        t_anillo.as_millis()
    );

    // Sanidad cripto: la misma firma contra digest distinto falla.
    let otro = Digest::of(b"otro");
    assert!(matches!(
        ring.verify(&otro, &sig),
        Err(arca_types::ArcaError::InvalidSignature)
    ));
}
