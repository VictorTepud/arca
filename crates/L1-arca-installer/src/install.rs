//! El flujo de `install` (docs/10 §1, spec 12 §3): verify-while-extract →
//! commit atómico → registro en el store.

use std::collections::BTreeMap;
use std::fs;

use arca_7z::{Archive, DirSink, ExtractPlan};
use arca_pkg_model::{ArchiveEntries, ArchiveEntry, EntryKind, Manifest};
use arca_sign::{
    PackageSignature, StreamingVerifier, MANIFEST_DIGEST_PATH, MANIFEST_PATH, SIGNATURE_PATH,
};
use arca_store::AppRecord;
use arca_types::{ArcaError, Digest, Res};

use crate::opts::{InstallOpts, InstallOutcome, PackageSource};
use crate::progress::{InstallPhase, InstallProgress};
use crate::sink::{MemorySink, VerifySink};
use crate::{
    chmod_0700, clear_dir, parse_version, remove_path, sync_dir, version_dir_name, Installer,
    TRASH_DIR,
};

impl Installer {
    /// Instala (o actualiza) un paquete `.arca`.
    ///
    /// Flujo (ver docs del crate para el diagrama completo):
    ///
    /// 1. `sweep` de restos de crashes anteriores.
    /// 2. Abre el 7z y lista las entradas (pre-check: todo path saneable).
    /// 3. **Fase Manifest**: extracción selectiva a memoria de `manifest.toml`
    ///    + `meta/signature.bin` + `meta/manifest.digest` (≤ 64 KiB cada una);
    ///    `Manifest::parse` + `validate_layout` + `manifest.digest` ==
    ///    blake3(manifest) (control temprano, docs/06 §2).
    /// 4. Política de versiones contra el store (downgrade/reinstalación).
    /// 5. **Fase Extract**: extracción COMPLETA a `apps/<id>/.staging-<rand>/`
    ///    con [`VerifySink`] (tee hacia el [`StreamingVerifier`]).
    /// 6. **Fase Verify**: `verifier.finish()` — shas de artefactos, blake3
    ///    del manifest, digest canónico y firma ed25519 contra el anillo.
    /// 7. **Fase Commit**: versión anterior → `.trash`, staging → `v<semver>`
    ///    (rename atómico + fsync de dir) y `Tx { upsert_app }` → commit.
    ///
    /// Cualquier fallo → el staging lo borra su Drop-guard y el estado en
    /// disco/store queda como estaba (si el commit de store falla tras el
    /// rename, el swap se revierte internamente antes de devolver el error).
    ///
    /// # Errors
    /// - [`ArcaError::InvalidSignature`]: firma que no valida contra el anillo.
    /// - [`ArcaError::InvalidPackage`]: layout/shas/digest/manifest inválidos,
    ///   downgrade sin permiso, o paquete sin backend para esta variante.
    /// - [`ArcaError::Io`]: E/S del paquete o del disco.
    /// - [`ArcaError::NotFound`] nunca (la ausencia previa es "instalar").
    pub fn install(&self, src: PackageSource, mut opts: InstallOpts) -> Res<InstallOutcome> {
        let mut progress = opts.progress.take();
        let mut emit = |phase: InstallPhase, frac: f64| {
            if let Some(cb) = progress.as_mut() {
                cb(InstallProgress { phase, frac });
            }
        };

        // 0. Restos de crashes anteriores: NUNCA se instala sobre basura
        // (task T08 §8: sweep al inicio de install).
        let swept = self.sweep()?;
        if swept > 0 {
            tracing::info!(
                target: "arca::arca-installer",
                swept,
                "sweep previo a install limpió restos"
            );
        }

        // 1. Abrir + listado (sin descomprimir contenido).
        let reader = src.into_reader()?;
        let mut archive = Archive::open(reader)?;
        let entries = archive.entries()?;
        precheck_paths(&entries)?;

        // 2. Fase Manifest: metadatos a memoria (sin tocar disco).
        emit(InstallPhase::Manifest, 0.0);
        let mut mem = MemorySink::new();
        let plan_meta = ExtractPlan::parse(&[MANIFEST_PATH, SIGNATURE_PATH, MANIFEST_DIGEST_PATH])?;
        {
            let mut mprog = |frac: f64| emit(InstallPhase::Manifest, frac);
            archive.extract(&plan_meta, &mut mem, &mut mprog)?;
        }
        let manifest_bytes = mem.take(MANIFEST_PATH)?;
        let manifest = Manifest::parse(&manifest_bytes)?;
        let listing = to_listing(&entries);
        manifest.validate_layout(&listing)?;

        // 3. Firma + digest de control del manifest.
        let psig = PackageSignature::parse(&mem.take(SIGNATURE_PATH)?)?;
        let digest_bytes = mem.take(MANIFEST_DIGEST_PATH)?;
        let digest_txt = String::from_utf8(digest_bytes).map_err(|_| {
            ArcaError::InvalidPackage("installer: meta/manifest.digest no es UTF-8")
        })?;
        let claimed = Digest::from_hex(digest_txt.trim())?;
        let manifest_sha = Digest::of(&manifest_bytes);
        if claimed != manifest_sha {
            tracing::error!(
                target: "arca::arca-installer",
                declared = %claimed,
                real = %manifest_sha,
                "manifest.digest no coincide con blake3(manifest.toml)"
            );
            return Err(ArcaError::InvalidPackage(
                "installer: manifest.digest no coincide con el manifest",
            ));
        }
        emit(InstallPhase::Manifest, 1.0);

        // 4. El paquete debe poder ejecutarse en ESTA variante de host.
        manifest.backend_for(self.host)?;

        // 5. Política de versiones (antes de gastar disco en staging).
        let id = manifest.package.id.clone();
        let prev = self.store.get_app(&id)?;
        if let Some(rec) = &prev {
            let cur = parse_version(&rec.version)?;
            let new = &manifest.package.version;
            if cur > *new && !opts.allow_downgrade {
                tracing::warn!(
                    target: "arca::arca-installer",
                    installed = %cur,
                    requested = %new,
                    "downgrade rechazado (InstallOpts::allow_downgrade)"
                );
                return Err(ArcaError::InvalidPackage(
                    "installer: downgrade prohibido sin allow_downgrade",
                ));
            }
        }

        // 6. Fase Extract: staging + verify-while-extract.
        let app_dir = self.ensure_app_dir(&id)?;
        let staging = tempfile::Builder::new()
            .prefix(crate::STAGING_PREFIX)
            .tempdir_in(&app_dir)
            .map_err(ArcaError::Io)?;
        chmod_0700(staging.path())?;

        let expected: BTreeMap<String, [u8; 32]> = manifest
            .artifacts
            .values()
            .map(|a| (a.path.as_str().to_owned(), a.sha256))
            .collect();
        let mut verifier =
            StreamingVerifier::new(&expected, manifest_sha.0, &self.ring, *psig.sig_bytes());
        emit(InstallPhase::Extract, 0.0);
        {
            let mut vsink = VerifySink::new(
                DirSink::new(staging.path().to_path_buf()),
                &mut verifier,
                &expected,
            );
            let mut eprog = |frac: f64| emit(InstallPhase::Extract, frac);
            archive.extract(&ExtractPlan::all(), &mut vsink, &mut eprog)?;
        }
        emit(InstallPhase::Verify, 0.0);
        verifier.finish()?;
        emit(InstallPhase::Verify, 1.0);

        // 7. Fase Commit: swap atómico de directorios + store.
        emit(InstallPhase::Commit, 0.0);
        let outcome = self.commit(&manifest, prev.as_ref(), staging, &opts)?;
        emit(InstallPhase::Commit, 1.0);
        Ok(outcome)
    }

    /// Commit atómico: trash de la anterior → rename del staging → Tx del
    /// store. Si el store falla tras el rename, el swap se revierte.
    fn commit(
        &self,
        manifest: &Manifest,
        prev: Option<&AppRecord>,
        staging: tempfile::TempDir,
        opts: &InstallOpts,
    ) -> Res<InstallOutcome> {
        let id = &manifest.package.id;
        let app_dir = self.app_dir(id);
        let v_new_name = version_dir_name(&manifest.package.version);
        let dest = app_dir.join(&v_new_name);
        let cur_name = match prev {
            Some(rec) => Some(version_dir_name(&parse_version(&rec.version)?)),
            None => None,
        };

        // 7a. La versión activa actual se aparta a .trash ANTES del rename:
        // si algo falla después, sweep() restaura desde .trash y nunca queda
        // "registrada pero sin árbol" (docs/06 §7, spec 12 §5).
        if let Some(cur_name) = cur_name.as_ref() {
            let cur = app_dir.join(cur_name);
            if cur.exists() {
                let trash = app_dir.join(TRASH_DIR);
                clear_dir(&trash)?;
                fs::rename(&cur, trash.join(cur_name)).map_err(ArcaError::Io)?;
                chmod_0700(&trash)?;
            }
        }

        // 7b. staging → v<semver> (atómico: mismo filesystem).
        fs::rename(staging.path(), &dest).map_err(ArcaError::Io)?;
        // El guard deja de ser responsable: el dir ya vive en su nombre final.
        let _ = staging.keep();
        sync_dir(&app_dir);

        // 7c. Registro: archivos primero, commit de db al final (T07 §orden).
        let store_op = || -> Res<()> {
            let mut tx = self.store.begin()?;
            self.store.upsert_app(&mut tx, manifest, opts.source)?;
            if let Some(caps) = &opts.auto_grant_caps {
                self.store.grant_caps(&mut tx, id, caps)?;
            }
            tx.commit()
        };
        if let Err(e) = store_op() {
            // Fail-safe: revertir el swap para no dejar v_new sin registro.
            tracing::error!(
                target: "arca::arca-installer",
                error = %e,
                "commit del store falló tras el rename: revirtiendo swap"
            );
            let _ = remove_path(&dest);
            if let Some(cur_name) = cur_name.as_ref() {
                let trashed = app_dir.join(TRASH_DIR).join(cur_name);
                if trashed.exists() {
                    if fs::rename(&trashed, app_dir.join(cur_name)).is_ok() {
                        sync_dir(&app_dir);
                    }
                }
            }
            return Err(e);
        }

        // 7d. keep_old=false: sin material de rollback.
        if !opts.keep_old {
            let _ = clear_dir(&app_dir.join(TRASH_DIR));
            sync_dir(&app_dir);
        }

        // 7e. Auditoría post-commit (best-effort: la Tx retiene el mutex del
        // store, `Store::audit` no puede anidarse — ver docs del crate).
        let detail = match prev {
            None => format!("install v{}", manifest.package.version),
            Some(rec) => format!("update v{} → v{}", rec.version, manifest.package.version),
        };
        crate::audit_detail(self, id, manifest.requested_caps(), &detail);

        Ok(match prev {
            None => InstallOutcome::Installed {
                app: id.clone(),
                version: manifest.package.version.clone(),
            },
            Some(rec) => InstallOutcome::Updated {
                app: id.clone(),
                from: parse_version(&rec.version)?,
                to: manifest.package.version.clone(),
            },
        })
    }
}

/// Pre-check del listing: TODO path debe ser saneable por `arca-7z`
/// (fail-fast antes de descomprimir; la segunda barrera vuelve a mirar cada
/// path justo antes de abrir el destino).
fn precheck_paths(entries: &[arca_7z::EntryInfo]) -> Res<()> {
    for e in entries {
        if e.safe_path().is_none() {
            tracing::error!(
                target: "arca::arca-installer",
                path = %e.path,
                "entrada con path rechazado por el sandbox (pre-check)"
            );
            return Err(ArcaError::InvalidPackage(
                "installer: path de entrada rechazado por el sandbox",
            ));
        }
    }
    Ok(())
}

/// Listing `arca-7z` → `ArchiveEntries` de pkg-model (para
/// `validate_layout`). El path viaja tal cual (crudo): la validación de
/// sintaxis la hace `RelPath::new` dentro de `validate_layout`.
///
/// NOTA(agent): sevenz-rust2 no expone "es symlink" en el listing, y su
/// decodificador escribe toda entrada como archivo regular (0600): un
/// symlink malicioso queda neutralizado ESTRUCTURALMENTE. `EntryKind::Symlink`
/// nunca se produce aquí; `verify_installed` sí detecta symlinks que alguien
/// plantó en disco después (anti-tamper).
fn to_listing(entries: &[arca_7z::EntryInfo]) -> ArchiveEntries {
    ArchiveEntries::from_entries(entries.iter().map(|e| {
        let kind = if e.is_dir {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        ArchiveEntry::new(e.path.clone(), kind)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precheck_rechaza_traversal() {
        let entries = vec![
            arca_7z::EntryInfo {
                path: "manifest.toml".to_owned(),
                size: 1,
                crc: None,
                is_dir: false,
            },
            arca_7z::EntryInfo {
                path: "../evil".to_owned(),
                size: 1,
                crc: None,
                is_dir: false,
            },
        ];
        assert!(matches!(
            precheck_paths(&entries),
            Err(ArcaError::InvalidPackage(_))
        ));
        assert!(precheck_paths(&entries[..1]).is_ok());
    }

    #[test]
    fn listing_mapea_kinds() {
        let entries = vec![
            arca_7z::EntryInfo {
                path: "manifest.toml".to_owned(),
                size: 1,
                crc: None,
                is_dir: false,
            },
            arca_7z::EntryInfo {
                path: "bin".to_owned(),
                size: 0,
                crc: None,
                is_dir: true,
            },
        ];
        let l = to_listing(&entries);
        assert!(l.has_file("manifest.toml"));
        assert!(!l.has_file("bin"));
    }
}
