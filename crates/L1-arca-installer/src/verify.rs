//! `verify_installed`: re-hash del disco contra el manifest (anti-tamper,
//! spec 12 §3: panel de diagnóstico y spot-checks).

use std::fs;
use std::io::Read;
use std::path::Path;

use arca_pkg_model::{ArchiveEntries, EntryKind};
use arca_types::{ArcaError, Res};
use sha2::{Digest as ShaDigest, Sha256};

use crate::{parse_version, version_dir_name, Installer};

/// Buffer de re-hash streaming (nunca se carga el artefacto en RAM).
const HASH_BUF: usize = 64 * 1024;

impl Installer {
    /// Re-hashea los artefactos instalados y re-valida el layout del árbol
    /// (post-install, post-update o sospecha de tamper).
    ///
    /// Comprueba:
    /// 1. Que exista registro en el store y que `v<version>/` esté en disco.
    /// 2. Que el `manifest.toml` del árbol parsea y su versión coincide con
    ///    la registrada.
    /// 3. El layout del árbol contra el manifest (extras/faltantes) — incluye
    ///    detección de **symlinks plantados en disco** (rechazo duro).
    /// 4. El sha256 de cada artefacto declarado, streaming (64 KiB buffer).
    ///
    /// # Errors
    /// - [`ArcaError::NotFound`]: la app no está registrada.
    /// - [`ArcaError::InvalidPackage`]: cualquier discrepancia disco↔manifest.
    /// - [`ArcaError::Io`]: árbol ilegible o ausente.
    pub fn verify_installed(&self, id: &arca_types::AppId) -> Res<()> {
        let Some(rec) = self.store.get_app(id)? else {
            return Err(ArcaError::NotFound(id.clone()));
        };
        let dir = self
            .app_dir(id)
            .join(version_dir_name(&parse_version(&rec.version)?));
        if !dir.is_dir() {
            tracing::error!(
                target: "arca::arca-installer::verify",
                dir = %dir.display(),
                "árbol de la versión activa ausente"
            );
            return Err(ArcaError::InvalidPackage(
                "installer: árbol de la app ausente",
            ));
        }

        let manifest_bytes = fs::read(dir.join(arca_sign::MANIFEST_PATH)).map_err(ArcaError::Io)?;
        let manifest = crate::Manifest::parse(&manifest_bytes)?;
        if manifest.package.version.to_string() != rec.version {
            tracing::error!(
                target: "arca::arca-installer::verify",
                store = %rec.version,
                disk = %manifest.package.version,
                "el manifest del árbol no es la versión registrada"
            );
            return Err(ArcaError::InvalidPackage(
                "installer: manifest del árbol ≠ versión registrada",
            ));
        }

        // Layout del árbol (extra/faltante/symlink plantado).
        let entries = walk_tree(&dir)?;
        manifest.validate_layout(&entries)?;

        // sha256 de cada artefacto declarado (streaming).
        for art in manifest.artifacts.values() {
            // Puente RelPath (pkg-model) → Path: por as_str, como siempre.
            let file = dir.join(art.path.as_str());
            let got = sha256_file(&file)?;
            if got != art.sha256 {
                tracing::error!(
                    target: "arca::arca-installer::verify",
                    path = %art.path.as_str(),
                    declared = %art.sha256_hex(),
                    real = %hex::encode(got),
                    "sha256 del artefacto instalado no coincide"
                );
                return Err(ArcaError::InvalidPackage(
                    "installer: sha256 del artefacto instalado no coincide",
                ));
            }
        }
        Ok(())
    }
}

/// sha256 de un archivo, streaming (memoria O(1)).
fn sha256_file(p: &Path) -> Res<[u8; 32]> {
    let mut f = fs::File::open(p).map_err(ArcaError::Io)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; HASH_BUF];
    loop {
        let n = f.read(&mut buf).map_err(ArcaError::Io)?;
        if n == 0 {
            break;
        }
        ShaDigest::update(&mut h, &buf[..n]);
    }
    Ok(h.finalize().into())
}

/// Camina el árbol instalado y construye el listing para
/// `validate_layout` (relativos a `root`, sin symlinks: un symlink en el
/// árbol instalado es tamper → error duro).
fn walk_tree(root: &Path) -> Res<ArchiveEntries> {
    let mut out = ArchiveEntries::new();
    walk_rec(root, "", &mut out)?;
    Ok(out)
}

/// Recursión de [`walk_tree`] con prefijo relativo acumulado.
fn walk_rec(dir: &Path, prefix: &str, out: &mut ArchiveEntries) -> Res<()> {
    for entry in fs::read_dir(dir).map_err(ArcaError::Io)? {
        let entry = entry.map_err(ArcaError::Io)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let ft = entry.file_type().map_err(ArcaError::Io)?;
        if ft.is_symlink() {
            tracing::error!(
                target: "arca::arca-installer::verify",
                path = %rel,
                "symlink en el árbol instalado (tamper)"
            );
            return Err(ArcaError::InvalidPackage(
                "installer: symlink en el árbol instalado",
            ));
        }
        if ft.is_dir() {
            out.push(&rel, EntryKind::Dir);
            walk_rec(&entry.path(), &rel, out)?;
        } else {
            out.push(&rel, EntryKind::File);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_tree_estructura() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("v1.0.0");
        std::fs::create_dir_all(root.join("bin/native-aarch64")).unwrap();
        std::fs::create_dir_all(root.join("meta")).unwrap();
        std::fs::write(root.join("manifest.toml"), b"m").unwrap();
        std::fs::write(root.join("bin/native-aarch64/app"), b"a").unwrap();
        std::fs::write(root.join("meta/graph.mmd"), b"g").unwrap();
        let entries = walk_tree(&root).unwrap();
        assert!(entries.has_file("manifest.toml"));
        assert!(entries.has_file("bin/native-aarch64/app"));
        assert!(entries.has_file("meta/graph.mmd"));
        assert!(!entries.has_file("bin"));
    }

    #[test]
    fn walk_tree_rechaza_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("v1.0.0");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("bin/app")).unwrap();
        let err = walk_tree(&root).unwrap_err();
        assert!(matches!(err, ArcaError::InvalidPackage(_)));
    }

    #[test]
    fn sha256_streaming_igual_a_directo() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.bin");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let a = sha256_file(&p).unwrap();
        let mut h = Sha256::new();
        ShaDigest::update(&mut h, &data);
        let b: [u8; 32] = h.finalize().into();
        assert_eq!(a, b);
    }
}
