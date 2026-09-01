//! Listing de entradas del archivo `.arca`: el insumo de
//! [`crate::Manifest::validate_layout`].
//!
//! `arca-7z` (spec 09) construye este listing desde el 7z real y lo pasa al
//! modelo; los tests lo construyen a mano. El path se guarda TAL CUAL aparece
//! en el archivo (con normalización menor de `/` finales) porque la validación
//! de sintaxis la hace [`crate::Manifest::validate_layout`], no este tipo.

/// Tipo de entrada del archivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// Archivo regular.
    File,
    /// Directorio.
    Dir,
    /// Symlink: prohibido por el layout (docs/07 §9, hardening F6).
    Symlink,
}

/// Una entrada del archivo: path crudo + tipo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    path: String,
    kind: EntryKind,
}

impl ArchiveEntry {
    /// Construye una entrada. Normaliza `/` finales: `bin/` queda como
    /// directorio `bin` (un slash final en un File se interpreta como Dir).
    pub fn new(path: impl Into<String>, kind: EntryKind) -> Self {
        let raw: String = path.into();
        let trimmed: &str = raw.trim_end_matches('/');
        let kind = if trimmed.len() != raw.len() && kind == EntryKind::File {
            EntryKind::Dir
        } else {
            kind
        };
        Self {
            path: trimmed.to_owned(),
            kind,
        }
    }

    /// Path normalizado (sin `/` final).
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Tipo de entrada.
    #[must_use]
    pub const fn kind(&self) -> EntryKind {
        self.kind
    }
}

/// Colección de entradas del `.arca` (listado del 7z).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveEntries {
    entries: Vec<ArchiveEntry>,
}

impl ArchiveEntries {
    /// Colección vacía.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Desde paths, todos como archivos regulares.
    #[must_use]
    pub fn from_paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            entries: paths
                .into_iter()
                .map(|p| ArchiveEntry::new(p, EntryKind::File))
                .collect(),
        }
    }

    /// Desde entradas ya construidas.
    #[must_use]
    pub fn from_entries<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = ArchiveEntry>,
    {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Añade una entrada (encadenable: `e.push(a, k).push(b, k);`).
    pub fn push(&mut self, path: impl Into<String>, kind: EntryKind) -> &mut Self {
        self.entries.push(ArchiveEntry::new(path, kind));
        self
    }

    /// Itera las entradas en orden de archivo.
    pub fn iter(&self) -> impl Iterator<Item = &ArchiveEntry> {
        self.entries.iter()
    }

    /// Número de entradas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// ¿Sin entradas?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// ¿Existe un ARCHIVO con ese path exacto (normalizado)?
    #[must_use]
    pub fn has_file(&self, path: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == EntryKind::File && e.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_final_se_normaliza_a_dir() {
        let e = ArchiveEntry::new("bin/", EntryKind::File);
        assert_eq!(e.path(), "bin");
        assert_eq!(e.kind(), EntryKind::Dir);
        let e = ArchiveEntry::new("bin", EntryKind::Dir);
        assert_eq!(e.path(), "bin");
        assert_eq!(e.kind(), EntryKind::Dir);
        // El slash final no tapa un symlink (el rechazo es por tipo).
        let e = ArchiveEntry::new("bin/lnk", EntryKind::Symlink);
        assert_eq!(e.kind(), EntryKind::Symlink);
    }

    #[test]
    fn push_encadena_y_has_file() {
        let mut e = ArchiveEntries::new();
        e.push("manifest.toml", EntryKind::File)
            .push("bin/", EntryKind::Dir)
            .push("bin/app", EntryKind::File);
        assert_eq!(e.len(), 3);
        assert!(!e.is_empty());
        assert!(e.has_file("manifest.toml"));
        assert!(e.has_file("bin/app"));
        // «bin/» quedó normalizado a Dir ⇒ no es archivo:
        assert!(!e.has_file("bin"));
        assert!(!e.has_file("no-existe"));
    }

    #[test]
    fn from_paths_y_from_entries() {
        let a = ArchiveEntries::from_paths(["a/b", "c"]);
        assert_eq!(a.len(), 2);
        assert!(a.has_file("a/b"));
        let b = ArchiveEntries::from_entries(vec![
            ArchiveEntry::new("x", EntryKind::File),
            ArchiveEntry::new("y/", EntryKind::File),
        ]);
        assert!(b.has_file("x"));
        assert!(!b.has_file("y/"));
    }
}
