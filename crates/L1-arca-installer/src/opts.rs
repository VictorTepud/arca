//! Opciones y resultados de la instalación (spec 12 §3).

use std::io::{Read, Seek};
use std::path::PathBuf;

use arca_store::InstallSource;
use arca_types::{AppId, Capability};
use semver::Version;

use crate::progress::InstallProgress;

/// Lector aleatorio unificado (los trait-objects de std no aceptan
/// `dyn Read + Seek` combinados: se define el super-trait vacío).
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Callback de progreso (enmienda del `Progressable` de la spec: ese trait no
/// existe en el workspace y arca-types está cerrado — ver docs del crate).
pub type ProgressFn = Box<dyn FnMut(InstallProgress)>;

/// De dónde salen los bytes del `.arca` (spec 12 §3: FileFd / Bytes / Path).
///
/// - **FileFd (SAF/uri)**: el glue de Android envuelve el fd del picker en un
///   `File`/reader arbitrario → [`PackageSource::Reader`] (este crate veta el
///   `unsafe` de `from_raw_fd`, lo hace el glue).
/// - **Bytes (dev-mode)**: el paquete ya está en RAM (p. ej. inyectado por
///   tests o tools-dev).
/// - **Path (tools-dev adb)**: un `.arca` en disco.
pub enum PackageSource {
    /// Ruta a un `.arca` en disco (tests, tools-dev tras `adb push`).
    Path(PathBuf),
    /// Lector ya abierto (fd del SAF envuelto por el glue, o un wrapper de
    /// test que inyecta fallos de E/S).
    Reader(Box<dyn ReadSeek>),
    /// Contenido completo en memoria (dev-mode).
    Bytes(Vec<u8>),
}

impl PackageSource {
    /// Convierte en el lector que consume `Archive::open`.
    ///
    /// # Errors
    /// [`ArcaError::Io`] si `Path` no se puede abrir.
    pub(crate) fn into_reader(self) -> arca_types::Res<Box<dyn ReadSeek>> {
        use arca_types::ArcaError;
        Ok(match self {
            Self::Path(p) => {
                let f = std::fs::File::open(&p).map_err(|e| {
                    tracing::error!(
                        target: "arca::arca-installer",
                        path = %p.display(),
                        error = %e,
                        "abrir paquete .arca"
                    );
                    ArcaError::Io(e)
                })?;
                Box::new(f)
            }
            Self::Reader(r) => r,
            Self::Bytes(b) => Box::new(std::io::Cursor::new(b)),
        })
    }
}

/// Opciones de una instalación (spec 12 §3 + extensiones documentadas).
pub struct InstallOpts {
    /// Permitir instalar una versión MENOR que la registrada (downgrade).
    /// `false` por defecto (docs/06 §7).
    pub allow_downgrade: bool,
    /// Capabilities EXTRA concedidas además de las del manifest (dev-mode:
    /// auto-grant sin diálogo). `None` = solo las del manifest (que el store
    /// ya concede al instalar, decisión T07).
    pub auto_grant_caps: Option<Vec<Capability>>,
    /// Conservar la versión anterior en `.trash` (rollback en 1 clic,
    /// docs/06 §7). `true` por defecto.
    pub keep_old: bool,
    /// Origen de la instalación (se persiste en `apps.installed_from`).
    /// `User` por defecto (SAF picker).
    pub source: InstallSource,
    /// Callback de progreso (fases + fracción). `None` = sin notificaciones.
    pub progress: Option<ProgressFn>,
}

impl Default for InstallOpts {
    fn default() -> Self {
        Self {
            allow_downgrade: false,
            auto_grant_caps: None,
            keep_old: true,
            source: InstallSource::User,
            progress: None,
        }
    }
}

impl InstallOpts {
    /// Opciones por defecto (usuario, sin downgrade, con rollback).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dev-mode: downgrade permitido + origen `dev`.
    #[must_use]
    pub fn dev() -> Self {
        Self {
            allow_downgrade: true,
            source: InstallSource::Dev,
            ..Self::default()
        }
    }
}

impl std::fmt::Debug for InstallOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // El callback no es Debug: se resume.
        f.debug_struct("InstallOpts")
            .field("allow_downgrade", &self.allow_downgrade)
            .field("auto_grant_caps", &self.auto_grant_caps)
            .field("keep_old", &self.keep_old)
            .field("source", &self.source)
            .field("progress", &self.progress.as_ref().map(|_| "<cb>"))
            .finish()
    }
}

/// Resultado de una instalación (spec 12 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Primera instalación de la app.
    Installed {
        /// App instalada.
        app: AppId,
        /// Versión instalada.
        version: Version,
    },
    /// Ya existía un registro: se reemplazó (update, downgrade explícito o
    /// reinstalación de la misma versión — `from == to`).
    Updated {
        /// App actualizada.
        app: AppId,
        /// Versión que había registrada.
        from: Version,
        /// Versión instalada ahora.
        to: Version,
    },
    /// Reservado (spec 12 §3): el flujo devuelto cuando el HOST decide
    /// auto-restaurar tras un fallo post-commit. Hoy solo se produce vía
    /// [`crate::Installer::rollback`]; se mantiene por contrato.
    RolledBack {
        /// App restaurada.
        app: AppId,
        /// Versión restaurada.
        version: Version,
    },
}

impl InstallOutcome {
    /// La app afectada (comodidad para el launcher).
    #[must_use]
    pub fn app(&self) -> &AppId {
        match self {
            Self::Installed { app, .. }
            | Self::Updated { app, .. }
            | Self::RolledBack { app, .. } => app,
        }
    }

    /// La versión resultante (la instalada o la restaurada).
    #[must_use]
    pub fn version(&self) -> &Version {
        match self {
            Self::Installed { version, .. }
            | Self::RolledBack { version, .. } => version,
            Self::Updated { to, .. } => to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opts_default_conservador() {
        let o = InstallOpts::default();
        assert!(!o.allow_downgrade);
        assert!(o.auto_grant_caps.is_none());
        assert!(o.keep_old);
        assert_eq!(o.source, InstallSource::User);
        assert!(o.progress.is_none());
        let d = InstallOpts::dev();
        assert!(d.allow_downgrade);
        assert_eq!(d.source, InstallSource::Dev);
    }

    #[test]
    fn outcome_accessors() {
        let app = AppId::new("dev.x.y").unwrap();
        let o = InstallOutcome::Updated {
            app: app.clone(),
            from: Version::parse("1.0.0").unwrap(),
            to: Version::parse("2.0.0").unwrap(),
        };
        assert_eq!(o.app(), &app);
        assert_eq!(o.version(), &Version::parse("2.0.0").unwrap());
    }
}
