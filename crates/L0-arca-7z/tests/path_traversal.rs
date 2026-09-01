//! Path traversal (spec 09 §6): el archivo `malicious.7z` del corpus trae
//! entradas maliciosas REALES escritas por py7zr (`../evil.txt`,
//! `a/../../b.txt` — py7zr normaliza `\` y las rutas absolutas al escribir,
//! pero respeta los `..` verbatim). Para cubrir el resto de patrones
//! canónicos (`/abs`, `C:\x`, `back\slash`, `..`) se genera además un 7z
//! con nombres VERBATIM usando el writer de `sevenz-rust2` (que no
//! sanea nada) — así la 2ª barrera se prueba contra todos ellos.

mod common;

use std::fs;
use std::io::Read;
use std::path::Path;

use arca_7z::{sanitize_entry_path, Archive, DirSink, EntrySink, ExtractPlan};
use arca_types::ArcaError;
use common::{corpus_dir, walk_files};

/// Los que py7zr escribe VERBATIM (el resto los normaliza al escribir).
const MALOS_PY7ZR: &[&str] = &["../evil.txt", "a/../../b.txt"];

/// Todos los patrones canónicos (spec 09 §6) + variantes.
const MALOS_VERBATIM: &[&str] = &[
    "../evil.txt",
    "/abs.txt",
    "C:\\x",
    "back\\slash.txt",
    "a/../../b.txt",
    "..",
];

#[test]
fn sanitize_rechaza_todos_los_patrones_canonicos() {
    for malo in MALOS_VERBATIM {
        assert!(sanitize_entry_path(malo).is_none(), "rechazar {malo:?}");
    }
    // NUL y saltos (py7zr los trunca/normaliza al escribir: pruebo a nivel
    // de función).
    assert!(sanitize_entry_path("nul\0byte.txt").is_none());
    assert!(sanitize_entry_path("salto\nlinea.txt").is_none());
    assert!(sanitize_entry_path("a\r\nb").is_none());
    // Y lo legítimo pasa.
    assert!(sanitize_entry_path("assets/fonts/inter.ttf").is_some());
}

#[test]
fn archivo_py7zr_malicioso_se_aborta_sin_escribir_nada() {
    let path = corpus_dir().join("malicious.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open (cabecera válida)");

    let entries = archive.entries().expect("entries");
    for m in MALOS_PY7ZR {
        let e = entries
            .iter()
            .find(|e| e.path == *m)
            .unwrap_or_else(|| panic!("falta entrada maliciosa {m}"));
        assert!(e.safe_path().is_none(), "{m} debía ser rechazado");
    }
    // La entrada válida sí sanea (misma mezcla de bueno y malo).
    assert!(entries.iter().any(|e| e.safe_path().is_some()));

    // Extracción: fail-fast, cero bytes escritos, nada fuera del sandbox.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
    assert!(res.is_err(), "la extracción debía abortar");
    assert!(
        !root.exists() || walk_files(&root).is_empty(),
        "nada en disco"
    );
    assert_eq!(walk_files(tmp.path()).len(), 0, "escape del sandbox");

    // Plan selectivo de la entrada inocente TAMBIÉN aborta (el pre-escaneo
    // es global, no solo de lo pedido).
    let mut sink2 = DirSink::new(tmp.path().join("out2"));
    let mut progreso2 = |_f: f64| {};
    let plan = ExtractPlan::parse(&["innocent.txt"]).expect("plan");
    assert!(archive.extract(&plan, &mut sink2, &mut progreso2).is_err());
}

/// Crea un 7z con nombres VERBATIM usando el writer de sevenz-rust2
/// (no sanea nada: ideal para probar la 2ª barrera).
fn crear_7z_verbatim(path: &Path, names: &[&str]) {
    let mut writer = sevenz_rust2::ArchiveWriter::create(path).expect("crear writer");
    let data: &[u8] = b"contenido del mal";
    for n in names {
        let entry = sevenz_rust2::ArchiveEntry::new_file(n);
        writer
            .push_archive_entry(entry, Some(data))
            .expect("push entrada");
    }
    writer.finish().expect("finish");
}

#[test]
fn archivo_verbatim_malicioso_se_aborta_entero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mal = tmp.path().join("mal.7z");
    crear_7z_verbatim(&mal, MALOS_VERBATIM);

    let mut file = fs::File::open(&mal).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let entries = archive.entries().expect("entries");
    assert_eq!(entries.len(), MALOS_VERBATIM.len());
    for e in &entries {
        assert!(e.safe_path().is_none(), "{} debía ser rechazado", e.path);
    }

    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
    assert!(res.is_err(), "extracción debía abortar");
    assert!(!root.exists() || walk_files(&root).is_empty());
    // Nada fuera del sink.
    let fuera: Vec<_> = walk_files(tmp.path())
        .into_iter()
        .filter(|p| !p.starts_with(&mal))
        .collect();
    assert!(fuera.is_empty(), "escape: {fuera:?}");
}

#[test]
fn nombres_con_nul_no_abren_el_paquete() {
    // NUL en nombre: 7z usa NUL como separador de nombres, así que la
    // cabecera queda inválida → error controlado en open (sin panic).
    let tmp = tempfile::tempdir().expect("tempdir");
    let mal = tmp.path().join("nul.7z");
    crear_7z_verbatim(&mal, &["nul\0byte.txt"]);
    let mut file = fs::File::open(&mal).expect("abrir");
    match Archive::open(&mut file) {
        Err(e) => assert!(
            matches!(e, ArcaError::Io(_)),
            "debe ser error controlado: {e}"
        ),
        Ok(mut archive) => {
            // Si algún día el parser lo tolera, la extracción debe seguir
            // rechazando el nombre.
            let root = tmp.path().join("out");
            let mut sink = DirSink::new(root.clone());
            let mut progreso = |_f: f64| {};
            assert!(archive
                .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
                .is_err());
        }
    }
}

#[test]
fn entradas_duplicadas_se_rechazan() {
    // Dos entradas con el MISMO nombre (colisión de sobreescritura).
    let tmp = tempfile::tempdir().expect("tempdir");
    let mal = tmp.path().join("dup.7z");
    crear_7z_verbatim(&mal, &["mismo.txt", "mismo.txt"]);
    let mut file = fs::File::open(&mal).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
    assert!(res.is_err(), "duplicados debían abortar");
    assert!(!root.exists() || walk_files(&root).is_empty());
}

#[test]
fn mezcla_malo_y_bueno_extrae_solo_si_todo_es_bueno() {
    // Un paquete con UNA entrada mala entre buenas aborta entero
    // (fail-fast: nunca media instalación).
    let tmp = tempfile::tempdir().expect("tempdir");
    let mal = tmp.path().join("mix.7z");
    crear_7z_verbatim(&mal, &["bueno.txt", "../malo.txt", "otro.txt"]);
    let mut file = fs::File::open(&mal).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    assert!(archive
        .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
        .is_err());
    assert!(
        !root.exists() || walk_files(&root).is_empty(),
        "nada debe quedar escrito (ni bueno.txt)"
    );
}

/// El invariante "todo path pasa sanitize ANTES de abrir el destino" se
/// sostiene incluso cuando el sink escribe en memoria: probamos que el
/// callback de progreso nunca recibe datos de una entrada maliciosa.
#[test]
fn nada_de_una_entrada_maliciosa_llega_al_sink() {
    struct SinkContador {
        eventos: Vec<String>,
    }
    impl EntrySink for SinkContador {
        fn mkdir(&mut self, rel: &arca_7z::RelPath) -> Result<(), ArcaError> {
            self.eventos.push(format!("dir:{}", rel));
            Ok(())
        }
        fn write_entry(
            &mut self,
            rel: &arca_7z::RelPath,
            _data: &mut dyn Read,
        ) -> Result<u64, ArcaError> {
            self.eventos.push(format!("file:{}", rel));
            Ok(0)
        }
        fn root(&self) -> &Path {
            Path::new("/tmp/arca7z-test")
        }
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let mal = tmp.path().join("mix2.7z");
    crear_7z_verbatim(&mal, &["bueno.txt", "../malo.txt"]);
    let mut file = fs::File::open(&mal).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let mut sink = SinkContador { eventos: vec![] };
    let mut progreso = |_f: f64| {};
    assert!(archive
        .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
        .is_err());
    assert!(
        sink.eventos.iter().all(|e| !e.contains("malo")),
        "el sink recibió una entrada maliciosa: {:?}",
        sink.eventos
    );
}
