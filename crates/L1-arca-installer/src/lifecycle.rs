//! Uninstall y rollback (docs/10 §8 y §7, spec 12 §3).

use std::fs;

use arca_types::{ArcaError, Res};
use semver::Version;

use crate::{
    audit_detail, chmod_0700, parse_version, remove_path, sync_dir, version_dir_name, Installer,
    TRASH_DIR, VERSION_PREFIX,
};

impl Installer {
    /// Desinstala una app: registro del store PRIMERO, árbol de archivos
    /// después (docs/10 §8).
    ///
    /// Orden elegido (crash-safe): si el proceso muere tras el commit del
    /// store pero antes de borrar el árbol, el sweep del siguiente arranque
    /// elimina `apps/<id>/` completo (app sin registro = basura). El orden
    /// inverso dejaría el bug "instalada pero sin binario" (spec 12 §5).
    ///
    /// # Errors
    /// - [`ArcaError::NotFound`]: la app no está instalada.
    /// - [`ArcaError::Io`]: borrado del árbol (el store YA quedó consistente;
    ///   sweep completa al siguiente arranque).
    pub fn uninstall(&self, id: &arca_types::AppId) -> Res<()> {
        let Some(rec) = self.store.get_app(id)? else {
            return Err(ArcaError::NotFound(id.clone()));
        };
        // Las caps concedidas se registran como evidencia ANTES de que la
        // cascada del DELETE las retire (el audit es append-only y sobrevive).
        let caps = self
            .store
            .caps_of(id)
            .map(|set| set.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let detail = format!("uninstall v{}", rec.version);

        let mut tx = self.store.begin()?;
        self.store.delete_app(&mut tx, id)?;
        tx.commit()?;

        let app_dir = self.app_dir(id);
        if app_dir.exists() {
            remove_path(&app_dir)?;
            sync_dir(&self.apps_dir());
        }
        audit_detail(self, id, &caps, &detail);
        Ok(())
    }

    /// Restaurar la versión anterior (`.trash`, docs/06 §7: "rollback en 1
    /// clic"). Devuelve la versión restaurada.
    ///
    /// Semántica de **swap**: la versión activa actual pasa a `.trash` (se
    /// puede volver a avanzar con otro rollback), la restaurada vuelve a
    /// `v<semver>` y el store se re-registra con SU manifest.
    ///
    /// # Errors
    /// - [`ArcaError::NotFound`]: la app no está instalada.
    /// - [`ArcaError::Internal`]: `.trash` vacío (nada que restaurar — p. ej.
    ///   `keep_old=false` o dos rollbacks seguidos) o .trash con estado
    ///   ilegible.
    pub fn rollback(&self, id: &arca_types::AppId) -> Res<Version> {
        let Some(rec) = self.store.get_app(id)? else {
            return Err(ArcaError::NotFound(id.clone()));
        };
        let app_dir = self.app_dir(id);
        let trash = app_dir.join(TRASH_DIR);
        let active_name = version_dir_name(&parse_version(&rec.version)?);

        // .trash puede traer la activa (crash a mitad de rollback) más la
        // anterior: se restaura la que NO es la activa.
        let mut candidatos: Vec<String> = Vec::new();
        if trash.is_dir() {
            for e in fs::read_dir(&trash).map_err(ArcaError::Io)? {
                let e = e.map_err(ArcaError::Io)?;
                let name = e.file_name().to_string_lossy().to_string();
                if name != active_name {
                    candidatos.push(name);
                }
            }
        }
        if candidatos.len() != 1 {
            tracing::error!(
                target: "arca::arca-installer",
                candidatos = ?candidatos,
                "rollback sin versión anterior clara en .trash"
            );
            return Err(ArcaError::Internal(
                "installer: no hay versión anterior en .trash",
            ));
        }
        let prev_name = candidatos.remove(0);
        let ver_str = prev_name.strip_prefix(VERSION_PREFIX).unwrap_or(&prev_name);
        let prev_version = parse_version(ver_str)?;
        let prev_dir = trash.join(&prev_name);

        // El manifest restaurado re-registra la app (fuente de verdad).
        let manifest_bytes = fs::read(prev_dir.join(arca_sign::MANIFEST_PATH))
            .map_err(ArcaError::Io)?;
        let manifest = crate::Manifest::parse(&manifest_bytes)?;

        // 1. La activa actual (si sigue en su sitio) pasa a .trash.
        let active_dir = app_dir.join(&active_name);
        if active_dir.exists() {
            let trashed_active = trash.join(&active_name);
            if trashed_active.exists() {
                // Resto de un rollback interrumpido: se descarta.
                remove_path(&trashed_active)?;
            }
            fs::rename(&active_dir, &trashed_active).map_err(ArcaError::Io)?;
        }
        // 2. La anterior vuelve a su nombre final.
        fs::rename(&prev_dir, app_dir.join(&prev_name)).map_err(ArcaError::Io)?;
        chmod_0700(&app_dir)?;
        sync_dir(&app_dir);

        // 3. Store con el manifest restaurado.
        let mut tx = self.store.begin()?;
        self.store
            .upsert_app(&mut tx, &manifest, rec.installed_from)?;
        tx.commit()?;

        let detail = format!("rollback v{} → v{}", rec.version, prev_version);
        audit_detail(self, id, &manifest.requested_caps().to_vec(), &detail);
        Ok(prev_version)
    }
}
