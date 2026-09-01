//! Reloj monotónico crudo, comparable ENTRE procesos (spec 01 §3).
//!
//! `std::time::Instant` no sirve: cada proceso tiene su propio origen y usa
//! CLOCK_MONOTONIC (afectado por NTP). Para medir latencias AIPC host↔sub-app
//! hace falta `CLOCK_MONOTONIC_RAW` (mismo origen en todo el sistema, sin
//! ajustes). Ambos procesos llaman esta misma función.

/// Nanosegundos de `CLOCK_MONOTONIC_RAW` (sin origen definido: solo deltas).
///
/// # Panics (debug_assert)
/// Nunca: `clock_gettime(CLOCK_MONOTONIC_RAW)` no falla en Linux/Android.
#[must_use]
pub fn now_mono_ns() -> u64 {
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    const CLOCK_MONOTONIC_RAW: i32 = 4; // idéntico en Linux y Android (bionic)
    extern "C" {
        fn clock_gettime(clk_id: i32, tp: *mut Timespec) -> i32;
    }
    // SAFETY: `clock_gettime` es async-signal-safe, no toca memoria más allá
    // del puntero `tp` (struct timespec de 16 bytes, pasado por referencia
    // válida). El FFI existe para evitar la dependencia `libc` en este crate
    // raíz (spec 01 §2: dependencias permitidas mínimas).
    let mut ts = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { clock_gettime(CLOCK_MONOTONIC_RAW, &mut ts) };
    debug_assert_eq!(
        rc, 0,
        "CLOCK_MONOTONIC_RAW es siempre exitoso en Linux/Android"
    );
    // El campo nunca es negativo para este reloj; el `max` es defensa estática.
    (ts.tv_sec.max(0) as u64) * 1_000_000_000 + (ts.tv_nsec.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotono_no_retrocede() {
        // Resolución de ns: dos llamadas seguidas pueden ser iguales, nunca
        // menores. Con 10k iteraciones se fuerza al menos un incremento.
        let mut prev = now_mono_ns();
        let mut incrementos = 0;
        for _ in 0..10_000 {
            let now = now_mono_ns();
            assert!(now >= prev, "reloj retrocedió: {prev} > {now}");
            if now > prev {
                incrementos += 1;
            }
            prev = now;
        }
        assert!(incrementos > 0);
    }

    #[test]
    fn duracion_razonable() {
        let a = now_mono_ns();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = now_mono_ns();
        let delta_ms = (b - a) / 1_000_000;
        assert!(
            (4..=200).contains(&delta_ms),
            "delta fuera de rango: {delta_ms} ms"
        );
    }
}
