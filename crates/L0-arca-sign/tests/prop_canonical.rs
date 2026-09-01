//! Property tests del digest canónico (proptest, seed determinista por
//! defecto): determinismo bajo permutación y sensibilidad a cada byte.
//! No requieren `signer` (solo `package_digest`).

use proptest::prelude::*;
use proptest::{array, collection};

use arca_sign::package_digest;

fn ref_entries(v: &[(String, [u8; 32])]) -> Vec<(&str, [u8; 32])> {
    v.iter().map(|(p, h)| (p.as_str(), *h)).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// El digest canónico es invariante a CUALQUIER permutación del input
    /// (incluye paths duplicados: el tie-break por hash mantiene el orden
    /// interno canónico). Es la propiedad "doble pack, mismo digest".
    #[test]
    fn digest_invariante_a_permutaciones(
        entries in collection::vec(("[a-z0-9/._-]{1,16}", array::uniform32(any::<u8>())), 0..16),
        manifest in array::uniform32(any::<u8>()),
        seed in any::<u64>(),
    ) {
        let mut barajado = entries.clone();
        // Fisher-Yates con LCG del seed (determinista por caso):
        let mut s = seed | 1;
        let mut next = || {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            s
        };
        for i in (1..barajado.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            barajado.swap(i, j);
        }
        prop_assert_eq!(
            package_digest(&ref_entries(&entries), manifest),
            package_digest(&ref_entries(&barajado), manifest)
        );
    }

    /// Sensibilidad: un solo bit del manifest cambia el digest (siempre), y
    /// un bit de un hash de archivo también (cuando hay entradas).
    #[test]
    fn digest_sensible_a_un_bit(
        entries in collection::vec(("[a-z0-9/._-]{1,16}", array::uniform32(any::<u8>())), 1..16),
        manifest in array::uniform32(any::<u8>()),
        bit in 0usize..256,
    ) {
        let base = package_digest(&ref_entries(&entries), manifest);
        let (byte, bit_mask) = (bit / 8, 1u8 << (bit % 8));

        // Manifest mutado:
        let mut m2 = manifest;
        m2[byte] ^= bit_mask;
        prop_assert_ne!(base, package_digest(&ref_entries(&entries), m2));

        // Hash de la primera entrada mutado:
        let mut e2 = entries.clone();
        e2[0].1[byte] ^= bit_mask;
        prop_assert_ne!(base, package_digest(&ref_entries(&e2), manifest));

        // Path de la primera entrada mutado (sufijo distinto):
        let mut e3 = entries.clone();
        e3[0].0.push('z');
        prop_assert_ne!(base, package_digest(&ref_entries(&e3), manifest));
    }
}
