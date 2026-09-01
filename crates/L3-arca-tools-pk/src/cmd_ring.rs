//! `trust-ring`: mantiene el anillo de pubkeys del host (spec 25 §3).

use std::path::Path;

use arca_sign::RingOfTrust;
use arca_types::{ArcaError, Digest, Res};

use crate::{hex32, read_pubkey};

/// `trust-ring add --pub-key <pub> --ring <txt> --emit <bin>`.
pub(crate) fn add(pub_key: &Path, ring_txt: &Path, emit: &Path) -> Res<()> {
    let pub_bytes = read_pubkey(pub_key)?;
    let fingerprint = Digest::of(&pub_bytes);

    // ring texto: una clave hex por línea (se crea si falta)
    let actual = std::fs::read_to_string(ring_txt).unwrap_or_default();
    let mut claves: Vec<[u8; 32]> = Vec::new();
    for linea in actual.lines() {
        let l = linea.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let d = Digest::from_hex(l)
            .map_err(|_| ArcaError::InvalidPackage("trust-ring: línea no es hex64 válida"))?;
        claves.push(d.0);
    }
    if claves.contains(&pub_bytes) {
        println!("trust-ring: la clave ya estaba ({} claves)", claves.len());
    } else {
        claves.push(pub_bytes);
        if let Some(p) = ring_txt.parent() {
            if !p.as_os_str().is_empty() {
                std::fs::create_dir_all(p)?;
            }
        }
        let mut txt = claves.iter().map(hex32).collect::<Vec<_>>().join("\n");
        txt.push('\n');
        std::fs::write(ring_txt, txt)?;
    }

    // emit bin (formato spec 08 §4)
    let mut ring = RingOfTrust::empty();
    for k in &claves {
        ring.push_bytes(k)?;
    }
    if let Some(p) = emit.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    std::fs::write(emit, ring.to_bin())?;
    println!(
        "trust-ring: {} claves → {} (+ {} bytes)",
        claves.len(),
        emit.display(),
        4 + 32 * claves.len()
    );
    println!("            nueva clave fingerprint: {fingerprint}");
    println!(
        "            recorta trusted-pubkeys.bin al host (include_bytes!) y RECOMPILA (ADR-012)"
    );
    Ok(())
}
