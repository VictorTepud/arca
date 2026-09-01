//! `arca-tools-pk` — CLI de empaquetado `.arca` (lado Deepin, spec 25).
//!
//! Capa L3 (bin) · unsafe: no · Corre en x86_64 (PC de empaquetado).
//!
//! Comandos:
//! - `keygen`      → par de claves ed25519 (`.key` 0600 + `.pub`)
//! - `graph`       → grafo de la app desde imports reales (`meta/graph.mmd`)
//! - `pack`        → valida + grafos + shas + 7z + firma = `.arca`
//! - `verify`      → EXACTAMENTE el algoritmo del host (misma lib arca-sign)
//! - `trust-ring`  → mantiene `trusted-pubkeys.bin` del host
//!
//! Códigos de exit: 0 = éxito · 1 = fallo operacional · 2 = error de uso
//! (clap). Sin `compile-wasm` funcional en v1 PC-sin-NDK: ver módulo
//! `wamrc` (stub honesto con diagnóstico).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

mod cmd_graph;
mod cmd_keygen;
mod cmd_pack;
mod cmd_ring;
mod cmd_verify;
mod graph;

use std::path::PathBuf;

use arca_types::{ArcaError, Res};
use clap::{Parser, Subcommand};

/// CLI de empaquetado de Arca.
#[derive(Debug, Parser)]
#[command(
    name = "arca-pk",
    version,
    about = "Crea, firma y verifica paquetes .arca",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Subcomandos (spec 25 §3).
#[derive(Debug, Subcommand)]
enum Cmd {
    /// Genera un par de claves ed25519 (0600) e imprime el fingerprint.
    Keygen {
        /// Directorio de salida.
        #[arg(long)]
        out: PathBuf,
    },
    /// Analiza imports del fuente y genera (o comprueba) `meta/graph.mmd`.
    Graph {
        /// Directorio del proyecto de la app.
        #[arg(long)]
        src: PathBuf,
        /// Solo comprobar sincronía (falla si el mmd no coincide).
        #[arg(long)]
        check_only: bool,
    },
    /// Empaqueta y firma un `.arca` desde el directorio del proyecto.
    Pack {
        /// Directorio del proyecto (contiene `manifest.toml`).
        #[arg(long)]
        src: PathBuf,
        /// Archivo `.arca` de salida.
        #[arg(long)]
        out: PathBuf,
        /// Clave privada ed25519 (32 B).
        #[arg(long)]
        key: PathBuf,
        /// Backend a empaquetar.
        #[arg(long, default_value = "auto")]
        backend: String,
    },
    /// Verifica un `.arca` con el mismo algoritmo que usará el host.
    Verify {
        /// Paquete a verificar.
        #[arg(long)]
        file: PathBuf,
        /// Clave pública ed25519 (32 B) de confianza.
        #[arg(long)]
        pubkey: PathBuf,
    },
    /// Añade una pubkey al anillo de confianza del host.
    #[command(name = "trust-ring")]
    TrustRing {
        /// Acción.
        #[command(subcommand)]
        action: RingAction,
    },
}

/// Subacciones de `trust-ring`.
#[derive(Debug, Subcommand)]
enum RingAction {
    /// Añade la clave al anillo y emite el bin.
    Add {
        /// Archivo `.pub` (32 bytes) a añadir.
        #[arg(long)]
        pub_key: PathBuf,
        /// Anillo texto (una clave hex por línea; se crea si falta).
        #[arg(long)]
        ring: PathBuf,
        /// Salida binaria para embeber en el host.
        #[arg(long)]
        emit: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let r = match cli.cmd {
        Cmd::Keygen { out } => cmd_keygen::run(&out),
        Cmd::Graph { src, check_only } => cmd_graph::run(&src, check_only),
        Cmd::Pack {
            src,
            out,
            key,
            backend,
        } => cmd_pack::run(&src, &out, &key, &backend),
        Cmd::Verify { file, pubkey } => cmd_verify::run(&file, &pubkey),
        Cmd::TrustRing { action } => match action {
            RingAction::Add {
                pub_key,
                ring,
                emit,
            } => cmd_ring::add(&pub_key, &ring, &emit),
        },
    };
    if let Err(e) = r {
        eprintln!("arca-pk: ERROR: {e}");
        std::process::exit(1);
    }
}

/// Lee un archivo .pub (32 B crudos o 64 hex chars).
pub(crate) fn read_pubkey(p: &std::path::Path) -> Res<[u8; 32]> {
    let data = std::fs::read(p)?;
    if data.len() == 32 {
        let mut b = [0u8; 32];
        b.copy_from_slice(&data);
        Ok(b)
    } else if data.len() == 64 {
        let s = String::from_utf8_lossy(&data).trim().to_owned();
        let mut b = [0u8; 32];
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = u8::from_str_radix(
                s.get(i * 2..i * 2 + 2)
                    .ok_or(ArcaError::InvalidPackage("pubkey: hex inválido"))?,
                16,
            )
            .map_err(|_| ArcaError::InvalidPackage("pubkey: hex inválido"))?;
        }
        Ok(b)
    } else {
        Err(ArcaError::InvalidPackage(
            "pubkey: 32 bytes crudos o 64 hex esperados",
        ))
    }
}

/// Lee la clave privada (32 B) para firmar.
pub(crate) fn read_secret(p: &std::path::Path) -> Res<arca_sign::SecretKey> {
    let data = std::fs::read(p)?;
    if data.len() != 32 {
        return Err(ArcaError::InvalidPackage(
            "key: 32 bytes (seed ed25519) esperados",
        ));
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&data);
    Ok(arca_sign::SecretKey::from_bytes(&b))
}

/// sha256 de bytes.
pub(crate) fn sha256(b: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(b);
    h.finalize().into()
}

/// hex minúsculas.
pub(crate) fn hex32(b: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in b {
        use std::fmt::Write as _;
        let _ = write!(s, "{byte:02x}");
    }
    s
}
