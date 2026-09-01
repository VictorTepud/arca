//! El modelo del paquete: [`Manifest`] y su parseo/validación (spec 02 §3,
//! docs/06 §3).
//!
//! `parse`/`parse_detailed` son **totales**: cualquier entrada produce un
//! error descriptivo, jamás un pánico. La validación cubre: tipos estrictos,
//! semver estricto, regex de `package.id`, rangos, paths saneados, claves de
//! artefactos y el invariante "al menos un artefacto runnable".

use std::collections::{BTreeMap, HashSet};

use arca_types::{AppId, ArcaError, Capability, Res};
use semver::Version;

use crate::error::PkgError;
use crate::hex;
use crate::host::HostVariant;
use crate::raw::{
    classify_toml_de, RawArtifact, RawManifest, RawPackage, RawProfile, RawRuntime, RawUi,
};
use crate::relpath::RelPath;
use crate::{HOST_VERSION, MAX_API_LEVEL, MAX_MANIFEST_BYTES};

/// Clave del artefacto nativo en `artifacts`.
pub const ARTIFACT_NATIVE: &str = "native";
/// Clave del artefacto wasm en `artifacts`.
pub const ARTIFACT_WASM: &str = "wasm";

/// Atlas por defecto (docs/06 §3: `atlas = 2048`).
const DEFAULT_ATLAS: u32 = 2048;
/// Límites de sanidad del `[profile]` (self-declaración para benches).
const LAUNCH_BUDGET_RANGE: &str = "1..=60000";
const MAX_FRAME_MB_RANGE: &str = "1..=256";
/// Rango del atlas de UI.
const ATLAS_RANGE: &str = "64..=16384 (potencia de 2)";

/// Preferencia de backend del paquete (docs/06 §3: "el host decide según
/// variante" — la preferencia es blanda, solo ordena la elección).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendPref {
    /// Prefiere el artefacto nativo.
    Native,
    /// Prefiere el artefacto wasm.
    Wasm,
    /// Sin preferencia: decide el host según su variante.
    Any,
}

impl BackendPref {
    /// Valor canónico en TOML.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wasm => "wasm",
            Self::Any => "any",
        }
    }
}

impl std::fmt::Display for BackendPref {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Política de respawn de la sub-app (docs/06 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RespawnPolicy {
    /// Nunca relanzar (una muerte es una muerte).
    Never,
    /// Relanzar solo si murió por crash.
    OnCrash,
    /// Relanzar siempre que termine.
    Always,
}

impl RespawnPolicy {
    /// Valor canónico en TOML.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnCrash => "on-crash",
            Self::Always => "always",
        }
    }
}

impl std::fmt::Display for RespawnPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Modo de ventanas de la sub-app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowsMode {
    /// Una sola ventana por instancia.
    Single,
    /// Varias ventanas por instancia.
    Multi,
}

impl WindowsMode {
    /// Valor canónico en TOML.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
        }
    }
}

impl std::fmt::Display for WindowsMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `[package]`: identidad y compatibilidad del paquete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    /// Id único (`^[a-z0-9.]{3,64}$`, docs/06 §3).
    pub id: AppId,
    /// Nombre visible (≤ 128 chars, sin marcas combinantes — ver nota NFC).
    pub name: String,
    /// Versión del paquete (semver estricto; prerelease/build permitidos).
    pub version: Version,
    /// Versión mínima del host Arca requerida.
    pub min_host: Version,
    /// Nivel del contrato Arca ABI/UI (1..= [`MAX_API_LEVEL`]).
    pub api_level: u32,
    /// Autores (metadato opcional).
    pub authors: Vec<String>,
    /// Descripción (metadato opcional).
    pub description: String,
    /// Tags de store (metadato opcional, `[a-z0-9-]`).
    pub tags: Vec<String>,
}

/// `[runtime]`: cómo se ejecuta la sub-app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// Preferencia de backend (blanda).
    pub backend_pref: BackendPref,
    /// Símbolo/sufijo de entrada (main arca).
    pub entry: String,
    /// Política de respawn.
    pub respawn: RespawnPolicy,
    /// Configuración de UI.
    pub ui: UiInfo,
    /// Capabilities solicitadas (se conceden en instalación, docs/07).
    pub perms: Vec<Capability>,
}

/// `[runtime.ui]`: configuración de UI de la sub-app. Todos los campos tienen
/// default (presentación, no seguridad).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiInfo {
    /// ¿La sub-app pinta síncronamente (bloquea el frame)?
    pub sync: bool,
    /// Modo de ventanas.
    pub windows: WindowsMode,
    /// Tamaño del atlas de texturas (potencia de 2).
    pub atlas: u32,
    /// Fuentes empaquetadas (paths relativos bajo `assets/`).
    pub fonts: Vec<RelPath>,
}

impl Default for UiInfo {
    fn default() -> Self {
        Self {
            sync: false,
            windows: WindowsMode::Single,
            atlas: DEFAULT_ATLAS,
            fonts: Vec::new(),
        }
    }
}

/// `[profile]`: auto-declaración de rendimiento para benches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInfo {
    /// Presupuesto de arranque auto-declarado (ms).
    pub launch_budget_ms: u32,
    /// Máximo de memoria por frame auto-declarado (MB).
    pub max_frame_mb: u32,
}

/// Un artefacto ejecutable del paquete (spec 02 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Path dentro del `.arca` (saneado).
    pub path: RelPath,
    /// SHA-256 del archivo extraído (integridad post-extract).
    pub sha256: [u8; 32],
    /// Campos extra del TOML (`aot`, `wasm_runtime`, extensiones).
    pub extra: BTreeMap<String, String>,
}

impl Artifact {
    /// SHA-256 en hex minúsculas (64 chars).
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        hex::encode32(&self.sha256)
    }

    /// Path AOT opcional (campo extra `aot`; ya validado en parse — si el
    /// manifest fue construido a mano puede faltar y da `None`).
    #[must_use]
    pub fn aot_path(&self) -> Option<RelPath> {
        self.extra.get("aot").and_then(|s| RelPath::new(s).ok())
    }
}

/// El manifest completo del paquete `.arca` (spec 02 §3, docs/06 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// `[package]`.
    pub package: PackageInfo,
    /// `[runtime]`.
    pub runtime: RuntimeInfo,
    /// `[artifacts.native]` / `[artifacts.wasm]` (claves validadas).
    pub artifacts: BTreeMap<String, Artifact>,
    /// `[profile]`.
    pub profile: ProfileInfo,
}

impl Manifest {
    /// Parse estricto de `manifest.toml` (bytes crudos).
    ///
    /// Total: cualquier entrada → [`ArcaError`] descriptivo, jamás pánico
    /// (spec 02 §4). Acepta y descarta BOM UTF-8; rechaza > 64 KiB.
    pub fn parse(bytes: &[u8]) -> Res<Self> {
        Self::parse_detailed(bytes).map_err(ArcaError::from)
    }

    /// Ídem [`Manifest::parse`] pero devolviendo el diagnóstico fino
    /// [`PkgError`] (para tools-pk, instalador y tests).
    pub fn parse_detailed(bytes: &[u8]) -> Result<Self, PkgError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(PkgError::TooLarge {
                bytes: bytes.len(),
                limit: MAX_MANIFEST_BYTES,
            });
        }
        // BOM UTF-8: se descarta (docs: "TOML con BOM" es un síntoma conocido).
        let payload: &[u8] = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &bytes[3..]
        } else {
            bytes
        };
        let s: &str = std::str::from_utf8(payload).map_err(|_| PkgError::NotUtf8)?;
        let raw: RawManifest = toml::from_str(s).map_err(classify_toml_de)?;
        Self::from_raw(raw)
    }

    /// Serializa el modelo de vuelta a TOML (útil para `arca-tools-pk` y para
    /// el test de roundtrip). Los Option del raw siempre van llenos.
    pub fn to_toml(&self) -> Res<String> {
        toml::to_string(&self.to_raw())
            .map_err(|_| ArcaError::Internal("pkg-model: error serializando manifest a TOML"))
    }

    /// Capabilities solicitadas (spec 02 §3).
    #[must_use]
    pub fn requested_caps(&self) -> &[Capability] {
        &self.runtime.perms
    }

    /// Artefacto `native`, si el paquete lo trae.
    #[must_use]
    pub fn native(&self) -> Option<&Artifact> {
        self.artifacts.get(ARTIFACT_NATIVE)
    }

    /// Artefacto `wasm`, si el paquete lo trae.
    #[must_use]
    pub fn wasm(&self) -> Option<&Artifact> {
        self.artifacts.get(ARTIFACT_WASM)
    }

    /// Elige el artefacto a ejecutar según la variante del host (ADR-001/003).
    ///
    /// - **Libre**: puede nativo y wasm; `backend_pref` ordena la preferencia
    ///   (`any` = nativo, el default del host-libre). Si la preferida no
    ///   está, cae a la otra: la preferencia es blanda (docs/06 §3).
    /// - **Moderno**: SOLO wasm (la ejecución nativa está vetada por
    ///   targetSdk 35); la preferencia se ignora si no es satisfacible.
    ///
    /// Falla únicamente si NINGÚN artefacto aplica (spec 02 §5: "falla si
    /// NINGUNO aplica"; el empaquetador dual evita ese caso).
    pub fn backend_for(&self, host: HostVariant) -> Res<&Artifact> {
        let native = self.artifacts.get(ARTIFACT_NATIVE);
        let wasm = self.artifacts.get(ARTIFACT_WASM);
        let (first, second): (Option<&Artifact>, Option<&Artifact>) = if host.can_native() {
            match self.runtime.backend_pref {
                BackendPref::Wasm => (wasm, native),
                BackendPref::Native | BackendPref::Any => (native, wasm),
            }
        } else {
            (wasm, None)
        };
        first.or(second).ok_or(ArcaError::Internal(
            "pkg-model: backend_for: ningún artefacto ejecutable en esta variante de host",
        ))
    }

    // ---- validación (raw → modelo) ----

    /// Convierte y valida la capa raw en el modelo tipado. Orden determinista
    /// (package → runtime → artifacts → profile) para errores estables.
    fn from_raw(raw: RawManifest) -> Result<Self, PkgError> {
        // ---------- [package] ----------
        let pkg = raw
            .package
            .ok_or(PkgError::MissingSection { section: "package" })?;
        let id_s = pkg.id.ok_or_else(|| missing("package.id"))?;
        let id = AppId::new(&id_s).map_err(|_| PkgError::BadAppId {
            value: id_s.clone(),
        })?;
        let name = pkg.name.ok_or_else(|| missing("package.name"))?;
        validate_name(&name)?;
        let version_s = pkg.version.ok_or_else(|| missing("package.version"))?;
        let version = Version::parse(&version_s).map_err(|_| PkgError::BadSemver {
            field: "package.version",
            value: version_s,
        })?;
        let min_host_s = pkg.min_host.ok_or_else(|| missing("package.min_host"))?;
        let min_host = Version::parse(&min_host_s).map_err(|_| PkgError::BadSemver {
            field: "package.min_host",
            value: min_host_s.clone(),
        })?;
        // "min_host ≤ versión actual del host: validado aquí" (spec 02 §4).
        if min_host > HOST_VERSION {
            return Err(PkgError::HostTooOld {
                value: min_host.to_string(),
                host: "1.0.0",
            });
        }
        let api_level = pkg.api_level.ok_or_else(|| missing("package.api_level"))?;
        if !(1..=MAX_API_LEVEL).contains(&api_level) {
            return Err(PkgError::UnsupportedApiLevel {
                value: api_level,
                max: MAX_API_LEVEL,
            });
        }
        let authors = pkg.authors.unwrap_or_default();
        for a in authors.iter() {
            validate_author(a)?;
        }
        let description = pkg.description.unwrap_or_default();
        validate_description(&description)?;
        let tags = pkg.tags.unwrap_or_default();
        for t in tags.iter() {
            validate_tag(t)?;
        }

        // ---------- [runtime] ----------
        let rt = raw
            .runtime
            .ok_or(PkgError::MissingSection { section: "runtime" })?;
        let backend_pref = parse_backend_pref(
            &rt.backend_pref
                .ok_or_else(|| missing("runtime.backend_pref"))?,
        )?;
        let entry = rt.entry.ok_or_else(|| missing("runtime.entry"))?;
        validate_entry(&entry)?;
        let respawn = parse_respawn(&rt.respawn.ok_or_else(|| missing("runtime.respawn"))?)?;

        // [runtime.ui]: todo presentacional ⇒ defaults cuando falta.
        let mut ui = UiInfo::default();
        if let Some(raw_ui) = rt.ui {
            if let Some(sync) = raw_ui.sync {
                ui.sync = sync;
            }
            if let Some(windows_s) = raw_ui.windows {
                ui.windows = parse_windows(&windows_s)?;
            }
            if let Some(atlas) = raw_ui.atlas {
                ui.atlas = atlas;
            }
            for f in raw_ui.fonts.unwrap_or_default() {
                // Los paths de fuente son un dominio propio (assets de UI):
                // clase BadFont, no BadPath genérico.
                let p = RelPath::new(&f).map_err(|e| match e {
                    PkgError::BadPath { reason, .. } => PkgError::BadFont {
                        path: f.clone(),
                        reason,
                    },
                    other => other,
                })?;
                if !p.is_under("assets") {
                    return Err(PkgError::BadFont {
                        path: f.clone(),
                        reason: "fuente fuera de assets/",
                    });
                }
                ui.fonts.push(p);
            }
        }
        validate_atlas(ui.atlas)?;

        let mut perms = Vec::new();
        for p in rt.perms.unwrap_or_default() {
            let cap = cap_from_manifest_name(&p)
                .ok_or_else(|| PkgError::BadCapability { value: p.clone() })?;
            perms.push(cap);
        }

        // ---------- [artifacts] ----------
        let raw_arts: BTreeMap<String, RawArtifact> = raw.artifacts.unwrap_or_default();
        if raw_arts.is_empty() {
            return Err(PkgError::NoArtifacts);
        }
        // Pase 1 — forma: claves conocidas + paths saneados + sin duplicados.
        // (El dup ANTES que las reglas de ubicación: un path repetido en dos
        // artefactos es una ambigüedad de integridad, se reporta como tal.)
        let mut seen: HashSet<RelPath> = HashSet::new();
        let mut forms: Vec<ArtForm> = Vec::with_capacity(raw_arts.len());
        for (key, ra) in raw_arts.iter() {
            if key != ARTIFACT_NATIVE && key != ARTIFACT_WASM {
                return Err(PkgError::BadArtifact {
                    key: key.clone(),
                    reason: "la clave debe ser «native» o «wasm»",
                });
            }
            let path_s = ra
                .path
                .as_deref()
                .ok_or_else(|| missing(&format!("artifacts.{key}.path")))?;
            let path = RelPath::new(path_s)?;
            if !seen.insert(path.clone()) {
                return Err(PkgError::DuplicateArtifactPath {
                    path: path.to_string(),
                });
            }
            let aot = match ra.extra.get("aot") {
                Some(v) => {
                    let p = RelPath::new(v)?;
                    if !seen.insert(p.clone()) {
                        return Err(PkgError::DuplicateArtifactPath { path: v.clone() });
                    }
                    Some(p)
                }
                None => None,
            };
            forms.push(ArtForm {
                key: key.clone(),
                path,
                aot,
            });
        }
        // Pase 2 — semántica por artefacto: ubicación, sha256, extras.
        // (forms se construyó iterando raw_arts: el zip está alineado.)
        let mut artifacts: BTreeMap<String, Artifact> = BTreeMap::new();
        for (f, ra) in forms.into_iter().zip(raw_arts.values()) {
            artifacts.insert(f.key.clone(), artifact_semantic(f, ra)?);
        }

        // ---------- [profile] ----------
        let prof = raw
            .profile
            .ok_or(PkgError::MissingSection { section: "profile" })?;
        let launch_budget_ms = prof
            .launch_budget_ms
            .ok_or_else(|| missing("profile.launch_budget_ms"))?;
        if !(1..=60_000).contains(&launch_budget_ms) {
            return Err(PkgError::OutOfRange {
                field: "profile.launch_budget_ms",
                value: launch_budget_ms.to_string(),
                range: LAUNCH_BUDGET_RANGE,
            });
        }
        let max_frame_mb = prof
            .max_frame_mb
            .ok_or_else(|| missing("profile.max_frame_mb"))?;
        if !(1..=256).contains(&max_frame_mb) {
            return Err(PkgError::OutOfRange {
                field: "profile.max_frame_mb",
                value: max_frame_mb.to_string(),
                range: MAX_FRAME_MB_RANGE,
            });
        }

        Ok(Self {
            package: PackageInfo {
                id,
                name,
                version,
                min_host,
                api_level,
                authors,
                description,
                tags,
            },
            runtime: RuntimeInfo {
                backend_pref,
                entry,
                respawn,
                ui,
                perms,
            },
            artifacts,
            profile: ProfileInfo {
                launch_budget_ms,
                max_frame_mb,
            },
        })
    }

    /// Reconstruye la capa raw (para serializar a TOML).
    fn to_raw(&self) -> RawManifest {
        RawManifest {
            package: Some(RawPackage {
                id: Some(self.package.id.as_str().to_owned()),
                name: Some(self.package.name.clone()),
                version: Some(self.package.version.to_string()),
                min_host: Some(self.package.min_host.to_string()),
                api_level: Some(self.package.api_level),
                authors: Some(self.package.authors.clone()),
                description: Some(self.package.description.clone()),
                tags: Some(self.package.tags.clone()),
            }),
            runtime: Some(RawRuntime {
                backend_pref: Some(self.runtime.backend_pref.as_str().to_owned()),
                entry: Some(self.runtime.entry.clone()),
                respawn: Some(self.runtime.respawn.as_str().to_owned()),
                ui: Some(RawUi {
                    sync: Some(self.runtime.ui.sync),
                    windows: Some(self.runtime.ui.windows.as_str().to_owned()),
                    atlas: Some(self.runtime.ui.atlas),
                    fonts: Some(
                        self.runtime
                            .ui
                            .fonts
                            .iter()
                            .map(|p| p.as_str().to_owned())
                            .collect(),
                    ),
                }),
                perms: Some(
                    self.runtime
                        .perms
                        .iter()
                        .map(|c| cap_to_manifest_name(*c).to_owned())
                        .collect(),
                ),
            }),
            artifacts: Some(
                self.artifacts
                    .iter()
                    .map(|(k, a)| {
                        (
                            k.clone(),
                            RawArtifact {
                                path: Some(a.path.as_str().to_owned()),
                                sha256: Some(a.sha256_hex()),
                                extra: a.extra.clone(),
                            },
                        )
                    })
                    .collect(),
            ),
            profile: Some(RawProfile {
                launch_budget_ms: Some(self.profile.launch_budget_ms),
                max_frame_mb: Some(self.profile.max_frame_mb),
            }),
        }
    }
}

/// Forma intermedia de artefacto (pase 1 de validación).
struct ArtForm {
    /// Clave del artefacto.
    key: String,
    /// Path saneado.
    path: RelPath,
    /// Path AOT saneado (si declaró `aot`).
    aot: Option<RelPath>,
}

/// Helper: error de campo ausente.
fn missing(field: &str) -> PkgError {
    PkgError::MissingField {
        field: field.to_owned(),
    }
}

/// Semántica del artefacto (pase 2): ubicación por clave, sha256, extras.
fn artifact_semantic(f: ArtForm, ra: &RawArtifact) -> Result<Artifact, PkgError> {
    let key: &str = &f.key;
    if key == ARTIFACT_NATIVE && !f.path.is_under("bin/native-aarch64") {
        return Err(PkgError::BadArtifact {
            key: key.to_owned(),
            reason: "el path de «native» debe estar bajo bin/native-aarch64/",
        });
    }
    if key == ARTIFACT_WASM && (!f.path.is_under("bin/wasm") || !f.path.as_str().ends_with(".wasm"))
    {
        return Err(PkgError::BadArtifact {
            key: key.to_owned(),
            reason: "el path de «wasm» debe estar bajo bin/wasm/ con sufijo .wasm",
        });
    }
    let sha_s = ra
        .sha256
        .as_deref()
        .ok_or_else(|| missing(&format!("artifacts.{key}.sha256")))?;
    let sha256 = hex::decode32(sha_s).map_err(|e| PkgError::BadSha256 { reason: e.reason() })?;
    if let Some(aot) = &f.aot {
        if key != ARTIFACT_WASM {
            return Err(PkgError::BadArtifact {
                key: key.to_owned(),
                reason: "«aot» solo tiene sentido en el artefacto wasm",
            });
        }
        if !aot.is_under("bin/wasm") || !aot.as_str().ends_with(".aot") {
            return Err(PkgError::BadArtifact {
                key: key.to_owned(),
                reason: "«aot» debe estar bajo bin/wasm/ con sufijo .aot",
            });
        }
    }
    if let Some(wr) = ra.extra.get("wasm_runtime") {
        if key != ARTIFACT_WASM {
            return Err(PkgError::BadArtifact {
                key: key.to_owned(),
                reason: "«wasm_runtime» solo tiene sentido en el artefacto wasm",
            });
        }
        parse_wasm_runtime(wr)?;
    }
    Ok(Artifact {
        path: f.path,
        sha256,
        extra: ra.extra.clone(),
    })
}

/// Nombre de capability en el dialecto del manifest (docs/06 §3, docs/07 §3:
/// forma punteada). Solo se acepta ESTA forma (estricto).
fn cap_from_manifest_name(s: &str) -> Option<Capability> {
    Some(match s {
        "net.client" => Capability::NetClient,
        "net.server" => Capability::NetServer,
        "clipboard.read" => Capability::ClipboardRead,
        "clipboard.write" => Capability::ClipboardWrite,
        "notify" => Capability::Notify,
        "share" => Capability::Share,
        "open-uri" => Capability::OpenUri,
        "vibrate" => Capability::Vibrate,
        "fs.vault" => Capability::FsVault,
        "system.store.read" => Capability::SystemStoreRead,
        "background-audio" => Capability::BackgroundAudio,
        _ => return None,
    })
}

/// Inverso de [`cap_from_manifest_name`] (para serializar).
///
/// NOTA: `Capability` es `#[non_exhaustive]`; si `arca-types` añade una
/// nueva capability antes de que este dialecto la mapee, se emite su nombre
/// canónico kebab (ese manifest luego no re-parsearía en ESTA versión del
/// modelo: falla explícito mejor que `unreachable`).
fn cap_to_manifest_name(c: Capability) -> &'static str {
    match c {
        Capability::NetClient => "net.client",
        Capability::NetServer => "net.server",
        Capability::ClipboardRead => "clipboard.read",
        Capability::ClipboardWrite => "clipboard.write",
        Capability::Notify => "notify",
        Capability::Share => "share",
        Capability::OpenUri => "open-uri",
        Capability::Vibrate => "vibrate",
        Capability::FsVault => "fs.vault",
        Capability::SystemStoreRead => "system.store.read",
        Capability::BackgroundAudio => "background-audio",
        _ => c.as_str(),
    }
}

/// Parse de `backend_pref` (string → enum, con contexto de campo).
fn parse_backend_pref(s: &str) -> Result<BackendPref, PkgError> {
    match s {
        "native" => Ok(BackendPref::Native),
        "wasm" => Ok(BackendPref::Wasm),
        "any" => Ok(BackendPref::Any),
        _ => Err(PkgError::BadEnum {
            field: "runtime.backend_pref",
            value: s.to_owned(),
        }),
    }
}

/// Parse de `respawn`.
fn parse_respawn(s: &str) -> Result<RespawnPolicy, PkgError> {
    match s {
        "never" => Ok(RespawnPolicy::Never),
        "on-crash" => Ok(RespawnPolicy::OnCrash),
        "always" => Ok(RespawnPolicy::Always),
        _ => Err(PkgError::BadEnum {
            field: "runtime.respawn",
            value: s.to_owned(),
        }),
    }
}

/// Parse de `ui.windows`.
fn parse_windows(s: &str) -> Result<WindowsMode, PkgError> {
    match s {
        "single" => Ok(WindowsMode::Single),
        "multi" => Ok(WindowsMode::Multi),
        _ => Err(PkgError::BadEnum {
            field: "runtime.ui.windows",
            value: s.to_owned(),
        }),
    }
}

/// Parse de `wasm_runtime` (docs/06 §3: "wamr-aot | wamr-interp | wasmtime").
fn parse_wasm_runtime(s: &str) -> Result<(), PkgError> {
    match s {
        "wamr-aot" | "wamr-interp" | "wasmtime" => Ok(()),
        _ => Err(PkgError::BadEnum {
            field: "artifacts.wasm.wasm_runtime",
            value: s.to_owned(),
        }),
    }
}

/// `package.name`: 1..=128 chars, sin controles, sin marcas combinantes
/// (U+0300..U+036F).
///
/// NOTA(agent): la spec dice "normalizar NFC en parse", pero
/// `unicode-normalization` no está en las dependencias permitidas (spec 02
/// §2) y std no trae normalización. Desvío documentado: se RECHAZA la forma
/// descompuesta (las precompuestas siguen siendo válidas); cubre el caso
/// "Unicode raro" de la tabla de errores sin añadir dependencias.
fn validate_name(name: &str) -> Result<(), PkgError> {
    let n = name.chars().count();
    if n == 0 || n > 128 {
        return Err(PkgError::BadName {
            value: name.to_owned(),
            reason: "la longitud debe ser 1..=128 caracteres",
        });
    }
    if name.chars().any(char::is_control) {
        return Err(PkgError::BadName {
            value: name.to_owned(),
            reason: "contiene caracteres de control",
        });
    }
    if name.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c)) {
        return Err(PkgError::BadName {
            value: name.to_owned(),
            reason: "marcas combinantes (usar la forma NFC precompuesta)",
        });
    }
    Ok(())
}

/// Autor: 1..=128 chars, sin controles.
fn validate_author(a: &str) -> Result<(), PkgError> {
    let n = a.chars().count();
    if n == 0 || n > 128 {
        return Err(PkgError::BadAuthor {
            value: a.to_owned(),
            reason: "la longitud debe ser 1..=128 caracteres",
        });
    }
    if a.chars().any(char::is_control) {
        return Err(PkgError::BadAuthor {
            value: a.to_owned(),
            reason: "contiene caracteres de control",
        });
    }
    Ok(())
}

/// Descripción: ≤ 1024 chars (se permiten \n, \r, \t por TOML multiline).
fn validate_description(d: &str) -> Result<(), PkgError> {
    let extracto: String = d.chars().take(32).collect();
    if d.chars().count() > 1024 {
        return Err(PkgError::BadDescription {
            value: extracto,
            reason: "excede 1024 caracteres",
        });
    }
    if d.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(PkgError::BadDescription {
            value: extracto,
            reason: "contiene caracteres de control",
        });
    }
    Ok(())
}

/// Tag: `[a-z0-9-]` y 1..=32 (machine-friendly para queries del store).
fn validate_tag(t: &str) -> Result<(), PkgError> {
    if t.is_empty() || t.len() > 32 {
        return Err(PkgError::BadTag {
            value: t.to_owned(),
            reason: "la longitud debe ser 1..=32",
        });
    }
    if !t
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(PkgError::BadTag {
            value: t.to_owned(),
            reason: "solo [a-z0-9-]",
        });
    }
    Ok(())
}

/// `runtime.entry`: 1..=64 bytes, charset `[A-Za-z0-9._-]`.
fn validate_entry(entry: &str) -> Result<(), PkgError> {
    if entry.is_empty() || entry.len() > 64 {
        return Err(PkgError::BadEntry {
            value: entry.to_owned(),
            reason: "la longitud debe ser 1..=64 bytes",
        });
    }
    if !entry
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(PkgError::BadEntry {
            value: entry.to_owned(),
            reason: "solo [A-Za-z0-9._-]",
        });
    }
    Ok(())
}

/// Atlas: potencia de 2 en 64..=16384.
fn validate_atlas(v: u32) -> Result<(), PkgError> {
    if !v.is_power_of_two() || !(64..=16_384).contains(&v) {
        return Err(PkgError::OutOfRange {
            field: "runtime.ui.atlas",
            value: v.to_string(),
            range: ATLAS_RANGE,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_roundtrip_completo() {
        for c in Capability::all() {
            let name = cap_to_manifest_name(*c);
            assert_eq!(cap_from_manifest_name(name), Some(*c), "{name}");
        }
    }

    #[test]
    fn caps_rechaza_variantes_no_canonicas() {
        // El dialecto del manifest es la forma punteada de docs/06/07:
        for s in [
            "net-client",
            "NET.CLIENT",
            "net.client ",
            "fs-vault",
            "background.audio",
            "net.admin",
            "",
        ] {
            assert!(cap_from_manifest_name(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn enums_as_str_canonico() {
        assert_eq!(BackendPref::Native.as_str(), "native");
        assert_eq!(BackendPref::Wasm.as_str(), "wasm");
        assert_eq!(BackendPref::Any.as_str(), "any");
        assert_eq!(RespawnPolicy::OnCrash.as_str(), "on-crash");
        assert_eq!(WindowsMode::Multi.as_str(), "multi");
    }

    #[test]
    fn validadores_de_texto() {
        assert!(validate_name("Mi Teclado Pro").is_ok());
        assert!(validate_name("tú también").is_ok()); // precompuesta
        assert!(validate_name("Cafe\u{301}").is_err()); // descompuesta
        assert!(validate_name("").is_err());
        assert!(validate_entry("app").is_ok());
        assert!(validate_entry("").is_err());
        assert!(validate_entry("app/../../x").is_err());
        assert!(validate_tag("tools").is_ok());
        assert!(validate_tag("Tools!").is_err());
        assert!(validate_atlas(2048).is_ok());
        assert!(validate_atlas(1000).is_err());
        assert!(validate_atlas(32).is_err());
        assert!(validate_atlas(32_768).is_err());
    }
}
