//! Diagnóstico fino de `arca-pkg-model`.
//!
//! [`PkgError`] es el error "ancho" para tools/instalador/tests (spec 02 §4:
//! parse total y descriptivo). El error canónico del ecosistema sigue siendo
//! [`arca_types::ArcaError`]: la conversión `From` colapsa cada variante a un
//! mensaje de clase estático (política de `arca-types`: contexto estático,
//! jamás datos dinámicos hacia apps/logs del host).

use arca_types::ArcaError;

/// Error de parseo/validación de `manifest.toml` y del layout del `.arca`.
///
/// Variante = clase de error con contexto dinámico (campo, valor). La etiqueta
/// corta por variante se obtiene con [`PkgError::kind`] (útil en tests y
/// métricas).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PkgError {
    /// El manifest excede [`crate::MAX_MANIFEST_BYTES`].
    #[error("manifest de {bytes} B excede el máximo de {limit} B")]
    TooLarge {
        /// Tamaño real en bytes.
        bytes: usize,
        /// Límite permitido.
        limit: usize,
    },
    /// El manifest no es UTF-8 válido (BOM UTF-8 sí se acepta y se descarta).
    #[error("manifest no es UTF-8 válido")]
    NotUtf8,
    /// Error de sintaxis TOML (incluye claves duplicadas).
    #[error("error de sintaxis TOML: {detail}")]
    TomlSyntax {
        /// Mensaje crudo del parser TOML.
        detail: String,
    },
    /// Tipo incorrecto para un campo (p. ej. `api_level = "1"`).
    #[error("tipo incorrecto en TOML: {detail}")]
    TomlType {
        /// Mensaje crudo del parser TOML.
        detail: String,
    },
    /// Campo/sección desconocida: síntoma clásico de un manifest de una
    /// `api_level` futura (spec 02 §5, fila 1) → rechazo limpio.
    #[error("campo desconocido (¿manifest de api_level futura?): {detail}")]
    UnknownField {
        /// Mensaje del parser con el campo ofensivo.
        detail: String,
    },
    /// Valor inválido para un enum de strings (`backend_pref`, `respawn`,
    /// `windows`, `wasm_runtime`).
    #[error("valor inválido en {field}: «{value}»")]
    BadEnum {
        /// Campo TOML donde ocurrió.
        field: &'static str,
        /// Valor rechazado.
        value: String,
    },
    /// Campo obligatorio ausente.
    #[error("campo obligatorio ausente: {field}")]
    MissingField {
        /// Campo esperado (ruta TOML, p. ej. `package.id`).
        field: String,
    },
    /// Sección obligatoria ausente.
    #[error("sección obligatoria ausente: [{section}]")]
    MissingSection {
        /// Nombre de la sección.
        section: &'static str,
    },
    /// `package.id` no cumple `^[a-z0-9.]{3,64}$` (docs/06 §3).
    #[error("package.id inválido: «{value}»")]
    BadAppId {
        /// Valor rechazado.
        value: String,
    },
    /// Campo semver inválido (`package.version` / `package.min_host`).
    #[error("semver inválido en {field}: «{value}»")]
    BadSemver {
        /// Campo TOML.
        field: &'static str,
        /// Valor rechazado.
        value: String,
    },
    /// `package.name` inválido (vacío, demasiado largo, controles o forma
    /// Unicode descompuesta).
    #[error("package.name inválido: «{value}»: {reason}")]
    BadName {
        /// Valor rechazado.
        value: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// `runtime.entry` inválido (vacío, largo o charset).
    #[error("runtime.entry inválido: «{value}»: {reason}")]
    BadEntry {
        /// Valor rechazado.
        value: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// Tag de `package.tags` inválido (charset `[a-z0-9-]`, 1..=32).
    #[error("package.tags inválido: «{value}»: {reason}")]
    BadTag {
        /// Valor rechazado.
        value: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// Autor inválido (vacío, largo o controles).
    #[error("package.authors inválido: «{value}»: {reason}")]
    BadAuthor {
        /// Valor rechazado.
        value: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// `package.description` inválida (demasiado larga o controles).
    #[error("package.description inválida: «{value}»: {reason}")]
    BadDescription {
        /// Extracto del valor (truncado).
        value: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// Path de fuente de `runtime.ui.fonts` inválido (sintaxis o fuera de
    /// `assets/`).
    #[error("fuente inválida «{path}»: {reason}")]
    BadFont {
        /// Path rechazado.
        path: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// Valor numérico fuera de rango (api_level no, pero atlas/budget/frame sí).
    #[error("valor fuera de rango en {field}: {value} (rango {range})")]
    OutOfRange {
        /// Campo TOML.
        field: &'static str,
        /// Valor numérico como texto.
        value: String,
        /// Rango esperado.
        range: &'static str,
    },
    /// Capability con nombre desconocido en `runtime.perms` (docs/06/07:
    /// forma punteada, p. ej. `net.client`).
    #[error("capability inválida en runtime.perms: «{value}»")]
    BadCapability {
        /// Valor rechazado.
        value: String,
    },
    /// `sha256` de artefacto no es 64 hex chars.
    #[error("sha256 inválido: {reason}")]
    BadSha256 {
        /// Razón legible.
        reason: &'static str,
    },
    /// Artefacto con clave, ubicación o campos extra inválidos.
    #[error("artefacto «{key}» inválido: {reason}")]
    BadArtifact {
        /// Clave del artefacto (`native`/`wasm`).
        key: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// No hay ningún artefacto runnable (invariante: ≥ 1 de `native`|`wasm`).
    #[error("sin artefacto runnable: se requiere al menos [artifacts.native] o [artifacts.wasm]")]
    NoArtifacts,
    /// Dos artefactos (o aot) declaran el mismo path: ambigüedad prohibida.
    #[error("path de artefacto duplicado: {path}")]
    DuplicateArtifactPath {
        /// Path duplicado.
        path: String,
    },
    /// `api_level` del paquete no está en `1..=MAX_API_LEVEL`.
    #[error("api_level {value} no soportado (soportado: 1..={max})")]
    UnsupportedApiLevel {
        /// Nivel pedido por el paquete.
        value: u32,
        /// Máximo soportado por esta biblioteca.
        max: u32,
    },
    /// `min_host` mayor que la versión del host que modela este crate.
    #[error("min_host {value} > versión del host {host}")]
    HostTooOld {
        /// Versión requerida por el paquete.
        value: String,
        /// Versión del host.
        host: &'static str,
    },
    /// Path inválido (sintaxis de [`crate::RelPath`]).
    #[error("path inválido «{path}»: {reason}")]
    BadPath {
        /// Path rechazado.
        path: String,
        /// Razón legible.
        reason: &'static str,
    },

    // ---- layout (validate_layout) ----
    /// Entrada del archivo con path que no pasa el saneo de [`crate::RelPath`].
    #[error("layout: path de entrada inválido «{path}»: {reason}")]
    LayoutBadPath {
        /// Path crudo de la entrada.
        path: String,
        /// Razón legible.
        reason: &'static str,
    },
    /// Entrada fuera del layout permitido (raíz distinta de `manifest.toml`).
    #[error("layout: entrada fuera del layout permitido: «{path}»")]
    LayoutExtra {
        /// Path de la entrada extra.
        path: String,
    },
    /// `manifest.toml` no aparece como archivo en el listing.
    #[error("layout: manifest.toml ausente o no es archivo")]
    LayoutNoManifest,
    /// Archivo declarado en el manifest que no existe en el archivo.
    #[error("layout: archivo declarado ausente: «{path}»")]
    LayoutMissing {
        /// Path declarado y ausente.
        path: String,
    },
    /// Dos entradas con el mismo path normalizado.
    #[error("layout: entrada duplicada: «{path}»")]
    LayoutDuplicate {
        /// Path duplicado.
        path: String,
    },
    /// Entrada de tipo symlink: prohibida (docs/07 §9, path-traversal).
    #[error("layout: symlink prohibido: «{path}»")]
    LayoutSymlink {
        /// Path del symlink.
        path: String,
    },
    /// Archivo bajo `bin/` que el manifest no declara: todo binario extraíble
    /// debe estar pineado con sha256 (defensa exec).
    #[error("layout: archivo bajo bin/ no declarado en el manifest: «{path}»")]
    LayoutUndeclaredBin {
        /// Path no declarado.
        path: String,
    },
}

impl PkgError {
    /// Etiqueta corta y estable de la variante (para tests/métricas).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "TooLarge",
            Self::NotUtf8 => "NotUtf8",
            Self::TomlSyntax { .. } => "TomlSyntax",
            Self::TomlType { .. } => "TomlType",
            Self::UnknownField { .. } => "UnknownField",
            Self::BadEnum { .. } => "BadEnum",
            Self::MissingField { .. } => "MissingField",
            Self::MissingSection { .. } => "MissingSection",
            Self::BadAppId { .. } => "BadAppId",
            Self::BadSemver { .. } => "BadSemver",
            Self::BadName { .. } => "BadName",
            Self::BadEntry { .. } => "BadEntry",
            Self::BadTag { .. } => "BadTag",
            Self::BadAuthor { .. } => "BadAuthor",
            Self::BadDescription { .. } => "BadDescription",
            Self::BadFont { .. } => "BadFont",
            Self::OutOfRange { .. } => "OutOfRange",
            Self::BadCapability { .. } => "BadCapability",
            Self::BadSha256 { .. } => "BadSha256",
            Self::BadArtifact { .. } => "BadArtifact",
            Self::NoArtifacts => "NoArtifacts",
            Self::DuplicateArtifactPath { .. } => "DuplicateArtifactPath",
            Self::UnsupportedApiLevel { .. } => "UnsupportedApiLevel",
            Self::HostTooOld { .. } => "HostTooOld",
            Self::BadPath { .. } => "BadPath",
            Self::LayoutBadPath { .. } => "LayoutBadPath",
            Self::LayoutExtra { .. } => "LayoutExtra",
            Self::LayoutNoManifest => "LayoutNoManifest",
            Self::LayoutMissing { .. } => "LayoutMissing",
            Self::LayoutDuplicate { .. } => "LayoutDuplicate",
            Self::LayoutSymlink { .. } => "LayoutSymlink",
            Self::LayoutUndeclaredBin { .. } => "LayoutUndeclaredBin",
        }
    }

    /// Mensaje de clase estática para [`ArcaError::Internal`] (sin datos
    /// dinámicos, según la política de `arca-types`).
    const fn arca_class(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "pkg-model: manifest demasiado grande",
            Self::NotUtf8 => "pkg-model: manifest no es UTF-8",
            Self::TomlSyntax { .. } => "pkg-model: error de sintaxis TOML",
            Self::TomlType { .. } => "pkg-model: tipo incorrecto en manifest",
            Self::UnknownField { .. } => "pkg-model: campo desconocido (¿api_level futura?)",
            Self::BadEnum { .. } => "pkg-model: valor de enum inválido en manifest",
            Self::MissingField { .. } => "pkg-model: campo obligatorio ausente",
            Self::MissingSection { .. } => "pkg-model: sección obligatoria ausente",
            Self::BadAppId { .. } => "pkg-model: package.id inválido",
            Self::BadSemver { .. } => "pkg-model: semver inválido",
            Self::BadName { .. } => "pkg-model: package.name inválido",
            Self::BadEntry { .. } => "pkg-model: runtime.entry inválido",
            Self::BadTag { .. } => "pkg-model: package.tags inválido",
            Self::BadAuthor { .. } => "pkg-model: package.authors inválido",
            Self::BadDescription { .. } => "pkg-model: package.description inválida",
            Self::BadFont { .. } => "pkg-model: fuente de UI inválida",
            Self::OutOfRange { .. } => "pkg-model: valor fuera de rango",
            Self::BadCapability { .. } => "pkg-model: capability inválida",
            Self::BadSha256 { .. } => "pkg-model: sha256 inválido",
            Self::BadArtifact { .. } => "pkg-model: artefacto inválido",
            Self::NoArtifacts => "pkg-model: paquete sin artefacto runnable",
            Self::DuplicateArtifactPath { .. } => "pkg-model: path de artefacto duplicado",
            Self::UnsupportedApiLevel { .. } => "pkg-model: api_level no soportado",
            Self::HostTooOld { .. } => "pkg-model: min_host superior a la versión del host",
            Self::BadPath { .. } => "pkg-model: path inválido",
            Self::LayoutBadPath { .. } => "pkg-model: layout: path de entrada inválido",
            Self::LayoutExtra { .. } => "pkg-model: layout: entrada extra",
            Self::LayoutNoManifest => "pkg-model: layout: manifest.toml ausente",
            Self::LayoutMissing { .. } => "pkg-model: layout: archivo declarado ausente",
            Self::LayoutDuplicate { .. } => "pkg-model: layout: entrada duplicada",
            Self::LayoutSymlink { .. } => "pkg-model: layout: symlink prohibido",
            Self::LayoutUndeclaredBin { .. } => "pkg-model: layout: binario no declarado",
        }
    }
}

impl From<PkgError> for ArcaError {
    fn from(e: PkgError) -> Self {
        match e {
            // El límite de tamaño encaja 1:1 con el semántico de FrameOverflow.
            PkgError::TooLarge { bytes, limit } => ArcaError::FrameOverflow { bytes, limit },
            other => ArcaError::Internal(other.arca_class()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_large_mapea_a_frame_overflow() {
        let e = ArcaError::from(PkgError::TooLarge {
            bytes: 70_000,
            limit: 65_536,
        });
        assert!(matches!(
            e,
            ArcaError::FrameOverflow {
                bytes: 70_000,
                limit: 65_536
            }
        ));
    }

    #[test]
    fn clases_estáticas_sin_datos_dinámicos() {
        let e = ArcaError::from(PkgError::BadAppId {
            value: "MALO".to_owned(),
        });
        match e {
            ArcaError::Internal(msg) => {
                assert!(msg.starts_with("pkg-model:"));
                assert!(!msg.contains("MALO"), "sin datos dinámicos hacia apps");
            }
            other => panic!("esperaba Internal, llegó {other:?}"),
        }
    }

    #[test]
    fn kind_cubre_todas_las_variantes() {
        // Muestra: el match de kind() es exhaustivo (el compilador lo exige).
        assert_eq!(PkgError::NoArtifacts.kind(), "NoArtifacts");
        assert_eq!(PkgError::LayoutNoManifest.kind(), "LayoutNoManifest");
        assert_eq!(
            PkgError::BadEnum {
                field: "runtime.respawn",
                value: "sometimes".to_owned(),
            }
            .kind(),
            "BadEnum"
        );
    }
}
