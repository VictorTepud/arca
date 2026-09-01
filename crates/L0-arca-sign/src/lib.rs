//! `arca-sign` — ed25519 verify + digest blake3 canónico de paquetes `.arca`.
//!
//! Capa L0 · unsafe: **no** · Contrato: `specs/arca-08-sign.md`.
//!
//! División de responsabilidades (docs/06 §5):
//! - **Host** (este crate sin features): solo VERIFICA. Cero secretos en el
//!   dispositivo — únicamente el anillo de pubkeys embebidas (ADR-012).
//! - **Deepin / tools-pk** (feature `signer`): keygen + firma del digest
//!   canónico. La clave privada jamás sale del PC de empaquetado.
//!
//! Enmiendas documentadas respecto de la spec 08 (bitácora T06):
//! - Los paths entran como `&str` saneado (el `RelPath` de pkg-model se
//!   convierte a `&str` en el installer): así este crate no depende de
//!   `arca-pkg-model` y respeta §2 (deps: types + ed25519-dalek + blake3 + hex).
//! - `StreamingVerifier::new` toma además `manifest_sha` (necesario para el
//!   registro `M` del digest canónico).
//! - Dep extra `sha2` (los sha256 de artefactos los fija el manifest v1).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod digest;
pub mod ring;
pub mod sig;
#[cfg(feature = "signer")]
pub mod signer;
pub mod stream;

pub use digest::package_digest;
pub use ring::RingOfTrust;
pub use sig::PackageSignature;
pub use stream::StreamingVerifier;

#[cfg(feature = "signer")]
pub use signer::{generate_keypair, keygen, sign_digest, KeyFiles, SecretKey};

/// Nombre del archivo de firma dentro del `.arca` (docs/06 §2).
pub const SIGNATURE_PATH: &str = "meta/signature.bin";
/// Nombre del manifest dentro del `.arca`.
pub const MANIFEST_PATH: &str = "manifest.toml";
/// Nombre del digest de control del manifest (docs/06 §2).
pub const MANIFEST_DIGEST_PATH: &str = "meta/manifest.digest";
