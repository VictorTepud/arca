//! Validación del layout interno del `.arca` (docs/06 §2, spec 02 §3).
//!
//! Cruza el manifest con el listing del archivo ([`ArchiveEntries`]):
//! rutas permitidas exactas, sin extras, sin faltantes. Es la primera
//! barrera contra path-traversal sobre ENTRADAS (la segunda la aplica
//! `arca-7z` sobre cada path al extraer).
//!
//! Reglas (v1):
//! 1. Ninguna entrada puede ser symlink (docs/07 §9).
//! 2. Todo path pasa el saneo de [`RelPath`] (sin `..`, abs, `\`, …).
//! 3. Todo path cae bajo [`crate::LAYOUT`] (`manifest.toml`, `bin/`,
//!    `assets/`, `icons/`, `meta/`) — nada más en la raíz.
//! 4. Sin paths duplicados (tras normalizar `/` finales).
//! 5. `manifest.toml` existe exactamente una vez, como archivo.
//! 6. Todo path DECLARADO en el manifest (artefactos, `aot`, fuentes) existe
//!    como archivo (sin faltantes).
//! 7. Todo archivo bajo `bin/` está declarado: cada binario extraíble queda
//!    pineado con sha256 (nada ejecutable "de propina").

use std::collections::HashSet;

use arca_types::{ArcaError, Res};

use crate::entries::{ArchiveEntries, EntryKind};
use crate::error::PkgError;
use crate::manifest::Manifest;
use crate::relpath::RelPath;

/// ¿Cae el path dentro del layout permitido ([`crate::LAYOUT`])?
#[must_use]
pub fn is_allowed_path(p: &RelPath) -> bool {
    p.as_str() == "manifest.toml"
        || p.is_under("bin")
        || p.is_under("assets")
        || p.is_under("icons")
        || p.is_under("meta")
}

impl Manifest {
    /// Valida el layout del archivo contra este manifest (spec 02 §3).
    /// Devuelve [`ArcaError`] canónico.
    pub fn validate_layout(&self, entries: &ArchiveEntries) -> Res<()> {
        self.validate_layout_detailed(entries)
            .map_err(ArcaError::from)
    }

    /// Ídem [`Manifest::validate_layout`] con diagnóstico fino [`PkgError`].
    ///
    /// NOTA(agent): docs/06 pide además que `manifest.toml` sea "primero en
    /// el orden de streams". Un listing de paths no garantiza el orden de
    /// streams del 7z (header ≠ stream), así que NO se valida aquí: lo
    /// garantiza `arca-tools-pk` al empaquetar. Desvío documentado.
    pub fn validate_layout_detailed(&self, entries: &ArchiveEntries) -> Result<(), PkgError> {
        let mut seen: HashSet<RelPath> = HashSet::new();
        let mut files: HashSet<RelPath> = HashSet::new();
        let mut has_manifest = false;

        for entry in entries.iter() {
            let raw = entry.path();
            // Regla 1: symlinks prohibidos (antes que cualquier otra cosa).
            if entry.kind() == EntryKind::Symlink {
                return Err(PkgError::LayoutSymlink {
                    path: raw.to_owned(),
                });
            }
            // Un `/` final marca directorio; se normaliza antes de validar.
            let trimmed: &str = raw.trim_end_matches('/');
            let rel = match RelPath::new(trimmed) {
                Ok(r) => r,
                Err(PkgError::BadPath { reason, .. }) => {
                    return Err(PkgError::LayoutBadPath {
                        path: raw.to_owned(),
                        reason,
                    });
                }
                // RelPath::new solo produce BadPath; brazo defensivo total.
                Err(e) => return Err(e),
            };
            // Regla 3: dentro del layout permitido.
            if !is_allowed_path(&rel) {
                return Err(PkgError::LayoutExtra {
                    path: raw.to_owned(),
                });
            }
            // Regla 4: sin duplicados (normalizado).
            if !seen.insert(rel.clone()) {
                return Err(PkgError::LayoutDuplicate {
                    path: raw.to_owned(),
                });
            }
            let is_dir = entry.kind() == EntryKind::Dir || trimmed.len() != raw.len();
            if !is_dir {
                if rel.as_str() == "manifest.toml" {
                    has_manifest = true;
                }
                files.insert(rel);
            }
        }

        // Regla 5: manifest.toml como archivo.
        if !has_manifest {
            return Err(PkgError::LayoutNoManifest);
        }

        // Regla 6: declarados presentes (artefactos, aot, fuentes).
        for art in self.artifacts.values() {
            if !files.contains(&art.path) {
                return Err(PkgError::LayoutMissing {
                    path: art.path.to_string(),
                });
            }
            if let Some(aot) = art.aot_path() {
                if !files.contains(&aot) {
                    return Err(PkgError::LayoutMissing {
                        path: aot.to_string(),
                    });
                }
            }
        }
        for font in &self.runtime.ui.fonts {
            if !files.contains(font) {
                return Err(PkgError::LayoutMissing {
                    path: font.to_string(),
                });
            }
        }

        // Regla 7: nada ejecutable sin declarar bajo bin/ (incluye los
        // AOT: también son binarios extraíbles).
        let mut declared: HashSet<RelPath> = HashSet::new();
        for art in self.artifacts.values() {
            declared.insert(art.path.clone());
            if let Some(aot) = art.aot_path() {
                declared.insert(aot);
            }
        }
        for f in &files {
            if f.is_under("bin") && !declared.contains(f) {
                return Err(PkgError::LayoutUndeclaredBin {
                    path: f.to_string(),
                });
            }
        }
        Ok(())
    }
}
