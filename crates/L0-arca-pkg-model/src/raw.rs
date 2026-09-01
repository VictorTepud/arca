//! Capa raw de `manifest.toml`: espejo 1:1 del TOML, sin semántica.
//!
//! Esta capa solo captura FORMA (tipos básicos, campos conocidos); la
//! SEMÁNTICA (semver, ids, paths, rangos, enums por string) la valida
//! [`crate::manifest`] para producir errores precisos por campo. Los campos
//! son `Option` ⇒ la ausencia es detectable como [`crate::PkgError::MissingField`].
//!
//! Con `deny_unknown_fields` (menos en artefactos, que usan `extra` como
//! punto de extensión) los manifests de `api_level` futuro se rechazan limpio
//! (spec 02 §5, fila 1).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::PkgError;

/// Documento `manifest.toml` crudo.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawManifest {
    /// `[package]`.
    pub(crate) package: Option<RawPackage>,
    /// `[runtime]`.
    pub(crate) runtime: Option<RawRuntime>,
    /// `[artifacts.<clave>]`.
    pub(crate) artifacts: Option<BTreeMap<String, RawArtifact>>,
    /// `[profile]`.
    pub(crate) profile: Option<RawProfile>,
}

/// `[package]` crudo.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPackage {
    /// `id`.
    pub(crate) id: Option<String>,
    /// `name`.
    pub(crate) name: Option<String>,
    /// `version` (semver como string).
    pub(crate) version: Option<String>,
    /// `min_host` (semver como string).
    pub(crate) min_host: Option<String>,
    /// `api_level`.
    pub(crate) api_level: Option<u32>,
    /// `authors`.
    pub(crate) authors: Option<Vec<String>>,
    /// `description`.
    pub(crate) description: Option<String>,
    /// `tags`.
    pub(crate) tags: Option<Vec<String>>,
}

/// `[runtime]` crudo. Los enums van como string (se validan con contexto).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRuntime {
    /// `backend_pref`: "native | wasm | any".
    pub(crate) backend_pref: Option<String>,
    /// `entry`.
    pub(crate) entry: Option<String>,
    /// `respawn`: "never | on-crash | always".
    pub(crate) respawn: Option<String>,
    /// `ui`.
    pub(crate) ui: Option<RawUi>,
    /// `perms` (capabilities en forma punteada).
    pub(crate) perms: Option<Vec<String>>,
}

/// `[runtime.ui]` crudo.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawUi {
    /// `sync`.
    pub(crate) sync: Option<bool>,
    /// `windows`: "single | multi".
    pub(crate) windows: Option<String>,
    /// `atlas`.
    pub(crate) atlas: Option<u32>,
    /// `fonts`.
    pub(crate) fonts: Option<Vec<String>>,
}

/// `[artifacts.<clave>]` crudo. `extra` (flatten) captura el resto de claves
/// con valores string: `aot`, `wasm_runtime`, y extensiones futuras.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct RawArtifact {
    /// `path`.
    pub(crate) path: Option<String>,
    /// `sha256` (64 hex).
    pub(crate) sha256: Option<String>,
    /// Claves extra (solo strings; otro tipo ⇒ error de tipo TOML).
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, String>,
}

/// `[profile]` crudo.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawProfile {
    /// `launch_budget_ms`.
    pub(crate) launch_budget_ms: Option<u32>,
    /// `max_frame_mb`.
    pub(crate) max_frame_mb: Option<u32>,
}

/// Clasifica un error de `toml::de::Error` en la clase gruesa de
/// [`PkgError`] (el detalle dinámico viaja dentro del mensaje).
pub(crate) fn classify_toml_de(e: toml::de::Error) -> PkgError {
    let msg = e.to_string();
    if msg.contains("unknown field") {
        PkgError::UnknownField { detail: msg }
    } else if msg.contains("invalid type") || msg.contains("invalid value") {
        PkgError::TomlType { detail: msg }
    } else {
        // Sintaxis, claves duplicadas, comillas sin cerrar, etc.
        PkgError::TomlSyntax { detail: msg }
    }
}
