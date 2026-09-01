//! Ring SPSC H→C para eventos de input (docs/04 §6).
//!
//! Un productor (host escribe input), un consumidor (sub-app lee). Posiciones
//! `tail` (productor, Release) y `head` (consumidor, Release). Sin futex:
//! tolerante al freezer por construcción (spec 05 §4).
//!
//! Layout (cabecera de 64 B = 1 línea de caché):
//! ```text
//!  0  magic   u32 = 0x52494E47 ('RING')
//!  4  version u16 = 1
//!  6  rsvd    u16
//!  8  slot_size  u32
//! 12  capacity   u32
//! 16  head    u64 (AtomicU64) — índice de consumo (módulo capacity)
//! 24  tail    u64 (AtomicU64) — índice de producción
//! 32  writer  u64 (AtomicU64) — detector de doble productor (SPSC)
//! 40  reader  u64 (AtomicU64) — detector de doble consumidor
//! 48  pad     16 B
//! 64  slots[capacity][slot_size]
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use arca_types::{ArcaError, Res};

/// Resultado de un push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    /// Slot publicado.
    Ok,
    /// Ring lleno: el slot NO se escribió. Política de compactación:
    /// ver [`RingSpsc::push_compacting`] y el contrato de seq por slot.
    Full,
    /// El slot se fusionó con el anterior (compactación de moves del host).
    /// Solo válido bajo el contrato de "seq embebido por slot" (abajo).
    Compacted {
        /// Slots consumidos de más tras la fusión (informativo).
        merged: u32,
    },
}

/// Firma de la función de fusión de [`RingSpsc::push_compacting`]:
/// (último slot publicado, slot nuevo, salida fusionada).
pub type MergeFn<'a> = &'a dyn Fn(&[u8], &[u8], &mut [u8]) -> bool;

/// Magic del ring.
pub const RING_MAGIC: u32 = 0x5249_4E47;
/// Versión del layout.
pub const RING_VERSION: u16 = 1;
/// Bytes de cabecera (línea de caché).
pub const RING_HEADER: usize = 64;

/// Contador global de vistas (único por instancia de `RingSpsc`).
static VIEW_N: AtomicU64 = AtomicU64::new(1);

/// Identidad de la VISTA (pid + nº de vista) para el detector SPSC.
///
/// Por instancia (no por hilo): dos vistas creadas por el mismo hilo también
/// son un bug de doble productor. El pid evita colisiones tras fork (el
/// hijo hereda el contador pero no el pid).
fn spsc_token() -> u64 {
    let pid = std::process::id() as u64;
    let n = VIEW_N.fetch_add(1, Ordering::Relaxed);
    (pid << 32) | (n & 0xffff_ffff)
}

/// Vista lock-free sobre una región mapeada que contiene un ring inicializado.
///
/// Invariante unsafe central: **la región apuntada es memoria compartida RW
/// mapeada en ambos procesos con el MISMO contenido físico** y permanece
/// mapeada mientras viva cualquier `RingSpsc` que la referencie.
#[derive(Debug)]
pub struct RingSpsc {
    base: *mut u8,
    slot_size: usize,
    capacity: usize,
    writer_token: u64,
    reader_token: u64,
}

// Seguro de mover entre hilos: toda la sincronización es vía atómicos en shm.
unsafe impl Send for RingSpsc {}
unsafe impl Sync for RingSpsc {}

impl RingSpsc {
    /// Inicializa el ring en un buffer recién mapeado (lado productor o
    /// antes de compartir). `slots * slot_size + 64` bytes exactos.
    pub fn init(buf: &mut [u8], slot_size: usize, slots: usize) -> Res<()> {
        if slot_size == 0 || slots == 0 {
            return Err(ArcaError::Internal("ring: slot_size/slots 0"));
        }
        let need = RING_HEADER + slots * slot_size;
        if buf.len() != need {
            return Err(ArcaError::Internal("ring: tamaño de región incorrecto"));
        }
        buf.fill(0);
        // Invariante: base alineada a página ⇒ alineación u32/u64 OK.
        unsafe {
            let p = buf.as_mut_ptr();
            (p as *mut u32).write_unaligned(RING_MAGIC);
            (p.add(4) as *mut u16).write_unaligned(RING_VERSION);
            (p.add(8) as *mut u32).write_unaligned(slot_size as u32);
            (p.add(12) as *mut u32).write_unaligned(slots as u32);
        }
        Ok(())
    }

    /// Adjunta un ring ya inicializado a una región mapeada (cualquier lado).
    ///
    /// # Safety
    /// `bytes` debe ser una región RW de shm de al menos `RING_HEADER + cap*ss`
    /// bytes que permanezca mapeada durante la vida de `Self`, y solo UN
    /// productor + UN consumidor construirán vistas de ESTE ring.
    pub unsafe fn from_bytes(bytes: &[u8]) -> Res<Self> {
        if bytes.len() < RING_HEADER {
            return Err(ArcaError::Internal("ring: región menor que cabecera"));
        }
        let p = bytes.as_ptr() as *mut u8;
        let magic = unsafe { (p as *const u32).read_unaligned() };
        if magic != RING_MAGIC {
            return Err(ArcaError::Internal("ring: magic inválido"));
        }
        let version = unsafe { (p.add(4) as *const u16).read_unaligned() };
        if version != RING_VERSION {
            return Err(ArcaError::Internal("ring: versión de layout no soportada"));
        }
        let slot_size = unsafe { (p.add(8) as *const u32).read_unaligned() } as usize;
        let capacity = unsafe { (p.add(12) as *const u32).read_unaligned() } as usize;
        if slot_size == 0 || capacity == 0 {
            return Err(ArcaError::Internal("ring: geometría corrupta"));
        }
        if bytes.len() < RING_HEADER + capacity * slot_size {
            return Err(ArcaError::Internal("ring: región menor que geometría"));
        }
        Ok(Self {
            base: p,
            slot_size,
            capacity,
            writer_token: spsc_token(),
            reader_token: spsc_token(),
        })
    }

    /// Slots de capacidad del ring.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes por slot.
    #[must_use]
    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// Slots pendientes de consumir (aprox en carrera: exacto si un solo
    /// lado corre).
    #[must_use]
    pub fn pending(&self) -> u64 {
        let tail = self.tail().load(Ordering::Acquire);
        let head = self.head().load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    fn head(&self) -> &AtomicU64 {
        // Invariante: offset 16 múltiplo de 8 sobre base alineada a página.
        unsafe { &*(self.base.add(16) as *const AtomicU64) }
    }
    fn tail(&self) -> &AtomicU64 {
        unsafe { &*(self.base.add(24) as *const AtomicU64) }
    }
    fn writer_id(&self) -> &AtomicU64 {
        unsafe { &*(self.base.add(32) as *const AtomicU64) }
    }
    fn reader_id(&self) -> &AtomicU64 {
        unsafe { &*(self.base.add(40) as *const AtomicU64) }
    }

    /// Registra el productor; error si ya hay otro (SPSC roto).
    fn claim_writer(&self) -> Res<()> {
        let me = self.writer_token;
        // Invariante: CAS en shm es lock-free (aarch64/x86_64).
        match self
            .writer_id()
            .compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),                 // primer productor
            Err(cur) if cur == me => Ok(()), // el mismo (re-push)
            Err(_) => Err(ArcaError::Internal(
                "ring: ¡segundo productor detectado! (SPSC)",
            )),
        }
    }

    /// Registra el consumidor; error si ya hay otro.
    fn claim_reader(&self) -> Res<()> {
        let me = self.reader_token;
        match self
            .reader_id()
            .compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(cur) if cur == me => Ok(()),
            Err(_) => Err(ArcaError::Internal(
                "ring: ¡segundo consumidor detectado! (SPSC)",
            )),
        }
    }

    fn slot_ptr(&self, idx: usize) -> *mut u8 {
        // Invariante: idx < capacity garantizado por callers.
        unsafe { self.base.add(RING_HEADER + idx * self.slot_size) }
    }

    /// Publica un slot (productor). `slot.len()` debe ser `slot_size()`.
    ///
    /// Orden de memoria: escritura del payload ANTES de `tail.store(Release)`;
    /// el consumidor ve el payload completo tras `tail.load(Acquire)`.
    pub fn push(&self, slot: &[u8]) -> Res<PushResult> {
        if slot.len() != self.slot_size {
            return Err(ArcaError::Internal(
                "ring: push con tamaño de slot incorrecto",
            ));
        }
        self.claim_writer()?;
        let tail = self.tail().load(Ordering::Relaxed); // propio
        let head = self.head().load(Ordering::Acquire); // progreso del consumidor
        if tail.wrapping_sub(head) >= self.capacity as u64 {
            return Ok(PushResult::Full);
        }
        let idx = (tail % self.capacity as u64) as usize;
        unsafe {
            std::ptr::copy_nonoverlapping(slot.as_ptr(), self.slot_ptr(idx), self.slot_size);
        }
        self.tail().store(tail.wrapping_add(1), Ordering::Release);
        Ok(PushResult::Ok)
    }

    /// Push con compactación del tipo "fusionar con el último slot no
    /// consumido" (política drop-oldest del host para moves de puntero,
    /// docs/04 §6).
    ///
    /// # Contrato de seguridad (llamador)
    /// Solo es válido si los slots **embeben un `seq` creciente** en su
    /// payload (lo hace arca-input): la fusión reescribe el último slot
    /// publicado y el consumidor detecta el cambio comparando su seq y
    /// RELEYENDO (mini-seqlock por slot). Sin seq embebido NO usar
    /// (lecturas torn no detectables).
    pub fn push_compacting(&self, slot: &[u8], can_merge: MergeFn<'_>) -> Res<PushResult> {
        match self.push(slot)? {
            PushResult::Ok => Ok(PushResult::Ok),
            PushResult::Compacted { .. } => Ok(PushResult::Compacted { merged: 0 }),
            PushResult::Full => {
                // fusión con el último slot publicado (NO consumido: el ring
                // está lleno ⇒ head == tail - capacity ⇒ tail-1 ≥ head).
                let tail = self.tail().load(Ordering::Relaxed);
                let last_idx = (tail.wrapping_sub(1) % self.capacity as u64) as usize;
                let mut scratch = vec![0u8; self.slot_size]; // fusiona fuera de shm
                let merged = {
                    let last = unsafe {
                        std::slice::from_raw_parts(self.slot_ptr(last_idx), self.slot_size)
                    };
                    if !can_merge(last, slot, &mut scratch) {
                        return Ok(PushResult::Full);
                    }
                    scratch
                };
                // Invariante: el consumidor puede estar leyendo `last` ahora
                // mismo; el contrato de seq embebido lo detecta y relee.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        merged.as_ptr(),
                        self.slot_ptr(last_idx),
                        self.slot_size,
                    );
                }
                Ok(PushResult::Compacted { merged: 1 })
            }
        }
    }

    /// Consume un slot a `out` (consumidor). `false` = ring vacío.
    ///
    /// Invariante de no-torn: el productor nunca sobrescribe el slot en
    /// `head % capacity` mientras esté sin consumir (su chequeo de espacio
    /// se lo impide), así que la copia es estable sin revalidación.
    pub fn pop(&self, out: &mut [u8]) -> Res<bool> {
        if out.len() != self.slot_size {
            return Err(ArcaError::Internal(
                "ring: pop con tamaño de slot incorrecto",
            ));
        }
        self.claim_reader()?;
        let head = self.head().load(Ordering::Relaxed); // propio
        let tail = self.tail().load(Ordering::Acquire); // progreso del productor
        if head == tail {
            return Ok(false);
        }
        let idx = (head % self.capacity as u64) as usize;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.slot_ptr(idx) as *const u8,
                out.as_mut_ptr(),
                self.slot_size,
            );
        }
        self.head().store(head.wrapping_add(1), Ordering::Release);
        Ok(true)
    }

    /// Consume hasta `max` slots invocando `f` por cada uno (payload prestado
    /// de la shm — cero copias). `f` devolviendo `false` detiene el drenado.
    ///
    /// Invariante: `f` se llama con el slot aún "no consumido" ⇒ el productor
    /// no puede reescribirlo durante la llamada (ver `pop`). El préstamo no
    /// puede escapar de `f` (lifetime).
    pub fn pop_each(&self, max: usize, mut f: impl FnMut(&[u8]) -> bool) -> Res<usize> {
        self.claim_reader()?;
        let mut n = 0usize;
        while n < max {
            let head = self.head().load(Ordering::Relaxed);
            let tail = self.tail().load(Ordering::Acquire);
            if head == tail {
                break;
            }
            let idx = (head % self.capacity as u64) as usize;
            let slot = unsafe {
                std::slice::from_raw_parts(self.slot_ptr(idx) as *const u8, self.slot_size)
            };
            if !f(slot) {
                break;
            }
            self.head().store(head.wrapping_add(1), Ordering::Release);
            n += 1;
        }
        Ok(n)
    }
}
