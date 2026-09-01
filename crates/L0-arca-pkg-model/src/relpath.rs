//! [`RelPath`]: path relativo saneado (newtype con invariantes).
//!
//! Es la PRIMERA barrera contra el path-traversal (spec 02 §3): todo path que
//! cruza el modelo del paquete (artefactos, fuentes, entradas del archivo)
//! pasa por aquí. La SEGUNDA barrera vive en `arca-7z` (`sanitize_entry_path`,
//! spec 09) y la tercera es el extractor (permisos 0700/0600 + sin symlinks).
//!
//! Invariantes garantizadas por construcción:
//! - relativo (nunca empieza por `/`), UTF-8 (es `&str`), sin `\`;
//! - sin componentes `.`/`..`/vacíos (canonicaliza `a//b`, `a/`, `./a`);
//! - sin caracteres de control (NUL incluido) ni `:` (drives de Windows y
//!   alternate data streams);
//! - profundidad ≤ [`MAX_DEPTH`], componente ≤ [`MAX_COMPONENT_BYTES`],
//!   longitud total ≤ [`MAX_PATH_BYTES`].
//!
//! El "symlink-escape en componentes" NO es detectable desde un string: se
//! defiende (a) rechazando entradas marcadas como symlink en
//! [`crate::Manifest::validate_layout`] y (b) en el extractor de `arca-7z`.

use crate::error::PkgError;

/// Longitud total máxima de un path del paquete (bytes UTF-8).
pub const MAX_PATH_BYTES: usize = 1024;
/// Profundidad máxima (número de componentes). Alineado con spec 09.
pub const MAX_DEPTH: usize = 16;
/// Longitud máxima por componente (bytes). Alineado con spec 09.
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Path relativo saneado. Ver invariantes en el módulo.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    /// Valida y construye. Total: cualquier input → [`PkgError::BadPath`].
    pub fn new(s: &str) -> Result<Self, PkgError> {
        if s.is_empty() {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "vacío",
            });
        }
        if s.len() > MAX_PATH_BYTES {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "excede 1024 bytes",
            });
        }
        if s.contains('\\') {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "contiene backslash",
            });
        }
        if s.chars().any(char::is_control) {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "contiene caracteres de control",
            });
        }
        if s.contains(':') {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "contiene ':' (drive de Windows / ADS)",
            });
        }
        if s.starts_with('/') {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "es absoluto",
            });
        }
        let comps: Vec<&str> = s.split('/').collect();
        for comp in &comps {
            if comp.is_empty() {
                return Err(PkgError::BadPath {
                    path: s.to_owned(),
                    reason: "componente vacío ('//', '/' final o inicial)",
                });
            }
            if *comp == "." {
                return Err(PkgError::BadPath {
                    path: s.to_owned(),
                    reason: "componente '.'",
                });
            }
            if *comp == ".." {
                return Err(PkgError::BadPath {
                    path: s.to_owned(),
                    reason: "componente '..' (path traversal)",
                });
            }
            if comp.len() > MAX_COMPONENT_BYTES {
                return Err(PkgError::BadPath {
                    path: s.to_owned(),
                    reason: "componente > 255 bytes",
                });
            }
        }
        if comps.len() > MAX_DEPTH {
            return Err(PkgError::BadPath {
                path: s.to_owned(),
                reason: "profundidad > 16",
            });
        }
        Ok(Self(s.to_owned()))
    }

    /// String interna (saneada).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Componentes del path (`bin/wasm/app.wasm` → `bin`, `wasm`, `app.wasm`).
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// Profundidad (número de componentes).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.0.matches('/').count() + 1
    }

    /// ¿Está el path bajo el directorio `dir` (o es exactamente `dir`)?
    /// `dir` se da SIN slash final (`"bin"`, `"bin/wasm"`).
    #[must_use]
    pub fn is_under(&self, dir: &str) -> bool {
        match self.0.strip_prefix(dir) {
            Some(rest) => rest.is_empty() || rest.starts_with('/'),
            None => false,
        }
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelPath({:?})", self.0)
    }
}

impl serde::Serialize for RelPath {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for RelPath {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALIDOS: &[&str] = &[
        "a",
        "manifest.toml",
        "bin/wasm/app.wasm",
        "bin/native-aarch64/app",
        "assets/fonts/inter.ttf",
        "meta/graph.mmd",
        "assets/fonts/NotoSans-ÉBold.otf",
        "icons/icon-192.png",
        // 16 componentes exactos, cortos:
        "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p",
    ];

    #[test]
    fn tabla_validos() {
        for s in VALIDOS {
            let p = RelPath::new(s);
            assert!(p.is_ok(), "debía ser válido: {s:?}");
        }
    }

    #[test]
    fn tabla_invalidos() {
        let mut invalidos: Vec<String> = [
            "".to_owned(),
            "/abs".to_owned(),
            "//x".to_owned(),
            "a/".to_owned(),
            "a//b".to_owned(),
            "./a".to_owned(),
            "../a".to_owned(),
            "a/../b".to_owned(),
            "a/./b".to_owned(),
            "..".to_owned(),
            ".".to_owned(),
            "a\\b".to_owned(),
            "C:x".to_owned(),
            "c:/x".to_owned(),
            "a:b".to_owned(),
            "a\u{0}b".to_owned(),
            "a\u{7}b".to_owned(),
            "bin/\u{7f}".to_owned(),
            // 17 componentes:
            "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q".to_owned(),
        ]
        .into_iter()
        .collect();
        // componente > 255 bytes:
        invalidos.push(format!("bin/{}", "x".repeat(256)));
        // longitud total > 1024 (componentes cortos, muchos niveles no: usar 4 grandes):
        invalidos.push(format!(
            "{}/{}/{}/{}",
            "y".repeat(300),
            "y".repeat(300),
            "y".repeat(300),
            "y".repeat(300)
        ));
        for s in &invalidos {
            assert!(RelPath::new(s).is_err(), "debía ser inválido: {s:?}");
        }
    }

    #[test]
    fn is_under_exacto_o_prefijo_de_directorio() {
        let p = RelPath::new("bin/wasm/app.wasm").expect("válido");
        assert!(p.is_under("bin"));
        assert!(p.is_under("bin/wasm"));
        assert!(!p.is_under("bin/native-aarch64"));
        assert!(!p.is_under("binary"));
        let raiz = RelPath::new("bin").expect("válido");
        assert!(raiz.is_under("bin"));
        assert!(!raiz.is_under("bi"));
    }

    #[test]
    fn depth_y_components() {
        let p = RelPath::new("bin/wasm/app.wasm").expect("válido");
        assert_eq!(p.depth(), 3);
        assert_eq!(
            p.components().collect::<Vec<_>>(),
            ["bin", "wasm", "app.wasm"]
        );
        let p = RelPath::new("a").expect("válido");
        assert_eq!(p.depth(), 1);
    }

    #[test]
    fn display_y_debug() {
        let p = RelPath::new("meta/graph.mmd").expect("válido");
        assert_eq!(p.to_string(), "meta/graph.mmd");
        assert!(format!("{p:?}").contains("RelPath"));
    }
}
