//! Límite de memoria (spec 09 §6 / DoD T05): extraer **500 MiB** con pico
//! de RAM por debajo de 32 MB.
//!
//! Cómo se garantiza y verifica (documentado, ver también `Archive::extract`):
//!
//! 1. **Por construcción**: el bucle de extracción copia por bloques de
//!    tamaño fijo (`DirSink`: 1 MiB; drenado: 256 KiB); nunca se retiene
//!    una entrada completa. La única memoria proporcional fuera de este
//!    crate es el diccionario LZMA2 del decoder (paquete generado con
//!    preset 1 ⇒ diccionario de 1 MiB).
//! 2. **Contador de bytes**: `CountingSink` drena la entrada de 500 MiB en
//!    lecturas ≤ 64 KiB y cuenta el total (debe dar exacto).
//! 3. **Medición real de picos**: este binario instala un
//!    `#[global_allocator]` contable y mide el pico de bytes vivos
//!    (alloc − dealloc) DURANTE la extracción de 500 MiB: es una medida de
//!    asignación más estricta que el RSS (el RSS incluye páginas del
//!    allocator no devueltas al SO, así que si el pico de allocs pasa, el
//!    RSS puede quedar por encima solo por fragmentación del allocator,
//!    no por el diseño del streaming).
//!
//! El corpus `big500.7z` es un 7z REAL de py7zr (preset 1) cuyo contenido
//! son 500 MiB de datos comprimibles con contador por bloque.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use arca_7z::{Archive, DirSink, EntrySink, ExtractPlan};
use arca_types::{ArcaError, Res};

use common::corpus_dir;

/// Bytes actualmente vivos (global de ESTE binario de test).
static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// Pico histórico de bytes vivos.
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn bump(size: usize) {
    let cur = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(cur, Ordering::Relaxed);
}

/// Allocator contable (solo para este test).
struct PeakAlloc;

unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            bump(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = layout.size();
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            CURRENT.fetch_sub(old, Ordering::Relaxed);
            bump(new_size);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            bump(layout.size());
        }
        ptr
    }
}

#[global_allocator]
static GLOBAL: PeakAlloc = PeakAlloc;

/// Sink de conteo: drena en lecturas ≤ 64 KiB y verifica el streaming.
struct CountingSink {
    root: PathBuf,
    bytes: u64,
    max_read: usize,
    reads: u64,
}

impl CountingSink {
    fn new() -> Self {
        Self {
            root: PathBuf::from("/tmp/arca7z-counting"),
            bytes: 0,
            max_read: 0,
            reads: 0,
        }
    }
}

impl EntrySink for CountingSink {
    fn mkdir(&mut self, _rel: &arca_7z::RelPath) -> Res<()> {
        Ok(())
    }

    fn write_entry(&mut self, _rel: &arca_7z::RelPath, data: &mut dyn Read) -> Res<u64> {
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = data.read(&mut buf).map_err(ArcaError::Io)?;
            if n == 0 {
                break;
            }
            self.bytes += n as u64;
            self.max_read = self.max_read.max(n);
            self.reads += 1;
        }
        Ok(self.bytes)
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

const BIG: u64 = 500 * 1024 * 1024;
const LIMITE: usize = 32 * 1024 * 1024;

#[test]
fn extraccion_500mb_pico_menor_de_32mb() {
    let path = corpus_dir().join("big500.7z");
    let mut file = fs::File::open(&path).expect("abrir big500.7z");
    let mut archive = Archive::open(&mut file).expect("open big500.7z");

    // La entrada declara 500 MiB exactos.
    let entries = archive.entries().expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "gigante.bin");
    assert_eq!(entries[0].size, BIG);

    // ------- Fase 1: contador de bytes (sink en memoria nula) -------
    let mut sink = CountingSink::new();
    let mut llamadas: u64 = 0;
    let mut ultima = 0.0f64;
    let mut progreso = |f: f64| {
        assert!(f >= ultima, "progreso retrocede: {ultima} > {f}");
        ultima = f;
        llamadas += 1;
    };
    let base_peak = PEAK.load(Ordering::Relaxed);
    let base_current = CURRENT.load(Ordering::Relaxed);

    archive
        .extract(&ExtractPlan::all(), &mut sink, &mut progreso)
        .expect("extract 500 MiB");

    assert_eq!(sink.bytes, BIG, "bytes contados ≠ 500 MiB");
    // 500 MiB / 256 KiB ≈ 2000 callbacks esperados.
    assert!(llamadas >= 1000, "progreso demasiado escaso: {llamadas}");
    assert_eq!(ultima, 1.0);
    // Lecturas en bloques (nunca el archivo entero de una vez).
    assert!(
        sink.max_read <= 64 * 1024,
        "lectura gigante: {}",
        sink.max_read
    );
    assert!(
        sink.reads >= BIG / (64 * 1024),
        "pocas lecturas: {}",
        sink.reads
    );

    let pico = PEAK.load(Ordering::Relaxed) - base_peak;
    let pico_vivos = CURRENT.load(Ordering::Relaxed);
    assert!(
        pico < LIMITE,
        "pico de alloc durante 500 MiB: {} MB (límite 32 MB)",
        pico / (1024 * 1024)
    );
    // Y al terminar no queda nada retenido (los buffers se liberan).
    let fuga = pico_vivos.saturating_sub(base_current);
    assert!(
        fuga < 1024 * 1024,
        "posible fuga: {fuga} bytes vivos de más"
    );

    // ------- Fase 2: disco real (flujo del installer) -------
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("out");
    let mut sink_disco = DirSink::new(root.clone());
    let mut progreso2 = |_f: f64| {};
    let base_peak2 = PEAK.load(Ordering::Relaxed);
    let mut file2 = fs::File::open(&path).expect("reabrir");
    let mut archive2 = Archive::open(&mut file2).expect("reabrir");
    archive2
        .extract(&ExtractPlan::all(), &mut sink_disco, &mut progreso2)
        .expect("extract a disco");
    let pico2 = PEAK.load(Ordering::Relaxed) - base_peak2;
    assert!(
        pico2 < LIMITE,
        "pico de alloc en extracción a disco: {} MB",
        pico2 / (1024 * 1024)
    );
    assert_eq!(fs::metadata(root.join("gigante.bin")).unwrap().len(), BIG);
}
