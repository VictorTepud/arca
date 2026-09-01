//! Rutas relativas saneadas y [`sanitize_entry_path`] — la **segunda barrera**
//! de seguridad del contenedor (spec 09 §3).
//!
//! Un paquete `.arca` es un 7z normal: los nombres de entrada los escribe el
//! empaquetador y NO se pueden dar por buenos. Aunque `arca-pkg-model` valida
//! el manifest y el layout antes (primera barrera), este módulo vuelve a
//! validar cada path **antes de abrir el archivo destino** (defense-in-depth,
//! spec 09 §4).
//!
//! Regla corta: solo se aceptan rutas **UTF-8 relativas**, con componentes
//! alfanuméricos razonables, sin `..`, sin absolutas, sin `\`, sin NUL ni
//! caracteres de control, profundidad ≤ 16 y ≤ 255 bytes por componente.

use std::path::Path;

/// Profundidad máxima de una ruta del paquete (spec 09 §3).
pub const MAX_DEPTH: usize = 16;

/// Longitud máxima en bytes de cada componente (spec 09 §3).
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Longitud máxima total en bytes de la ruta saneada.
///
/// NOTA(agent): la spec fija depth y componente, pero no total. Un nombre de
/// megabytes es un vector de DoS de memoria trivial, así que se acota a 4096
/// (PATH_MAX de Linux como cota conservadora).
pub const MAX_TOTAL_BYTES: usize = 4096;

/// Ruta relativa **ya saneada** dentro de un paquete.
///
/// Solo se puede construir a través de [`sanitize_entry_path`]: el tipo
/// garantiza por construcción que es relativa, UTF-8, sin `..`, sin `\`,
/// sin NUL/controles, con depth ≤ [`MAX_DEPTH`] y componentes ≤
/// [`MAX_COMPONENT_BYTES`].
///
/// NOTA(agent): la spec 09 prevé que `RelPath` viva en `arca-pkg-model`
/// ("a discusión del arquitecto si prefiere invertir"). T04 corre en paralelo
/// y este crate no puede depender de él, así que el tipo vive aquí; cuando
/// pkg-model aterrice, el arquitecto decide si se re-exporta o se migra a
/// `arca-types`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    /// Construye desde un String **ya validado** (solo uso interno del crate).
    fn from_sanitized(s: String) -> Self {
        Self(s)
    }

    /// La ruta como `&str` (idéntica a la representación canónica saneada).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// La ruta como [`Path`] relativo (para joins con el directorio destino).
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }

    /// Número de componentes (`a/b/c` → 3).
    pub fn depth(&self) -> usize {
        self.0.split('/').count()
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// ¿Es un componente de "unidad de Windows" (`C:`, `D:x`)? Rechazo por
/// defensa en profundidad (el objetivo es Android, pero un `C:` colaría como
/// ruta relativa en Linux y es señal clara de path absoluto mal serializado).
fn is_windows_drive(comp: &str) -> bool {
    let b = comp.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Sanea el nombre crudo de una entrada de archivo 7z (spec 09 §3).
///
/// Devuelve [`Some`] solo si la ruta es una **ruta relativa UTF-8 segura**:
///
/// - No vacía, sin componentes vacíos (`//`, `/` inicial/final).
/// - Sin `..` en ningún componente (path traversal).
/// - Sin `\` (separador Windows: `a\..\b` debe rechazarse en Linux).
/// - Sin NUL y sin ningún carácter de control ASCII (0x00–0x1F, 0x7F:
///   saltos de línea, tabs…).
/// - Sin letra de unidad estilo Windows (`C:...`).
/// - Componentes `.` se eliminan (normalización: `a/./b` → `a/b`);
///   si no queda nada (`.`), se rechaza.
/// - depth ≤ [`MAX_DEPTH`], componente ≤ [`MAX_COMPONENT_BYTES`] bytes,
///   total ≤ [`MAX_TOTAL_BYTES`] bytes.
///
/// El 7z original ya limita algunas cosas (nombres UTF-16 válidos, NUL como
/// separador de nombres); este filtro es deliberadamente independiente de
/// eso: defensa en profundidad.
pub fn sanitize_entry_path(raw: &str) -> Option<RelPath> {
    // Longitud total temprana (DoS de memoria): barato y acota todo lo demás.
    if raw.is_empty() || raw.len() > MAX_TOTAL_BYTES {
        return None;
    }
    // NUL, '\', y cualquier control ASCII (incluye saltos CR/LF).
    if raw
        .as_bytes()
        .iter()
        .any(|&b| b < 0x20 || b == 0x7F || b == b'\\')
    {
        return None;
    }

    let mut comps: Vec<&str> = Vec::new();
    for comp in raw.split('/') {
        match comp {
            "" => return None,   // ruta absoluta o componente vacío
            "." => continue,     // normalización
            ".." => return None, // traversal
            c if is_windows_drive(c) => return None,
            c => {
                if c.len() > MAX_COMPONENT_BYTES {
                    return None;
                }
                comps.push(c);
            }
        }
    }
    if comps.is_empty() || comps.len() > MAX_DEPTH {
        return None;
    }
    Some(RelPath::from_sanitized(comps.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &[(&str, &str)] = &[
        ("a", "a"),
        ("a/b.txt", "a/b.txt"),
        ("bin/native-aarch64/app", "bin/native-aarch64/app"),
        ("a/./b", "a/b"), // '.' se normaliza
        ("./a", "a"),
        (
            "assets/fonts/Inter-Regular.otf",
            "assets/fonts/Inter-Regular.otf",
        ),
        ("ñandú/水.txt", "ñandú/水.txt"), // UTF-8 no ASCII: válido
        ("a/.../b", "a/.../b"),           // '...' es un nombre legal
        ("x.tar.gz", "x.tar.gz"),
    ];

    const BAD: &[&str] = &[
        "",            // vacía
        ".",           // solo '.'
        "..",          // traversal raíz
        "../evil",     // traversal clásico
        "/abs",        // absoluta
        "/etc/passwd", // absoluta con contenido
        "a/../../b",   // traversal intermedio
        "a/..",        // traversal final
        "a\\..\\b",    // '\' (separador Windows)
        "back\\slash", // '\' simple
        "C:\\x",       // unidad Windows
        "C:",          // unidad Windows sola
        "c:algo",      // unidad Windows minúscula
        "a//b",        // componente vacío
        "a/",          // '/' final
        "//evil",      // UNC-ish
        "a\x00b",      // NUL
        "a\nb",        // salto de línea
        "a\rb",        // CR
        "a\tb",        // tab
        "a\x7Fb",      // DEL
    ];

    fn deep(n: usize) -> String {
        (0..n)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn acepta_rutas_validas() {
        for (raw, want) in OK {
            let got = sanitize_entry_path(raw).unwrap_or_else(|| panic!("debería aceptar {raw:?}"));
            assert_eq!(got.as_str(), *want, "{raw:?}");
        }
    }

    #[test]
    fn rechaza_todas_las_maliciosas() {
        let mut all: Vec<String> = BAD.iter().map(|s| s.to_string()).collect();
        all.push(deep(17)); // depth 17 > MAX_DEPTH
        all.push("x".repeat(256)); // componente de 256 bytes
        all.push(format!("a/{}", "y".repeat(256)));
        all.push("z".repeat(MAX_TOTAL_BYTES + 1)); // total > 4096
        for raw in &all {
            assert!(
                sanitize_entry_path(raw).is_none(),
                "debería RECHAZAR {:?}",
                raw
            );
        }
    }

    #[test]
    fn limites_exactos() {
        // depth 16 justo: válido; 17: inválido.
        assert!(sanitize_entry_path(&deep(MAX_DEPTH)).is_some());
        assert!(sanitize_entry_path(&deep(MAX_DEPTH + 1)).is_none());
        // componente de 255: válido; 256: inválido.
        let c255 = "x".repeat(MAX_COMPONENT_BYTES);
        assert!(sanitize_entry_path(&c255).is_some());
        assert!(sanitize_entry_path(&"y".repeat(MAX_COMPONENT_BYTES + 1)).is_none());
        // total máximo alcanzable con los otros límites: 16×255 + 15 '/'
        // = 4095 bytes (el tope global de 4096 nunca dispara solo).
        let t: String = (0..MAX_DEPTH)
            .map(|_| "z".repeat(MAX_COMPONENT_BYTES))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(t.len(), 4095);
        assert!(sanitize_entry_path(&t).is_some());
    }

    #[test]
    fn metadatos_de_relpath() {
        let p = sanitize_entry_path("a/b/c.txt").unwrap();
        assert_eq!(p.depth(), 3);
        assert_eq!(p.as_path().components().count(), 3);
        assert_eq!(p.to_string(), "a/b/c.txt");
        // Dos saneados iguales son iguales (HashSet-friendly).
        let q = sanitize_entry_path("a/./b/./c.txt").unwrap();
        assert_eq!(p, q);
    }
}
