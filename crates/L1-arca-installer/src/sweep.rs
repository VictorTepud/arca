//! `sweep`: limpieza de restos de crashes (spec 12 §3/§4, docs/10 §1).
//!
//! Se llama al inicio de cada [`crate::Installer::install`] y al arrancar el
//! host. Convierte CUALQUIER estado intermedio de un crash en el estado que
//! dice el store (el store es la verdad):
//!
//! | estado en disco | acción |
//! |---|---|
//! | `apps/<id>/.staging-*` | se elimina (extracción sin commit) |
//! | `apps/<id>/v*` no registrada | se elimina (rename sin commit de store) |
//! | app SIN registro en el store | se elimina el árbol `apps/<id>/` completo |
//! | versión activa ausente + `.trash/<activa>` | se restaura desde `.trash` (update interrumpido pre-commit) |
//! | `.trash` | NO se toca (material de rollback, docs/06 §7) |

use std::fs;

use arca_types::{AppId, ArcaError, Res};

use crate::{parse_version, remove_path, sync_dir, version_dir_name, Installer, STAGING_PREFIX, TRASH_DIR, VERSION_PREFIX};

impl Installer {
    /// Limpia restos de crashes y devuelve cuántas reparaciones hizo
    /// (eliminaciones + restauraciones).
    ///
    /// # Errors
    /// [`ArcaError::Io`] si el listado o el borrado falla (se aborta: mejor
    /// un arranque ruidoso que un árbol a medias).
    pub fn sweep(&self) -> Res<usize> {
        let apps_dir = self.apps_dir();
        if !apps_dir.is_dir() {
            return Ok(0);
        }
        let mut count: usize = 0;
        for entry in fs::read_dir(&apps_dir).map_err(ArcaError::Io)? {
            let entry = entry.map_err(ArcaError::Io)?;
            let path = entry.path();
            if !path.is_dir() {
                // El namespace apps/ es del installer: un archivo suelto ahí
                // es basura, pero no interrumpe el arranque.
                tracing::warn!(
                    target: "arca::arca-installer::sweep",
                    path = %path.display(),
                    "archivo suelto en apps/ (se ignora)"
                );
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // apps/ solo contiene <appId>/; un dir con nombre inválido es
            // basura de un crash antiguo → árbol completo a la basura.
            let Some(id) = AppId::new(&name).ok() else {
                tracing::warn!(
                    target: "arca::arca-installer::sweep",
                    dir = %name,
                    "directorio sin nombre de AppId en apps/ (se elimina)"
                );
                remove_path(&path)?;
                count += 1;
                continue;
            };
            let rec = self.store.get_app(&id)?;
            let Some(rec) = rec else {
                // App sin registro: uninstall interrumpido tras el commit.
                tracing::info!(
                    target: "arca::arca-installer::sweep",
                    app = %id.as_str(),
                    "árbol sin registro en el store (se elimina)"
                );
                remove_path(&path)?;
                count += 1;
                continue;
            };
            count += self.sweep_app(&id, &version_dir_name(&parse_version(&rec.version)?))?;
        }
        Ok(count)
    }

    /// Limpia el árbol de UNA app registrada (staging + versiones sueltas +
    /// reparación de la activa).
    fn sweep_app(&self, id: &AppId, active_name: &str) -> Res<usize> {
        let app_dir = self.app_dir(id);
        let mut count: usize = 0;
        for entry in fs::read_dir(&app_dir).map_err(ArcaError::Io)? {
            let entry = entry.map_err(ArcaError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if name.starts_with(STAGING_PREFIX) {
                // Extracción sin commit: nunca fue visible ni verificada.
                tracing::info!(
                    target: "arca::arca-installer::sweep",
                    staging = %name,
                    "staging huérfano (se elimina)"
                );
                remove_path(&path)?;
                count += 1;
            } else if name.starts_with(VERSION_PREFIX) && name != active_name {
                // Directorio de versión sin registro (crash tras rename,
                // pre-commit del store). El prefijo v está reservado.
                tracing::info!(
                    target: "arca::arca-installer::sweep",
                    version = %name,
                    "versión no registrada (se elimina)"
                );
                remove_path(&path)?;
                count += 1;
            }
        }
        // Reparación: la activa no está en su sitio pero .trash la tiene
        // (update interrumpido entre el apartado y el rename — spec 12 §5
        // "sweep también repara").
        let active = app_dir.join(active_name);
        if !active.exists() {
            let trashed = app_dir.join(TRASH_DIR).join(active_name);
            if trashed.exists() {
                tracing::warn!(
                    target: "arca::arca-installer::sweep",
                    version = %active_name,
                    "versión activa ausente: restaurando desde .trash"
                );
                fs::rename(&trashed, &active).map_err(ArcaError::Io)?;
                sync_dir(&app_dir);
                count += 1;
            } else {
                tracing::error!(
                    target: "arca::arca-installer::sweep",
                    version = %active_name,
                    "app registrada sin árbol en disco ni .trash"
                );
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Installer, APPS_DIR};

    fn fixture() -> (tempfile::TempDir, Installer) {
        let tmp = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(
            arca_store::Store::open(&tmp.path().join("registry.db")).unwrap(),
        );
        let inst = Installer::new(tmp.path().join("files"), store, arca_sign::RingOfTrust::empty());
        (tmp, inst)
    }

    #[test]
    fn sweep_sin_apps_es_cero() {
        let (_tmp, inst) = fixture();
        assert_eq!(inst.sweep().unwrap(), 0);
    }

    #[test]
    fn sweep_elimina_directorio_basura_sin_appid() {
        let (tmp, inst) = fixture();
        let apps = tmp.path().join("files").join(APPS_DIR);
        std::fs::create_dir_all(apps.join("basura!!")).unwrap();
        std::fs::write(apps.join("basura!!/x.txt"), b"x").unwrap();
        assert_eq!(inst.sweep().unwrap(), 1);
        assert!(!apps.join("basura!!").exists());
        // Idempotente.
        assert_eq!(inst.sweep().unwrap(), 0);
    }
}
