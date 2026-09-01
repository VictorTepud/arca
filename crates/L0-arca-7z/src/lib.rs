//! `arca-7z` — extracción **streaming** y sandboxeada de archivos 7z
//! (`.arca`) sobre `sevenz-rust2`.
//!
//! Capa L0 · unsafe: no · Riesgo: R-04
//! Contrato completo: `specs/arca-09-7z.md` del blueprint.
//!
//! Este crate NO conoce manifests (eso es `arca-pkg-model`, la primera
//! barrera): recibe bytes de un 7z y produce archivos seguros. La
//! **segunda barrera** es [`sanitize_entry_path`]: todo path de entrada se
//! re-valida aquí antes de abrir el archivo destino (defense-in-depth).
//!
//! # Ejemplo
//!
//! ```no_run
//! use std::fs::File;
//! use arca_7z::{Archive, DirSink, ExtractPlan};
//!
//! let mut paquete = File::open("miapp-1.2.0.arca").unwrap();
//! let mut arc = Archive::open(paquete).unwrap();
//! for e in arc.entries().unwrap() {
//!     println!("{} ({} bytes)", e.path, e.size);
//! }
//! let mut sink = DirSink::new(std::path::PathBuf::from("/data/app/v1"));
//! let mut progreso = |frac: f64| { /* UI */ let _ = frac; };
//! // Solo bin/ para arranque rápido (docs/06 §2):
//! let plan = ExtractPlan::parse(&["bin", "manifest.toml"]).unwrap();
//! arc.extract(&plan, &mut sink, &mut progreso).unwrap();
//! ```
//!
//! # Invariantes (spec 09 §4)
//!
//! - **Streaming**: memoria O(1) por archivo (buffers fijos de 1 MiB);
//!   nunca se carga el paquete ni un archivo entero en RAM.
//! - Todo path pasa [`sanitize_entry_path`] **antes** de abrir el destino.
//! - Directorios 0700, archivos 0600, escritura vía `.arca-tmp` + rename.
//! - Progreso invocado cada ≥ 256 KiB.
//! - [`probe_features`] reporta codecs/filtros soportados (riesgo R-04).
//!
//! # Features opt-in
//!
//! `deflate`, `brotli`, `lz4`, `zstd` (passthrough a `sevenz-rust2`).
//! Por defecto SOLO el núcleo LZMA2/COPY/BZIP2/PPMD + filtros BCJ/DELTA:
//! la v1 del contenedor empaqueta LZMA2 plano (docs/06 §6) y el host de
//! Android agradece el binario pequeño (zstd arrastra C).

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

pub mod features;
pub mod path;
pub mod sink;

mod archive;

pub use archive::{Archive, EntryInfo, ExtractPlan, PROGRESS_EVERY_BYTES};
pub use features::{probe_features, CodecSupport, Features};
pub use path::{sanitize_entry_path, RelPath, MAX_COMPONENT_BYTES, MAX_DEPTH, MAX_TOTAL_BYTES};
pub use sink::{DirSink, EntrySink, COPY_BUF_BYTES, TMP_SUFFIX};
