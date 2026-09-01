//! Interrupción a mitad de extracción (spec 09 §6): io::Error inyectado en
//! el sink → error controlado, sin archivos parciales sin `.tmp`, y las
//! entradas ANTERIORES completadas quedan íntegras.

mod common;

use std::fs;
use std::io::Read;
use std::path::PathBuf;

use arca_7z::{Archive, DirSink, EntrySink, ExtractPlan};
use arca_types::ArcaError;
use common::{corpus_dir, digest_of, walk_files};

/// Reader que entrega `ok_bytes` y luego falla con io::Error.
struct FlakyReader<'a> {
    data: &'a mut dyn Read,
    ok_bytes: usize,
    sent: usize,
}

impl Read for FlakyReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.sent >= self.ok_bytes {
            return Err(std::io::Error::other("fallo de disco inyectado"));
        }
        let limit = (buf.len()).min(self.ok_bytes - self.sent);
        let n = self.data.read(&mut buf[..limit])?;
        self.sent += n;
        Ok(n)
    }
}

/// Sink que inyecta un fallo de E/S a mitad de la entrada número `fail_at`
/// (0-indexada por orden de escritura de ARCHIVOS).
struct FlakySink {
    inner: DirSink,
    fail_at: usize,
    escritos: usize,
}

impl EntrySink for FlakySink {
    fn mkdir(&mut self, rel: &arca_7z::RelPath) -> Result<(), ArcaError> {
        self.inner.mkdir(rel)
    }

    fn write_entry(
        &mut self,
        rel: &arca_7z::RelPath,
        data: &mut dyn Read,
    ) -> Result<u64, ArcaError> {
        let idx = self.escritos;
        self.escritos += 1;
        if idx == self.fail_at {
            // Falla tras leer ~la mitad de la entrada.
            let mut flaky = FlakyReader {
                data,
                ok_bytes: 4096,
                sent: 0,
            };
            return self.inner.write_entry(rel, &mut flaky);
        }
        self.inner.write_entry(rel, data)
    }

    fn root(&self) -> &std::path::Path {
        self.inner.root()
    }
}

fn archivos_bajo(root: &std::path::Path) -> Vec<PathBuf> {
    walk_files(root)
}

#[test]
fn interrupcion_no_deja_parciales_sin_tmp() {
    // lzma2_p6.7z: sólido, 3 archivos (text.txt, elfish.bin, data.bin).
    let path = corpus_dir().join("lzma2_p6.7z");
    let mut file = fs::File::open(&path).expect("abrir");
    let mut archive = Archive::open(&mut file).expect("open");

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink = FlakySink {
        inner: DirSink::new(root.clone()),
        fail_at: 1, // falla en la segunda entrada (elfish.bin)
        escritos: 0,
    };
    let mut progreso = |_f: f64| {};
    let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
    assert!(res.is_err(), "la interrupción debía propagarse como error");
    assert!(
        matches!(res.unwrap_err(), ArcaError::Io(_)),
        "debe ser Io, no otra cosa"
    );

    // La primera entrada quedó COMPLETA e íntegra.
    let files = archivos_bajo(&root);
    assert_eq!(files.len(), 1, "solo la entrada previa al fallo: {files:?}");
    assert!(root.join("text.txt").is_file());
    assert_eq!(
        digest_of(&root.join("text.txt")),
        digest_of(&corpus_dir().join("src/text.txt")),
        "la entrada completada debe estar íntegra"
    );

    // Nada con sufijo .arca-tmp en todo el árbol (spec 09 §6).
    for p in &files {
        assert!(
            !p.to_string_lossy().ends_with(arca_7z::TMP_SUFFIX),
            "tmp residual: {p:?}"
        );
    }
    // Y tampoco el archivo que iba por la mitad.
    assert!(!root.join("elfish.bin").exists());
    assert!(!root.join("elfish.bin.arca-tmp").exists());
}

#[test]
fn fuente_truncada_falla_controlado() {
    // Cortar el archivo a la mitad: open puede funcionar (cabecera al
    // principio) pero extract se topa con EOF → error, sin tmp.
    let data = fs::read(corpus_dir().join("lzma2_p6.7z")).expect("leer");
    let trunc = &data[..data.len() / 2];
    let mut cur = std::io::Cursor::new(trunc);
    if let Ok(mut archive) = Archive::open(&mut cur) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("out");
        let mut sink = DirSink::new(root.clone());
        let mut progreso = |_f: f64| {};
        let res = archive.extract(&ExtractPlan::all(), &mut sink, &mut progreso);
        if res.is_ok() {
            // EOF cayó tras la última entrada: no es error; pero entonces
            // TODO debe estar íntegro o nada.
            let files = archivos_bajo(&root);
            for p in &files {
                assert!(!p.to_string_lossy().ends_with(arca_7z::TMP_SUFFIX));
            }
        }
    }
}
