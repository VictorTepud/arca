//! Fuzz-lite (TASKS.json T04 acceptance): 1000 mutaciones aleatorias del
//! golden con semilla fija → `Manifest::parse`/`validate_layout` sin pánicos.
//!
//! El PRNG es un xorshift64* propio (determinista y sin dependencias):
//! u64 con wrapping ops es independiente de la plataforma.

mod common;

use arca_pkg_model::{Manifest, RelPath};
use common::{golden_entries, GOLDEN};

/// Semilla fija (documentada para reproducibilidad del fuzz-lite).
const SEED: u64 = 0x0D15_EA5E_FEED_C0DE;

/// xorshift64* determinista.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // |1: el xorshift jamás recibe 0.
        Self(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Entero uniforme en `0..n` (n ≥ 1).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n.max(1) as u64)) as usize
    }
}

/// Aplica 1..3 mutaciones de bytes sobre el golden (flip, overwrite,
/// truncar, duplicar chunk, borrar chunk, insertar byte).
fn mutate(golden: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut b = golden.to_vec();
    let ops = 1 + rng.below(3);
    for _ in 0..ops {
        if b.is_empty() {
            break;
        }
        match rng.below(6) {
            0 => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.below(b.len());
                b[i] = (rng.next_u64() & 0xFF) as u8;
            }
            2 => {
                let i = rng.below(b.len());
                b.truncate(i);
            }
            3 => {
                // duplicar un chunk corto en otra posición
                let i = rng.below(b.len());
                let l = 1 + rng.below((b.len() - i).min(32));
                let chunk: Vec<u8> = b[i..i + l].to_vec();
                let j = rng.below(b.len());
                let mut out: Vec<u8> = b[..j].to_vec();
                out.extend_from_slice(&chunk);
                out.extend_from_slice(&b[j..]);
                b = out;
            }
            4 => {
                // borrar un chunk corto
                let i = rng.below(b.len());
                let l = 1 + rng.below((b.len() - i).min(16));
                b.drain(i..i + l);
            }
            _ => {
                let i = rng.below(b.len() + 1);
                b.insert(i, (rng.next_u64() & 0xFF) as u8);
            }
        }
    }
    b
}

#[test]
fn fuzz_lite_1000_mutaciones_de_manifest_sin_panicos() {
    let golden = GOLDEN.as_bytes();
    let entries = golden_entries();
    let mut rng = Rng::new(SEED);
    let mut oks = 0usize;
    for i in 0..1000 {
        let mutated = mutate(golden, &mut rng);
        // La invarianta (spec 02 §4): total, jamás pánico. Un pánico aquí
        // revienta el test: eso es exactamente lo que buscamos.
        match Manifest::parse_detailed(&mutated) {
            Ok(man) => {
                oks += 1;
                // Los manifests mutados que sobreviven también ejercitan el
                // layout (contra las entradas golden: errores permitidos).
                let _ = man.validate_layout(&entries);
            }
            Err(e) => {
                // kind() es const y exhaustivo: tampoco puede entrar en pánico.
                let _ = e.kind();
            }
        }
        // sanity: el PRNG avanza (semilla fija ⇒ reproducible)
        assert!(rng.0 != 0, "iteración {i}: PRNG degenerado");
    }
    // Sanity anti-círculo vicio: con mutaciones de bytes, la gran mayoría
    // NO puede parsear (TOML estricto). Si TODO parseara, el fuzz no testea
    // nada. Semilla fija ⇒ determinista, no puede flakear.
    assert!(
        oks < 1000,
        "demasiados parseables ({oks}/1000): fuzz inútil"
    );
}

#[test]
fn fuzz_lite_relpath_2000_strings_sin_panicos() {
    // Paleta con todos los caracteres "peligrosos": /, \, :, ., .., NUL,
    // control, multibyte.
    let palette: Vec<char> = "/\\:..abAB01é\u{301}中\u{0}\u{7}x".chars().collect();
    let mut rng = Rng::new(SEED ^ 0xBADC_0FFE);
    for _ in 0..2000 {
        let n = rng.below(24);
        let mut s = String::new();
        for _ in 0..n {
            s.push(palette[rng.below(palette.len())]);
        }
        // Total: Ok o Err, jamás pánico.
        let _ = RelPath::new(&s);
    }
}
