//! `arca-installer` — flujo completo de instalación de paquetes `.arca`.
//!
//! Capa L1 · unsafe: no · Contrato: `specs/arca-12-installer.md` ·
//! Grafo: `graphs/installer.mmd`.
//!
//! Flujo (docs/10 §1, verify-while-extract):
//!
//! ```text
//! PackageSource → 7z open+entries → validate_layout (pkg-model)
//!   → pass1: manifest.toml + signature.bin + manifest.digest a memoria
//!   → StreamingVerifier (arca-sign) + extract a staging con tee por bloque
//!   → finish() (shas + digest canónico + ed25519)
//!   → rename staging→v<N> + swap symlink current + store commit (atómico)
//! ```
//!
//! Invariantes (spec 12 §4):
//! - **Verify-before-trust**: nada no verificado queda en disco usable:
//!   la extracción va a `.tmp-*` y el rename solo ocione tras
//!   [`arca_sign::StreamingVerifier::finish`] OK.
//! - Atómico por versión: `v<N>/.tmp` → `v<N>` → symlink `current` → store.
//! - Downgrade prohibido salvo [`InstallOpts::allow_downgrade`].
//! - [`Installer::sweep`] corre en cada arranque del host (init_host).
//!
//! Enmiendas documentadas (worklog T08):
//! - `install()` síncrono con callback de progreso (el `Progressable` de la
//!   spec requiere infra de streaming que v1 no tiene).
//! - PUENTE RelPath: `arca-7z` y `arca-pkg-model` definen tipos `RelPath`
//!   distintos; aquí se convierte siempre con `as_str()`.
//! - Cobertura de verificación: solo archivos DECLARADOS con sha256
//!   (artefactos). `assets/`/`icons/` sin declarar se extraen sin pin de
//!   firma (docs/06 §5 pide "todos"; el pin total llega en F6 cuando el
//!   manifest soporte artefactos genéricos). `bin/` está 100 % cubierto
//!   (regla 7 de pkg-model).
//! - Symlinks: `EntryInfo` de arca-7z no expone el bit de symlink: un
//!   symlink llegaría como archivo con contenido=ruta y fallaría el sha
//!   si está declarado. Detección explícita en F6 (hardening).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

mod flow;
mod sink;
mod source;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arca_sign::RingOfTrust;
use arca_store::Store;
use arca_types::{AppId, Capability, Res};
use semver::Version;

pub use source::PackageSource;

/// Origen del flujo de instalación.
pub(crate) type DynProgress<'a> = &'a mut dyn FnMut(InstallProgress);

/// Fase del flujo (para UI/logs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Phase {
    /// Apertura del 7z + listing.
    Open,
    /// Parse + validación del manifest.
    Manifest,
    /// Extracción + verificación streaming.
    Verify,
    /// Commit atómico (rename + symlink + store).
    Commit,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Manifest => "manifest",
            Self::Verify => "verify",
            Self::Commit => "commit",
        })
    }
}

/// Evento de progreso de instalación (fase + fracción `[0,1]`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstallProgress {
    /// Fase en curso.
    pub phase: Phase,
    /// Progreso dentro de la fase (y `1.0` al cerrarla).
    pub fraction: f64,
}

/// Opciones de instalación (spec 12 §3).
#[derive(Debug, Clone, Default)]
pub struct InstallOpts {
    /// Permitir instalar una versión MENOR a la registrada.
    pub allow_downgrade: bool,
    /// Capabilities a conceder automáticamente al instalar
    /// (subconjunto de las pedidas por el manifest; sin grants el host
    /// las pide por UI).
    pub auto_grant_caps: Option<Vec<Capability>>,
}

/// Resultado de un flujo de instalación.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InstallOutcome {
    /// Instalación fresca.
    Installed {
        /// App instalada.
        app: AppId,
        /// Versión instalada.
        version: Version,
    },
    /// Sustitución de una versión previa (la vieja queda en `.trash`).
    Updated {
        /// App actualizada.
        app: AppId,
        /// Versión previa (rollback disponible).
        from: Version,
        /// Versión nueva.
        to: Version,
    },
    /// Reservado para el flujo F4 (update en caliente con auto-rollback).
    /// Hoy [`Installer::install`] es fail-closed: nunca devuelve esto.
    RolledBack {
        /// App restaurada.
        app: AppId,
        /// Versión restaurada.
        to: Version,
    },
}

/// El instalador: raíz de datos + store + anillo de confianza.
pub struct Installer {
    /// Raíz `files/` del host: `<root>/<app-id>/v<N>/...`.
    root: PathBuf,
    /// Registro (SQLite). `Arc` porque el host-core lo comparte.
    store: Arc<Store>,
    /// Pubkeys embebidas (ADR-012).
    ring: RingOfTrust,
}

impl Installer {
    /// Construye el instalador. NO crea directorios (eso es `sweep`/install).
    #[must_use]
    pub fn new(root: PathBuf, store: Arc<Store>, ring: RingOfTrust) -> Self {
        Self { root, store, ring }
    }

    /// Instala un paquete (sin progreso).
    pub fn install(&self, src: PackageSource, opts: &InstallOpts) -> Res<InstallOutcome> {
        self.install_with_progress(src, opts, &mut |_| {})
    }

    /// Instala con callback de progreso.
    pub fn install_with_progress(
        &self,
        src: PackageSource,
        opts: &InstallOpts,
        progress: DynProgress<'_>,
    ) -> Res<InstallOutcome> {
        flow::install(self, src, opts, progress)
    }

    /// Desinstala: borra el árbol de la app + registro (tx: registro solo
    /// se borra si el árbol se eliminó).
    pub fn uninstall(&self, id: &AppId) -> Res<()> {
        flow::uninstall(self, id)
    }

    /// Vuelve a la versión previa (`.trash`). Devuelve la versión restaurada.
    pub fn rollback(&self, id: &AppId) -> Res<Version> {
        flow::rollback(self, id)
    }

    /// Limpieza de arranque: `.tmp-*` huérfanos + symlink `current` roto.
    /// Devuelve el nº de reparaciones.
    pub fn sweep(&self) -> Res<usize> {
        flow::sweep(self)
    }

    /// Re-sha del disco contra el manifest instalado (anti-tamper).
    pub fn verify_installed(&self, id: &AppId) -> Res<()> {
        flow::verify_installed(self, id)
    }

    /// Directorio de datos de una app (`<root>/<id>`).
    #[must_use]
    pub fn app_dir(&self, id: &AppId) -> PathBuf {
        self.root.join(id.as_str())
    }

    /// Directorio de la versión activa (`<root>/<id>/current`, resuelto).
    /// Err si no hay instalación válida.
    pub fn current_dir(&self, id: &AppId) -> Res<PathBuf> {
        let cur = self.app_dir(id).join("current");
        if cur.is_symlink() {
            let tgt = std::fs::read_link(&cur).map_err(arca_types::ArcaError::Io)?;
            // target relativo al directorio de la app
            let resolved = if tgt.is_absolute() {
                tgt
            } else {
                self.app_dir(id).join(tgt)
            };
            Ok(resolved)
        } else {
            Err(arca_types::ArcaError::NotFound(id.clone()))
        }
    }

    /// Store compartido (diagnóstico/launcher).
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Anillo de confianza (diagnóstico).
    #[must_use]
    pub fn ring(&self) -> &RingOfTrust {
        &self.ring
    }

    /// Raíz de datos.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}
