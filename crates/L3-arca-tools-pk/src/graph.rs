//! Análisis de imports → `meta/graph.mmd` (spec 25 §4).
//!
//! Grafo REAL de módulos del fuente: nodos = módulos Rust (por archivo),
//! aristas = `use crate::…` / `mod` / `super` entre ellos. Si el dev edita
//! el mmd a mano y miente → `check` falla (comparación con regenerado).

use std::collections::BTreeMap;
use std::path::Path;

use arca_types::{ArcaError, Res};
use walkdir::WalkDir;

/// Grafo de la app.
pub(crate) struct GrafoApp {
    /// módulo → archivo (orden alfabético por BTreeMap).
    pub(crate) mods: BTreeMap<String, String>,
    /// aristas (origen, destino).
    pub(crate) aristas: Vec<(String, String)>,
}

/// Analiza `src/` del proyecto.
pub(crate) fn analizar(src_rs: &Path) -> Res<GrafoApp> {
    let mut mods: BTreeMap<String, String> = BTreeMap::new();
    let mut aristas: Vec<(String, String)> = Vec::new();

    let mut archivos: Vec<(String, String)> = Vec::new(); // (modulo, path rel)
    for e in WalkDir::new(src_rs).into_iter().filter_map(|it| it.ok()) {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }
        let rel = p
            .strip_prefix(src_rs)
            .map_err(|_| ArcaError::Internal("graph: path fuera de src"))?;
        let modulo = modulo_de(rel);
        archivos.push((modulo, rel.to_string_lossy().into_owned()));
    }
    archivos.sort();

    for (modulo, archivo) in &archivos {
        mods.insert(modulo.clone(), archivo.clone());
        let contenido = std::fs::read_to_string(src_rs.join(archivo))?;
        for dep in imports_de(&contenido) {
            // arista solo si el destino existe como módulo (use crate::ui → "ui")
            if archivos.iter().any(|(m, _)| m == &dep) && &dep != modulo {
                aristas.push((modulo.clone(), dep));
            }
        }
    }
    aristas.sort();
    aristas.dedup();
    Ok(GrafoApp { mods, aristas })
}

/// Nombre de módulo desde path relativo (mod.rs/`main.rs` → padre).
fn modulo_de(rel: &Path) -> String {
    let mut comps: Vec<String> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(str::to_owned)
        .collect();
    let last = comps.pop().unwrap_or_default();
    let stem = last.trim_end_matches(".rs");
    if stem != "mod" && !stem.is_empty() && stem != "main" && stem != "lib" {
        comps.push(stem.to_owned());
    }
    comps.join(".")
}

/// Extrae destinos de `use crate::a::b` (→ "a"), `use super::*` (→ padre).
fn imports_de(codigo: &str) -> Vec<String> {
    let mut deps = Vec::new();
    for linea in codigo.lines() {
        let l = linea.trim_start();
        if let Some(resto) = l.strip_prefix("use ") {
            let ruta = resto.split(';').next().unwrap_or("").trim();
            if let Some(interna) = ruta.strip_prefix("crate::") {
                let raiz = interna.split("::").next().unwrap_or("");
                if !raiz.is_empty() {
                    deps.push(raiz.to_owned());
                }
            } else if ruta.starts_with("super::") || ruta.starts_with("crate") {
                // super::* desde un submódulo: padre
                deps.push(String::new());
            }
        }
    }
    deps
}

impl GrafoApp {
    /// Serializa a mermaid.
    pub(crate) fn a_mermaid(&self) -> String {
        let mut s = String::from("flowchart TD\n");
        for (nombre, archivo) in &self.mods {
            let label = if nombre.is_empty() {
                "(raíz)".to_owned()
            } else {
                nombre.clone()
            };
            s.push_str(&format!(
                "    m_{nombre}[\"{label}<br/><small>{archivo}</small>\"]\n"
            ));
        }
        for (a, b) in &self.aristas {
            s.push_str(&format!("    m_{a} --> m_{b}\n"));
        }
        s
    }
}

/// Comando `graph`: genera o comprueba `meta/graph.mmd` del proyecto.
pub(crate) fn cmd(src: &Path, check_only: bool) -> Res<()> {
    let grafo = analizar(&src.join("src"))?;
    let mmd = grafo.a_mermaid();
    let destino = src.join("meta/graph.mmd");
    if check_only {
        let actual = std::fs::read_to_string(&destino).map_err(|_| {
            ArcaError::InvalidPackage(
                "graph: meta/graph.mmd ausente (corre `arca-pk graph --src …` primero)",
            )
        })?;
        if normalizar(&actual) == normalizar(&mmd) {
            println!(
                "graph: en sincronía ({} módulos, {} aristas)",
                grafo.mods.len(),
                grafo.aristas.len()
            );
            Ok(())
        } else {
            Err(ArcaError::InvalidPackage(
                "graph out of sync: regenera con `arca-pk graph --src …`",
            ))
        }
    } else {
        if let Some(p) = destino.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&destino, &mmd)?;
        println!(
            "graph: {} generado ({} módulos, {} aristas)",
            destino.display(),
            grafo.mods.len(),
            grafo.aristas.len()
        );
        Ok(())
    }
}

/// Normalización para comparar ( espacios/CR ).
fn normalizar(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
