//! Flujo de instalación/rollback/sweep (spec 12 §3).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::Digest as Sha2Digest;

use arca_7z::{Archive, ExtractPlan};
use arca_pkg_model::{ArchiveEntries, EntryKind, Manifest};
use arca_sign::{
    PackageSignature, RingOfTrust, StreamingVerifier, MANIFEST_DIGEST_PATH, MANIFEST_PATH,
    SIGNATURE_PATH,
};
use arca_store::{AuditEvent, InstallSource, UnixMs};
use arca_types::{now_mono_ns, AppId, ArcaError, Digest, Res};
use semver::Version;

use crate::sink::{MemorySink, StagingGuard, StagingSink};
use crate::source::PackageSource;
use crate::{DynProgress, InstallOpts, InstallOutcome, InstallProgress, Installer, Phase};

/// Nombre del symlink de versión activa.
const CURRENT: &str = "current";
/// Prefijo de staging.
const TMP_PREFIX: &str = ".tmp-";
/// Directorio de versión previa (rollback).
const TRASH: &str = ".trash";

/// Estado intermedio entre prepare y commit (tests de interrupción lo usan).
pub(crate) struct Prepared {
    /// Directorio staging (todavía no es la versión instalada).
    pub(crate) staging: PathBuf,
    /// Directorio de la app (`<root>/<id>`).
    pub(crate) app_dir: PathBuf,
    /// Manifest ya parseado y validado.
    pub(crate) manifest: Manifest,
    /// Bytes del manifest (reescritos a staging en pass 2).
    #[allow(dead_code)] // usado por tests de interrupción c/d
    pub(crate) manifest_bytes: Vec<u8>,
    /// Guard que borra staging si el flujo muere antes del rename.
    pub(crate) guard: StagingGuard,
}

/// Instala un paquete (flujo completo).
pub(crate) fn install(
    inst: &Installer,
    src: PackageSource,
    opts: &InstallOpts,
    progress: DynProgress<'_>,
) -> Res<InstallOutcome> {
    let source = InstallSource::User; // v1: todo entra como user (SAF/adb)
    let prepared = prepare(inst, src, &inst.ring, progress)?;
    commit(inst, prepared, source, opts, progress)
}

/// Pass 1 + verificación + extracción a staging (TODO lo verificable ocurre
/// aquí; el disco destino aún no existe como versión instalable).
fn prepare(
    inst: &Installer,
    src: PackageSource,
    ring: &RingOfTrust,
    progress: DynProgress<'_>,
) -> Res<Prepared> {
    progress(InstallProgress {
        phase: Phase::Open,
        fraction: 0.0,
    });
    let mut reader = src.into_reader()?;
    let mut archive = Archive::open(&mut reader)?;
    let entries = archive.entries()?;

    // Listing para validate_layout (kind aproximado: sin bit de symlink,
    // ver nota en lib.rs).
    let mut listing = ArchiveEntries::new();
    for e in &entries {
        listing.push(
            e.path.clone(),
            if e.is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
        );
    }
    progress(InstallProgress {
        phase: Phase::Open,
        fraction: 1.0,
    });

    // ---- pass 1: manifest + firma + digest de control a memoria ----
    progress(InstallProgress {
        phase: Phase::Manifest,
        fraction: 0.0,
    });
    let plan1 = ExtractPlan::parse(&[MANIFEST_PATH, SIGNATURE_PATH, MANIFEST_DIGEST_PATH])?;
    let mut mem = MemorySink::new();
    archive.extract(&plan1, &mut mem, &mut |f| {
        progress(InstallProgress {
            phase: Phase::Manifest,
            fraction: f * 0.5,
        });
    })?;
    let manifest_bytes = mem
        .get(MANIFEST_PATH)
        .ok_or(ArcaError::InvalidPackage("manifest.toml ausente"))?
        .to_vec();
    let sig_bytes = mem
        .get(SIGNATURE_PATH)
        .ok_or(ArcaError::InvalidPackage("meta/signature.bin ausente"))?;
    let psig = PackageSignature::parse(sig_bytes)?;
    let md_raw = mem
        .get(MANIFEST_DIGEST_PATH)
        .ok_or(ArcaError::InvalidPackage("meta/manifest.digest ausente"))?;
    let md_hex = String::from_utf8_lossy(md_raw).trim().to_ascii_lowercase();
    let manifest_sha = Digest::of(&manifest_bytes).0;
    if !matches!(Digest::from_hex(&md_hex), Ok(d) if d.0 == manifest_sha) {
        return Err(ArcaError::InvalidPackage(
            "meta/manifest.digest no coincide con el manifest",
        ));
    }

    let manifest = Manifest::parse(&manifest_bytes)?;
    manifest.validate_layout(&listing)?;
    progress(InstallProgress {
        phase: Phase::Manifest,
        fraction: 1.0,
    });

    // ---- verificador ----
    let expected: BTreeMap<String, [u8; 32]> = manifest
        .artifacts
        .values()
        .map(|a| (a.path.as_str().to_owned(), a.sha256))
        .collect();
    let mut verifier = StreamingVerifier::new(&expected, manifest_sha, ring, *psig.sig_bytes());

    // ---- pass 2: extracción a staging con tee ----
    let id = manifest.package.id.clone();
    let app_dir = inst.app_dir(&id);
    std::fs::create_dir_all(&app_dir)?;
    let staging = app_dir.join(format!("{TMP_PREFIX}{:016x}", now_mono_ns()));
    let guard = StagingGuard::new(staging.clone());

    // plan: todo menos los 3 archivos ya leídos (se drenan con CRC)
    let skip = [MANIFEST_PATH, SIGNATURE_PATH, MANIFEST_DIGEST_PATH];
    let wanted: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|e| e.safe_path())
        .map(|p| p.as_str().to_owned())
        .filter(|p| !skip.contains(&p.as_str()))
        .collect();
    let wanted_refs: Vec<&str> = wanted.iter().map(String::as_str).collect();
    let plan2 = ExtractPlan::parse(&wanted_refs)?;

    progress(InstallProgress {
        phase: Phase::Verify,
        fraction: 0.0,
    });
    {
        let mut sink = StagingSink::new(
            arca_7z::DirSink::new(staging.clone()),
            &mut verifier,
            expected.keys().cloned().collect::<BTreeSet<String>>(),
        );
        archive.extract(&plan2, &mut sink, &mut |f| {
            progress(InstallProgress {
                phase: Phase::Verify,
                fraction: f,
            });
        })?;
    }
    // manifest: escribir a staging + alimentar blake3 (plan2 lo excluyó)
    std::fs::write(staging.join(MANIFEST_PATH), &manifest_bytes)?;
    std::fs::set_permissions(
        staging.join(MANIFEST_PATH),
        std::fs::Permissions::from_mode(0o600),
    )?;
    verifier.feed(MANIFEST_PATH, &manifest_bytes)?;
    verifier.finish()?;
    mark_bin_executable(&staging.join("bin"))?;
    progress(InstallProgress {
        phase: Phase::Verify,
        fraction: 1.0,
    });

    // post-verify: artefactos declarados presentes en staging
    for a in manifest.artifacts.values() {
        if !staging.join(a.path.as_str()).is_file() {
            return Err(ArcaError::InvalidPackage(
                "post-verify: artefacto declarado ausente",
            ));
        }
    }

    Ok(Prepared {
        staging,
        app_dir,
        manifest,
        manifest_bytes,
        guard,
    })
}

/// `bin/` necesita exec: 0755 tras la extracción (DirSink escribe 0600).
/// Recursivo: `bin/native-aarch64/app` vive en subdirectorio.
fn mark_bin_executable(dir: &Path) -> Res<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            mark_bin_executable(&path)?;
        } else if path.is_file() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Commit atómico: rename → trash del anterior → symlink swap → store.
fn commit(
    inst: &Installer,
    prepared: Prepared,
    source: InstallSource,
    opts: &InstallOpts,
    progress: DynProgress<'_>,
) -> Res<InstallOutcome> {
    progress(InstallProgress {
        phase: Phase::Commit,
        fraction: 0.1,
    });
    let Prepared {
        staging,
        app_dir,
        manifest,
        manifest_bytes: _,
        guard,
    } = prepared;
    let id = manifest.package.id.clone();
    let version = manifest.package.version.clone();

    // downgrade
    let old = inst
        .store
        .get_app(&id)?
        .and_then(|r| Version::parse(&r.version).ok());
    if let Some(oldv) = &old {
        if version < *oldv && !opts.allow_downgrade {
            return Err(ArcaError::InvalidPackage(
                "downgrade no permitido (InstallOpts::allow_downgrade)",
            ));
        }
    }

    let vdir = app_dir.join(format!("v{version}"));
    if vdir.exists() {
        std::fs::remove_dir_all(&vdir)?; // reinstalación de la misma versión
    }
    std::fs::rename(&staging, &vdir)?;
    guard.defuse();
    progress(InstallProgress {
        phase: Phase::Commit,
        fraction: 0.4,
    });

    // conservar la versión previa en .trash (rollback)
    if let Some(oldv) = &old {
        if *oldv != version {
            let old_dir = app_dir.join(format!("v{oldv}"));
            if old_dir.is_dir() {
                let trash = app_dir.join(TRASH);
                std::fs::remove_dir_all(&trash).ok();
                std::fs::create_dir_all(&trash)?;
                std::fs::rename(old_dir, trash.join(format!("v{oldv}")))?;
            }
        }
    }
    swap_current(&app_dir, &version)?;
    progress(InstallProgress {
        phase: Phase::Commit,
        fraction: 0.7,
    });

    // store (si falla: mejor esfuerzo de marcha atrás fs)
    let outcome = register(inst, &manifest, source, opts, old.clone(), version.clone());
    if outcome.is_err() {
        // revert: v<new> fuera, .trash vuelta, symlink atrás
        let _ = std::fs::rename(&vdir, &staging);
        let trash_new = app_dir.join(TRASH);
        if let Some(oldv) = &old {
            let back = trash_new.join(format!("v{oldv}"));
            if back.is_dir() {
                let _ = std::fs::rename(back, app_dir.join(format!("v{oldv}")));
                let _ = swap_current(&app_dir, oldv);
            }
        }
        let _ = std::fs::remove_dir_all(&staging);
    } else {
        progress(InstallProgress {
            phase: Phase::Commit,
            fraction: 1.0,
        });
    }
    outcome
}

/// Swap atómico del symlink `current` (rename sobre el existente).
fn swap_current(app_dir: &Path, version: &Version) -> Res<()> {
    let link = app_dir.join(CURRENT);
    let tmp_link = app_dir.join(".current-new");
    let _ = std::fs::remove_file(&tmp_link);
    std::os::unix::fs::symlink(format!("v{version}"), &tmp_link)?;
    std::fs::rename(&tmp_link, &link)?;
    Ok(())
}

/// Registro en store (+ caps automáticas + audit).
#[allow(clippy::too_many_arguments)]
fn register(
    inst: &Installer,
    manifest: &Manifest,
    source: InstallSource,
    opts: &InstallOpts,
    old: Option<Version>,
    version: Version,
) -> Res<InstallOutcome> {
    let id = manifest.package.id.clone();
    let mut tx = inst.store.begin()?;
    inst.store.upsert_app(&mut tx, manifest, source)?;
    if let Some(caps) = &opts.auto_grant_caps {
        inst.store.grant_caps(&mut tx, &id, caps)?;
    }
    tx.commit()?;
    // audit FUERA de la tx: `Tx` retiene el MutexGuard de la conexión y
    // `audit()` lockea el mismo mutex (deadlock). Además es best-effort:
    // evidencia append-only, nunca bloquea la instalación.
    let _ = inst.store.audit(&AuditEvent {
        app_id: id.clone(),
        cap: arca_types::Capability::SystemStoreRead,
        ts: UnixMs::now(),
        detail: format!("install v{version} from {}", source.as_str()),
    });
    Ok(match old {
        Some(from) if from != version => InstallOutcome::Updated {
            app: id,
            from,
            to: version,
        },
        Some(_) | None => InstallOutcome::Installed { app: id, version },
    })
}

/// Desinstala: registro (tx pendiente) → borrar árbol → commit.
pub(crate) fn uninstall(inst: &Installer, id: &AppId) -> Res<()> {
    if inst.store.get_app(id)?.is_none() {
        return Err(ArcaError::NotFound(id.clone()));
    }
    let app_dir = inst.app_dir(id);
    let mut tx = inst.store.begin()?;
    inst.store.delete_app(&mut tx, id)?;
    std::fs::remove_dir_all(&app_dir)?;
    tx.commit()?;
    let _ = inst.store.audit(&AuditEvent {
        app_id: id.clone(),
        cap: arca_types::Capability::SystemStoreRead,
        ts: UnixMs::now(),
        detail: "uninstall".to_string(),
    });
    Ok(())
}

/// Rollback a `.trash` (intercambio atómico de versiones).
pub(crate) fn rollback(inst: &Installer, id: &AppId) -> Res<Version> {
    let app_dir = inst.app_dir(id);
    let trash = app_dir.join(TRASH);
    // versión previa disponible
    let prev_dir = {
        let mut found: Option<PathBuf> = None;
        if trash.is_dir() {
            for e in std::fs::read_dir(&trash)? {
                let e = e?;
                if e.path().is_dir() {
                    found = Some(e.path());
                    break;
                }
            }
        }
        found.ok_or(ArcaError::InvalidPackage(
            "rollback: sin versión previa en .trash",
        ))?
    };
    let prev_name = prev_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(ArcaError::Internal("rollback: nombre de versión ilegible"))?
        .trim_start_matches('v')
        .to_owned();
    let prev_version = Version::parse(&prev_name)
        .map_err(|_| ArcaError::Internal("rollback: versión previa no parsea"))?;

    // manifest de la versión previa (está en su dir)
    let prev_manifest_bytes = std::fs::read(prev_dir.join(MANIFEST_PATH))?;
    let prev_manifest = Manifest::parse(&prev_manifest_bytes)?;

    // swap: prev → activa; activa → .trash (nueva fuente de rollback)
    let tmp_rb = app_dir.join(format!("{TMP_PREFIX}rb-{:016x}", now_mono_ns()));
    std::fs::rename(&prev_dir, &tmp_rb)?;
    let cur = app_dir.join(CURRENT);
    if cur.is_symlink() {
        if let Ok(tgt) = std::fs::read_link(&cur) {
            let cur_vdir = app_dir.join(&tgt);
            std::fs::remove_dir_all(&trash).ok();
            std::fs::create_dir_all(&trash)?;
            let _ = std::fs::rename(&cur_vdir, trash.join(tgt));
        }
    }
    let final_dir = app_dir.join(format!("v{prev_version}"));
    std::fs::rename(&tmp_rb, &final_dir)?;
    swap_current(&app_dir, &prev_version)?;

    let mut tx = inst.store.begin()?;
    inst.store
        .upsert_app(&mut tx, &prev_manifest, InstallSource::User)?;
    tx.commit()?;
    let _ = inst.store.audit(&AuditEvent {
        app_id: id.clone(),
        cap: arca_types::Capability::SystemStoreRead,
        ts: UnixMs::now(),
        detail: format!("rollback v{prev_version}"),
    });
    Ok(prev_version)
}

/// Limpieza de arranque (spec 12 §4): `.tmp-*`, versiones colgantes y
/// symlink `current` roto. Devuelve el nº de reparaciones.
pub(crate) fn sweep(inst: &Installer) -> Res<usize> {
    let mut count = 0usize;
    let root = inst.root();
    if !root.is_dir() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let app_dir = entry.path();
        if !app_dir.is_dir() {
            continue;
        }
        let Some(id_raw) = app_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(id) = AppId::new(id_raw) else { continue };

        // 1. staging huérfano
        for e in std::fs::read_dir(&app_dir)? {
            let e = e?;
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(TMP_PREFIX) {
                std::fs::remove_dir_all(e.path()).ok();
                count += 1;
            }
        }

        // 2. versiones colgantes (no current, no .trash)
        let cur_target: Option<String> = {
            let link = app_dir.join(CURRENT);
            if link.is_symlink() {
                std::fs::read_link(&link)
                    .ok()
                    .and_then(|t| t.file_name()?.to_str().map(str::to_owned))
            } else {
                None
            }
        };
        let trash_target: Option<String> = {
            let t = app_dir.join(TRASH);
            if t.is_dir() {
                std::fs::read_dir(&t).ok().and_then(|mut it| {
                    it.next()
                        .and_then(|e| e.ok())
                        .and_then(|e| e.file_name().to_str().map(str::to_owned))
                })
            } else {
                None
            }
        };
        for e in std::fs::read_dir(&app_dir)? {
            let e = e?;
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let is_version_dir =
                name.starts_with('v') && name.len() > 1 && Version::parse(&name[1..]).is_ok();
            if is_version_dir
                && cur_target.as_deref() != Some(name.as_str())
                && trash_target.as_deref() != Some(name.as_str())
                && e.path().is_dir()
            {
                std::fs::remove_dir_all(e.path()).ok();
                count += 1;
            }
        }

        // 3. current roto/ausente con registro vivo → re-apuntar
        if let Some(rec) = inst.store.get_app(&id)? {
            let link = app_dir.join(CURRENT);
            let healthy = link.is_symlink()
                && std::fs::read_link(&link)
                    .ok()
                    .and_then(|t| t.file_name()?.to_str().map(str::to_owned))
                    .is_some_and(|t| app_dir.join(&t).is_dir());
            if !healthy {
                if let Ok(v) = Version::parse(&rec.version) {
                    let target = app_dir.join(format!("v{v}"));
                    if target.is_dir() {
                        swap_current(&app_dir, &v)?;
                        count += 1;
                    }
                }
            }
        }
    }
    Ok(count)
}

/// Re-sha del disco contra el manifest de la versión activa.
pub(crate) fn verify_installed(inst: &Installer, id: &AppId) -> Res<()> {
    let dir = inst.current_dir(id)?;
    let manifest_bytes = std::fs::read(dir.join(MANIFEST_PATH))?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    for a in manifest.artifacts.values() {
        let f = dir.join(a.path.as_str());
        if !f.is_file() {
            return Err(ArcaError::InvalidPackage(
                "verify_installed: artefacto ausente",
            ));
        }
        let got = sha256_file(&f)?;
        if got != a.sha256 {
            return Err(ArcaError::InvalidPackage(
                "verify_installed: sha no coincide",
            ));
        }
        if let Some(aot) = a.aot_path() {
            if !dir.join(aot.as_str()).is_file() {
                return Err(ArcaError::InvalidPackage("verify_installed: aot ausente"));
            }
        }
    }
    Ok(())
}

/// sha256 de un archivo por bloques (anti-tamper de verify_installed).
fn sha256_file(p: &Path) -> Res<[u8; 32]> {
    let mut h = sha2::Sha256::new();
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha2::Digest::update(&mut h, &buf[..n]);
    }
    Ok(h.finalize().into())
}

/// Acceso de tests al prepare (interrupción c/d).
#[cfg(test)]
pub(crate) fn prepare_for_test(inst: &Installer, src: PackageSource) -> Res<Prepared> {
    prepare(inst, src, &inst.ring, &mut |_| {})
}
