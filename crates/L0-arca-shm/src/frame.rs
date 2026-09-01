//! Double-buffer de frames con seqlock (C→H, docs/04 §7).
//!
//! Región = 2 slots. Cada slot:
//! ```text
//!  0  seq  u64 (AtomicU64 en shm) — PAR = escribiendo/inválido,
//!                                     IMPAR = válido (docs/04 §7)
//!  8  pad  8 B
//! 16  payload N bytes (FrameHeader+meshes; el layout lo define gfx-protocol)
//! ```
//!
//! Protocolo escritor (sub-app): `begin_write` (seq→par) → escribe payload →
//! `publish` (seq→impar con `Release`).
//! Protocolo lector (host): lee seq (`Acquire`); si par → slot inválido;
//! copia payload; revalida seq — si cambió, reintenta (máx 2 por slot y
//! luego prueba el otro slot: docs/04 §7).

use std::sync::atomic::{AtomicU64, Ordering};

use arca_types::{ArcaError, Res};

/// Bytes de cabecera por slot (seq + pad).
pub const SLOT_HEADER: usize = 16;
/// Número de slots (double buffer).
pub const SLOTS: usize = 2;

/// Snapshot leído con éxito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSnap {
    /// Slot leído (0/1).
    pub which: usize,
    /// Seq del seqlock (impar = válido; creciente entre publicaciones).
    pub seq: u64,
    /// Bytes copiados al buffer del lector.
    pub len: usize,
}

/// Vista sobre la región de frames.
///
/// Invariante unsafe: región RW de shm compartida, mapeada mientras viva
/// esta vista, con geometría `2 * (SLOT_HEADER + frame_bytes)` exactos.
///
/// `next_seq` es PRIVADO de la instancia del ESCRITOR (no viaja por shm):
/// produce seqs GLOBALMENTE monotónicos (1, 3, 5, …) para que el lector
/// pueda elegir "el más nuevo" sin empates. Invariante de ciclo de vida:
/// la región se (re)inicializa en cada launch (el host crea memfd nuevos
/// por instancia), así que reiniciar `next_seq` a 1 nunca compite con seqs
/// viejos.
#[derive(Debug)]
pub struct FrameSlots {
    base: *mut u8,
    frame_bytes: usize,
    next_seq: AtomicU64,
}

unsafe impl Send for FrameSlots {}
unsafe impl Sync for FrameSlots {}

impl FrameSlots {
    /// Inicializa la región (zeros: seq=0 par = "sin frame válido").
    pub fn init(buf: &mut [u8], frame_bytes: usize) -> Res<()> {
        if frame_bytes == 0 {
            return Err(ArcaError::Internal("frames: frame_bytes 0"));
        }
        let need = region_len(frame_bytes);
        if buf.len() != need {
            return Err(ArcaError::Internal("frames: tamaño de región incorrecto"));
        }
        buf.fill(0);
        Ok(())
    }

    /// Adjunta una región ya inicializada.
    ///
    /// # Safety
    /// Misma invariante que la del tipo: región RW de shm viva durante `Self`
    /// y geometría correcta (se revalida el tamaño).
    pub unsafe fn from_bytes(bytes: &[u8]) -> Res<Self> {
        if bytes.len() < SLOT_HEADER * SLOTS {
            return Err(ArcaError::Internal("frames: región mínima"));
        }
        let frame_bytes = (bytes.len() - SLOT_HEADER * SLOTS) / SLOTS;
        if frame_bytes == 0 || bytes.len() != region_len(frame_bytes) {
            return Err(ArcaError::Internal("frames: geometría no cuadra"));
        }
        Ok(Self {
            base: bytes.as_ptr() as *mut u8,
            frame_bytes,
            next_seq: AtomicU64::new(1),
        })
    }

    /// Bytes de payload por slot.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    fn seq_of(&self, which: usize) -> &AtomicU64 {
        // Invariante: offset 0, alineación u64 sobre base de página.
        debug_assert!(which < SLOTS);
        unsafe { &*(self.base.add(which * (SLOT_HEADER + self.frame_bytes)) as *const AtomicU64) }
    }

    fn payload_ptr(&self, which: usize) -> *mut u8 {
        debug_assert!(which < SLOTS);
        unsafe {
            self.base
                .add(which * (SLOT_HEADER + self.frame_bytes) + SLOT_HEADER)
        }
    }

    /// Abre el slot `which` para escritura (escritor: sub-app).
    ///
    /// Si el guard se dropea sin `publish()`, el slot queda en seq PAR =
    /// inválido (frame abortado limpio; el lector lo salta).
    pub fn begin_write(&self, which: usize) -> Res<WriteSlot<'_>> {
        if which >= SLOTS {
            return Err(ArcaError::Internal("frames: which fuera de rango"));
        }
        let seq = self.seq_of(which);
        let cur = seq.load(Ordering::Relaxed);
        if cur % 2 == 1 {
            // impar (última publicación de ESTE slot): pasa a par = "escribiendo".
            // Se marca con el valor que TENDRÁ la próxima publicación menos 1
            // (par) para mantener la paridad local coherente.
            // Invariante: escritor ÚNICO (sub-app); store Relaxed porque el
            // publish posterior lleva Release y el lector revalida.
            let next = self.next_seq.load(Ordering::Relaxed);
            seq.store(next.wrapping_sub(1), Ordering::Relaxed);
        }
        // par (0 o previo): ya está en estado "escribiendo/inválido".
        Ok(WriteSlot {
            slots: self,
            which,
            published: false,
        })
    }

    /// Lee el frame válido más reciente, copiando a `out` (lector: host).
    ///
    /// Copiar-y-revalidar (no hay forma segura de prestar &shm mientras el
    /// escritor corre). `out.len() == frame_bytes()` para copia completa;
    /// para copias parciales de dos etapas (T20: header primero) usa
    /// [`Self::read_slot_into`].
    ///
    /// Devuelve `None` si ambos slots están inválidos o las 2 lecturas por
    /// slot fueron torn (escritor muy agresivo).
    pub fn read_latest_into(&self, out: &mut [u8]) -> Option<FrameSnap> {
        // Elige el slot válido con seq mayor (publicación más reciente).
        // Los seqs son globales monotónicos ⇒ sin empates (ver doc del tipo).
        let s0 = self.seq_of(0).load(Ordering::Acquire);
        let s1 = self.seq_of(1).load(Ordering::Acquire);
        let (v0, v1) = (s0 % 2 == 1, s1 % 2 == 1);
        let order: [usize; 2] = match (v0, v1) {
            (true, true) => {
                if s0 > s1 {
                    [0, 1]
                } else {
                    [1, 0]
                }
            }
            (true, false) => [0, 1],
            (false, true) => [1, 0],
            (false, false) => return None,
        };
        for which in order {
            if let Some(snap) = self.read_slot_into(which, out) {
                return Some(snap);
            }
        }
        None
    }

    /// Lee un slot concreto con protocolo seqlock completo (2 intentos).
    pub fn read_slot_into(&self, which: usize, out: &mut [u8]) -> Option<FrameSnap> {
        let n = out.len().min(self.frame_bytes);
        for _ in 0..2 {
            let s1 = self.seq_of(which).load(Ordering::Acquire);
            if s1 % 2 == 0 {
                continue; // escribiendo/inválido → reintenta (quizá termina)
            }
            // Invariante: copia bajo protección de seqlock — si el escritor
            // reescribe, seq cambia (par durante la escritura) y la
            // revalidación lo detecta.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.payload_ptr(which) as *const u8,
                    out.as_mut_ptr(),
                    n,
                );
            }
            let s2 = self.seq_of(which).load(Ordering::Acquire);
            if s1 == s2 && s2 % 2 == 1 {
                return Some(FrameSnap {
                    which,
                    seq: s1,
                    len: n,
                });
            }
        }
        None
    }
}

/// Guard de escritura de un slot de frame. `Drop` sin publicar = abort.
#[derive(Debug)]
pub struct WriteSlot<'a> {
    slots: &'a FrameSlots,
    which: usize,
    published: bool,
}

impl WriteSlot<'_> {
    /// Payload del slot (escribir aquí el frame serializado).
    pub fn payload(&mut self) -> &mut [u8] {
        // Invariante: escritor único; seq está PAR durante toda la escritura.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.slots.payload_ptr(self.which),
                self.slots.frame_bytes,
            )
        }
    }

    /// Publica el frame (seq par→impar con `Release`).
    pub fn publish(mut self) -> Res<()> {
        self.publish_inner()
    }

    fn publish_inner(&mut self) -> Res<()> {
        let seq = self.slots.seq_of(self.which);
        let cur = seq.load(Ordering::Relaxed);
        if cur % 2 != 0 {
            return Err(ArcaError::Internal(
                "frames: publish con seq impar (bug escritor)",
            ));
        }
        // Seq GLOBAL (único por publicación en ambos slots): sin empates,
        // el lector elige el máximo par-impar válido = el más reciente.
        let global = self
            .slots
            .next_seq
            .fetch_add(2, Ordering::Relaxed)
            .wrapping_add(2);
        // Invariante: Release ordena las escrituras del payload ANTES del
        // cambio de seq; el lector (Acquire) ve el frame completo.
        seq.store(global, Ordering::Release);
        self.published = true;
        Ok(())
    }
}

impl Drop for WriteSlot<'_> {
    fn drop(&mut self) {
        if !self.published {
            // Abandono: seq quedó PAR → inválido para el lector. Nada que
            // hacer (el siguiente begin_write lo reutiliza).
        }
    }
}

/// Tamaño total de la región para `frame_bytes` por slot.
#[must_use]
pub fn region_len(frame_bytes: usize) -> usize {
    SLOTS * (SLOT_HEADER + frame_bytes)
}
