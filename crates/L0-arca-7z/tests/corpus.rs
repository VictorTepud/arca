//! Corpus: extracción y verificación de hashes de los 25 archivos 7z
//! REALES generados con py7zr (spec 09 §6: "corpus 20 archivos").
//!
//! Cada archivo `ok` se: abre, lista, extrae COMPLETO a un directorio
//! temporal y se compara cada byte (blake3 vía `arca-types::Digest` contra
//! la fuente determinista del corpus). Los `fail` deben rechazarse con
//! error (sin panic). `big` se prueba en `mem_limit.rs`.

mod common;

use std::fs;
use std::path::Path;

use arca_7z::{Archive, DirSink, ExtractPlan};
use arca_types::ArcaError;
use common::{corpus_dir, digest_of, load_manifest, walk_files};

#[test]
fn corpus_completo_extraccion_y_hashes() {
    let manifest = load_manifest();
    // ≥20 archivos reales (DoD T05).
    assert!(
        manifest.len() >= 20,
        "corpus insuficiente: {}",
        manifest.len()
    );

    for arc in &manifest {
        match arc.mode.as_str() {
            "ok" => verificar_ok(arc),
            "fail" => verificar_falla(arc),
            "big" => {} // mem_limit.rs
            otro => panic!("modo desconocido {otro} en {}", arc.file),
        }
    }
}

/// Abre, lista y extrae completo; verifica tamaños, hashes y directorios.
fn verificar_ok(arc: &common::CorpusArchive) {
    let path = corpus_dir().join(&arc.file);
    let mut file = fs::File::open(&path).expect("abrir corpus");
    let mut archive = match Archive::open(&mut file) {
        Ok(a) => a,
        Err(e) => panic!("{}: open falló: {e}", arc.file),
    };

    // --- listado ---
    let entries = archive.entries().expect("entries");
    for exp in &arc.entries {
        let found = entries
            .iter()
            .find(|e| e.path == exp.arcname)
            .unwrap_or_else(|| panic!("{}: falta entrada {}", arc.file, exp.arcname));
        assert_eq!(
            found.size, exp.size,
            "{}: tamaño de {}",
            arc.file, exp.arcname
        );
        assert!(
            found.crc.is_some(),
            "{}: {} sin CRC (py7zr siempre lo escribe)",
            arc.file,
            exp.arcname
        );
    }
    for d in &arc.dirs {
        assert!(
            entries.iter().any(|e| e.is_dir && e.path == *d),
            "{}: falta dir {}",
            arc.file,
            d
        );
    }

    // --- extracción completa ---
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink = DirSink::new(root.clone());
    let mut progreso = |_f: f64| {};
    archive
        .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
        .unwrap_or_else(|e| panic!("{}: extract falló: {e}", arc.file));

    // --- verificación byte a byte (blake3) ---
    let mut extracted = 0usize;
    for exp in &arc.entries {
        let out = root.join(&exp.arcname);
        assert!(out.is_file(), "{}: no se extrajo {}", arc.file, exp.arcname);
        assert_eq!(
            fs::metadata(&out).unwrap().len(),
            exp.size,
            "{}: tamaño en disco de {}",
            arc.file,
            exp.arcname
        );
        assert_eq!(
            digest_of(&out),
            digest_of(&exp.src),
            "{}: contenido divergente en {}",
            arc.file,
            exp.arcname
        );
        extracted += 1;
    }
    // Sin archivos de más ni .tmp sueltos.
    let on_disk = walk_files(&root);
    assert_eq!(
        on_disk.len(),
        extracted,
        "{}: archivos extraídos {} ≠ esperados {}",
        arc.file,
        on_disk.len(),
        extracted
    );
    for d in &arc.dirs {
        assert!(root.join(d).is_dir(), "{}: dir {} no creado", arc.file, d);
    }
    // Sin .arca-tmp residuales en ningún nivel.
    for p in &on_disk {
        assert!(
            !p.to_string_lossy().ends_with(arca_7z::TMP_SUFFIX),
            "{}: .tmp residual {:?}",
            arc.file,
            p
        );
    }
}

/// El pipeline open→extract debe fallar de forma controlada (sin panic).
fn verificar_falla(arc: &common::CorpusArchive) {
    let path = corpus_dir().join(&arc.file);
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => panic!("{}: ni siquiera abre en disco: {e}", arc.file),
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    match Archive::open(&mut file) {
        Err(e) => {
            // Rechazo en open (p. ej. cabecera imposible): error controlado.
            assert!(matches!(e, ArcaError::Io(_)), "{}: {e:?}", arc.file);
        }
        Ok(mut archive) => {
            let root = tmp.path().join("out");
            let mut sink = DirSink::new(root.clone());
            let mut progreso = |_f: f64| {};
            let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
            assert!(
                res.is_err(),
                "{}: debía fallar (paquete malicioso/cifrado)",
                arc.file
            );
            // Fail-fast: nada escrito en disco.
            assert!(
                !root.exists() || walk_files(&root).is_empty(),
                "{}: se escribieron archivos antes de abortar",
                arc.file
            );
        }
    }
}

#[test]
fn progreso_monotono_hasta_1() {
    let path = corpus_dir().join("pkg_layout.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut sink = DirSink::new(tmp.path().join("out"));
    let mut puntos: Vec<f64> = Vec::new();
    let mut progreso = |f: f64| puntos.push(f);
    archive
        .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
        .expect("extract");
    assert!(!puntos.is_empty(), "sin callbacks de progreso");
    assert_eq!(puntos[0], 0.0);
    assert_eq!(*puntos.last().unwrap(), 1.0);
    for w in puntos.windows(2) {
        assert!(w[0] <= w[1], "progreso no monótono: {puntos:?}");
    }
}

#[test]
fn archivos_corruptos_rechazados() {
    let orig = fs::read(corpus_dir().join("lzma2_p3.7z")).expect("leer");

    // 1) byte volteado en la zona de datos pack (~60% del archivo):
    //    CRC de la entrada debe fallar en extract.
    let mut corrupt = orig.clone();
    let idx = corrupt.len() * 60 / 100;
    corrupt[idx] ^= 0xFF;
    let mut cur = std::io::Cursor::new(&corrupt);
    let mut archive = Archive::open(&mut cur).expect("open con datos corruptos");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut sink = DirSink::new(tmp.path().join("out"));
    let mut progreso = |_f: f64| {};
    let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
    assert!(res.is_err(), "CRC debía fallar tras corromper datos");

    // 2) byte volteado DENTRO de la cabecera (al final del archivo: la
    //    cabecera de un 7z va tras los datos pack): open debe fallar.
    //    Posición del header = 32 + next_header_offset (start header,
    //    offset u64 LE en el byte 12).
    let hdr_off = u64::from_le_bytes(orig[12..20].try_into().expect("slice 8")) as usize;
    let hdr_pos = 32 + hdr_off + 5;
    assert!(hdr_pos < orig.len(), "posición de cabecera fuera de rango");
    let mut hdr_corrupt = orig.clone();
    hdr_corrupt[hdr_pos] ^= 0xFF;
    let mut cur = std::io::Cursor::new(&hdr_corrupt);
    assert!(
        Archive::open(&mut cur).is_err(),
        "cabecera corrupta (byte {hdr_pos}) debía fallar en open"
    );

    // 3) truncado: open o extract falla, sin panic.
    let trunc = &orig[..orig.len() / 2];
    let mut cur = std::io::Cursor::new(trunc);
    if let Ok(mut archive) = Archive::open(&mut cur) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut sink = DirSink::new(tmp.path().join("out"));
        let mut progreso = |_f: f64| {};
        assert!(archive
            .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
            .is_err());
    }

    // 4) archivo que no es 7z: open falla (BadSignature → Io/InvalidData).
    let mut cur = std::io::Cursor::new(b"esto no es un 7z, es un png roto" as &[u8]);
    assert!(Archive::open(&mut cur).is_err());
}

#[test]
fn permisos_unix_0700_0600() {
    #[cfg(unix)]
    {
        let path = corpus_dir().join("pkg_layout.7z");
        let mut file = fs::File::open(&path).expect("abrir");
        let mut archive = Archive::open(&mut file).expect("open");
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("out");
        let mut sink = DirSink::new(root.clone());
        let mut progreso = |_f: f64| {};
        archive
            .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
            .expect("extract");
        use std::os::unix::fs::PermissionsExt;
        let m = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(m(&root), 0o700, "root 0700");
        assert_eq!(m(&root.join("bin")), 0o700);
        assert_eq!(m(&root.join("bin/native-aarch64")), 0o700);
        assert_eq!(
            m(&root.join("bin/native-aarch64/app")),
            0o600,
            "archivo 0600"
        );
        assert_eq!(m(&root.join("manifest.toml")), 0o600);
    }
}
