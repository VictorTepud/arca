//! Región de frames respaldada por un archivo (modo probe F3a).
//!
//! En producción la shm viaja como `memfd` pasada por AIPC (docs/04 §6).
//! El **probe visual F3a** no tiene AIPC todavía: el host es el APK Kotlin
//! (`DemoActivity`) y el hijo comparte `filesDir` con él (mismo UID, mismo
//! sandbox — la grieta de targetSdk 28 que F0 ya demostró). Un archivo
//! mapeado `MAP_SHARED` por ambos lados da exactamente la coherencia que
//! `FrameSlots` necesita, sin pasar descriptores por un socket.
//!
//! Flujo (docs del probe, `graphs/gfx-f3a.mmd`):
//!
//! 1. El HOST crea el archivo con `set_len(region_len(frame_bytes))`
//!    (quedan a cero ⇒ seq par ⇒ "sin frame válido": exactamente la
//!    semántica de [`FrameSlots::init`], que aquí cumple el filesystem).
//! 2. El host mapea el archivo RW (Java: `FileChannel.map`) y lo deja
//!    abierto mientras viva la sesión del demo.
//! 3. El HIJO llama [`FrameFile::open`] con la MISMA geometría, escribe
//!    frames con [`FrameSlots::begin_write`]/`publish` y avanza.
//! 4. El host lee con el protocolo seqlock (Acquire/revalidación) directo
//!    sobre su mapeo — Kotlin reimplementa esas ~10 líneas.
//!
//! Invariante de vida del archivo: el host NO debe truncar/borrar el
//! archivo mientras el hijo lo tenga adjunto (el probe solo lo borra antes
//! de cada spawn nuevo).

use std::fs::OpenOptions;
use std::os::fd::AsFd as _;
use std::path::Path;

use arca_types::{ArcaError, Res};

use crate::frame::{region_len, FrameSlots};
use crate::map::ShmMap;

/// Región de frames mapeada desde un archivo: safe de USAR (todo el unsafe
/// vive dentro de [`FrameSlots`]/[`ShmMap`], que son unsafe-heavy).
///
/// Invariante de drop: [`FrameSlots`] no tiene `Drop` (solo apunta dentro
/// del mapeo), y `ShmMap` desmapea al caer: el orden de campos garantiza
/// que nadie use `slots` después del `munmap` (los campos se sueltan por
/// orden de declaración y `slots` es el primero).
#[derive(Debug)]
pub struct FrameFile {
    /// Primero (se "suelta" primero: no-op, sin Drop).
    slots: FrameSlots,
    /// Segundo: al caer hace el `munmap` real.
    _map: ShmMap,
}

impl FrameFile {
    /// Abre y mapea RW el archivo de región en `path`, validando que su
    /// tamaño sea EXACTAMENTE `region_len(frame_bytes)` (un tamaño distinto
    /// = el host y el hijo no acuerdan geometría: fail-closed, sin SIGBUS).
    ///
    /// NO inicializa el contenido: eso es trabajo del creador (el host, con
    /// `set_len` — ver módulo). Reabrir un archivo de una sesión anterior
    /// reusa los seqs viejos: el probe lo evita borrando el archivo antes
    /// de cada spawn.
    pub fn open(path: &Path, frame_bytes: usize) -> Res<Self> {
        if frame_bytes == 0 {
            return Err(ArcaError::Internal("frame-file: frame_bytes 0"));
        }
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Self::from_file(&file, frame_bytes)
    }

    /// Igual que [`FrameFile::open`] sobre un `File` ya abierto (el
    /// `--selftest` de devapp-demo usa su archivo temporal).
    pub fn from_file(file: &std::fs::File, frame_bytes: usize) -> Res<Self> {
        if frame_bytes == 0 {
            return Err(ArcaError::Internal("frame-file: frame_bytes 0"));
        }
        let len = file.metadata()?.len() as usize;
        let want = region_len(frame_bytes);
        if len != want {
            // Invariante: el mapeo jamás es más grande que el archivo
            // (evita SIGBUS en escritura, spec 05 §5) ni más chico (el
            // lector del host esperaría bytes que no existen).
            return Err(ArcaError::Internal(
                "frame-file: el archivo no mide region_len(frame_bytes) exacto",
            ));
        }
        let map = ShmMap::from_fd(file.as_fd(), len)?;
        // Invariante descargado aquí (crate unsafe-heavy): `ShmMap` acaba
        // de mapear RW un archivo de `len` bytes exactos y nadie lo trunca
        // durante la sesión (invariante de vida del módulo); `from_bytes`
        // revalida la geometría por su cuenta.
        let slots = unsafe { FrameSlots::from_bytes(map.as_slice())? };
        Ok(Self { slots, _map: map })
    }

    /// Acceso a la región (escritor: begin_write/publish; lector:
    /// read_latest_into).
    #[must_use]
    pub fn slots(&self) -> &FrameSlots {
        &self.slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechaza_frame_bytes_cero() {
        let tmp = tempfile::tempfile().expect("tempfile");
        assert!(FrameFile::from_file(&tmp, 0).is_err());
    }

    #[test]
    fn rechaza_tamano_que_no_cuadra() {
        let tmp = tempfile::tempfile().expect("tempfile");
        tmp.set_len(64).expect("set_len");
        // frame_bytes=10 → región = 2*(16+10) = 52 ≠ 64
        assert!(FrameFile::from_file(&tmp, 10).is_err());
    }
}
