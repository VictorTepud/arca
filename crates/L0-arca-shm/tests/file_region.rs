//! Tests de integración de la región de frames por archivo (F3a).
//!
//! `FrameFile` es el pegamento entre el hijo (escritor) y el host Kotlin
//! (lector) del probe visual: este test reproduce el contrato completo en
//! PC con DOS mapeos independientes del mismo archivo — como dos procesos
//! distintos — y verifica el protocolo seqlock de punta a punta.

use arca_shm::{region_len, FrameFile};
use arca_types::Res;

/// Escritura de un frame de `frame_bytes` con patrón verificable.
fn publicar(frame: &FrameFile, semilla: u8) -> Res<()> {
    let slot = frame.slots();
    let mut w = slot.begin_write(0)?;
    let payload = w.payload();
    payload.fill(semilla);
    payload[0] = b'F'; // marca de frame
    w.publish()
}

#[test]
fn escritor_y_lector_en_mapeos_distintos_se_ven() {
    const FRAME_BYTES: usize = 4096;
    let path = std::env::temp_dir().join("arca-shm-file-region-test.bin");

    // Rol HOST: crear el archivo con el tamaño exacto de la región (cero
    // ⇒ seq par ⇒ "sin frame válido"): replica lo que hace DemoActivity
    // con setLength(regionLen).
    std::fs::write(&path, vec![0u8; region_len(FRAME_BYTES)]).expect("write region");

    // Dos adjuntos INDEPENDIENTES (distintos fd, distintos mmap): hijo…
    let hijo = FrameFile::open(&path, FRAME_BYTES).expect("attach hijo");
    // …y host.
    let host = FrameFile::open(&path, FRAME_BYTES).expect("attach host");

    // Antes del primer publish: ambos slots inválidos.
    let mut out = vec![0u8; FRAME_BYTES];
    assert!(host.slots().read_latest_into(&mut out).is_none());

    // Hijo publica; host ve el frame sin ninguna señalización extra (la
    // notificación por stdout del probe es SOLO pacing, no coherencia).
    publicar(&hijo, 0xAB).expect("publish");
    let snap = host.slots().read_latest_into(&mut out).expect("frame");
    assert_eq!(snap.which, 0);
    assert_eq!(out[0], b'F');
    assert!(out[1..].iter().all(|&b| b == 0xAB));

    // Segundo frame en el OTRO slot: el lector debe elegir el más nuevo.
    let antes = snap.seq;
    let slot = hijo.slots();
    let mut w = slot.begin_write(1).expect("begin 2");
    w.payload().fill(0x11);
    w.publish().expect("publish 2");
    let snap2 = host.slots().read_latest_into(&mut out).expect("frame 2");
    assert_eq!(snap2.which, 1);
    assert!(snap2.seq > antes);
    assert!(out.iter().all(|&b| b == 0x11));

    std::fs::remove_file(&path).ok();
}

#[test]
fn archivo_demasiado_chico_se_rechaza() {
    let tmp = tempfile::tempfile().expect("tempfile");
    tmp.set_len(16).expect("set_len");
    // region_len(8) = 2*(16+8) = 48 ≠ 16 → fail-closed.
    assert!(FrameFile::from_file(&tmp, 8).is_err());
}

#[test]
fn persistencia_de_seqlock_entre_adjuntos_sucesivos() {
    // El probe reabre el archivo del HOST (que ya existe); los seqs viejos
    // deben seguir honrándose: read_latest elige el mayor impar.
    const FRAME_BYTES: usize = 64;
    let path = std::env::temp_dir().join("arca-shm-reopen-test.bin");
    std::fs::write(&path, vec![0u8; region_len(FRAME_BYTES)]).expect("write");

    let w = FrameFile::open(&path, FRAME_BYTES).expect("attach w");
    let slots = w.slots();
    let mut g = slots.begin_write(1).expect("begin");
    g.payload().fill(7);
    g.publish().expect("publish");
    drop(w);

    let r = FrameFile::open(&path, FRAME_BYTES).expect("attach r");
    let mut out = vec![0u8; FRAME_BYTES];
    let snap = r.slots().read_latest_into(&mut out).expect("frame");
    assert_eq!(snap.which, 1);
    assert!(out.iter().all(|&b| b == 7));

    std::fs::remove_file(&path).ok();
}
