//! `arca-pkg-model` — el modelo del paquete `.arca` (spec 02).
//!
//! Único lugar donde "un paquete" significa algo: parseo y validación de
//! `manifest.toml` (docs/06 §3), tipado de artefactos y validación del
//! layout interno del contenedor (docs/06 §2). Es la PRIMERA barrera contra
//! el path-traversal; la segunda vive en `arca-7z` (spec 09).
//!
//! Invariantes (spec 02 §4):
//! - [`Manifest::parse`] es **total**: cualquier entrada produce un
//!   [`arca_types::ArcaError`] descriptivo, jamás un pánico.
//! - semver estricto (prerelease y build metadata permitidos).
//! - `api_level ≤ MAX_API_LEVEL` y `min_host ≤ HOST_VERSION` se validan aquí,
//!   no en el instalador.
//! - Un paquete declara al menos un artefacto runnable (`native` o `wasm`).
//!
//! Mapa de módulos:
//! - [`relpath`]: path relativo saneado (newtype con invariantes).
//! - [`manifest`]: modelo + parse/validate de `manifest.toml` + backend_for.
//! - [`entries`]: listing del archivo, insumo de la validación de layout.
//! - [`layout`]: validación del layout interno del `.arca`.
//! - [`host`]: variante de host (ADR-001/ADR-003).
//! - [`error`]: [`PkgError`], diagnóstico fino (→ `ArcaError`).
//!
//! Errores: los mensajes de [`arca_types::ArcaError`] son de clase estática
//! (política del ecosistema); el detalle dinámico (campo, valor, path) viaja
//! en [`PkgError`] para tools/instalador/tests.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

pub mod entries;
pub mod error;
pub mod host;
pub mod layout;
pub mod manifest;
pub mod relpath;

mod hex;
mod raw;

pub use entries::{ArchiveEntries, ArchiveEntry, EntryKind};
pub use error::PkgError;
pub use host::HostVariant;
pub use manifest::{
    Artifact, BackendPref, Manifest, PackageInfo, ProfileInfo, RespawnPolicy, RuntimeInfo, UiInfo,
    WindowsMode, ARTIFACT_NATIVE, ARTIFACT_WASM,
};
pub use relpath::RelPath;

/// Layout interno permitido del `.arca` (docs/06 §2): `manifest.toml`
/// obligatorio más los cuatro directorios raíz.
pub const LAYOUT: &[&str] = &["manifest.toml", "bin/", "assets/", "icons/", "meta/"];

/// Tamaño máximo de `manifest.toml` en bytes (64 KiB, spec 02 §3).
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// Nivel de API del contrato Arca ABI/UI soportado por ESTA versión del
/// modelo (v1 ⇒ 1). Un manifest con `api_level` mayor se rechaza limpio
/// (síntoma "unknown field `backends`" de la spec 02 §5).
pub const MAX_API_LEVEL: u32 = 1;

/// Versión del host que esta biblioteca modela. El check `min_host ≤ host`
/// (spec 02 §4) se hace contra esta constante AQUÍ, no en el instalador.
/// Debe bump-earse con cada release del host.
pub const HOST_VERSION: semver::Version = semver::Version::new(1, 0, 0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consts_de_la_spec_estables() {
        // Espejo del contrato literal de spec 02 §3: si esto se rompe, se
        // rompió el contrato, no el test.
        assert_eq!(
            LAYOUT,
            &["manifest.toml", "bin/", "assets/", "icons/", "meta/"]
        );
        assert_eq!(MAX_MANIFEST_BYTES, 65_536);
        assert_eq!(MAX_API_LEVEL, 1);
        assert_eq!(HOST_VERSION.to_string(), "1.0.0");
        assert!(matches!(semver::Version::parse("1.0.0"), Ok(v) if v == HOST_VERSION));
        assert_eq!(ARTIFACT_NATIVE, "native");
        assert_eq!(ARTIFACT_WASM, "wasm");
    }
}
