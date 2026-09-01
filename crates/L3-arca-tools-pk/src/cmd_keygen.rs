//! `keygen`: par de claves ed25519 (spec 25 §3).

use std::path::Path;

use arca_sign::generate_keypair;
use arca_types::Res;

/// Genera el par en `out` e imprime el fingerprint.
pub(crate) fn run(out: &Path) -> Res<()> {
    std::fs::create_dir_all(out)?;
    let kf = generate_keypair(out)?;
    println!("clave privada : {} (0600)", kf.secret_path.display());
    println!("clave pública : {}", kf.public_path.display());
    println!("fingerprint   : {}", kf.key_id);
    println!();
    println!("Añade la pública al anillo del host:");
    println!("  arca-pk trust-ring add --pub-key {} --ring keys/trusted-pubkeys.txt --emit keys/trusted-pubkeys.bin", kf.public_path.display());
    Ok(())
}
