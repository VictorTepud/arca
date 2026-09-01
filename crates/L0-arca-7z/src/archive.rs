//! [`Archive`]: apertura, listado y extracción **streaming** de paquetes 7z
//! sobre `sevenz-rust2` (spec 09 §3).
//!
//! Este crate no conoce manifests (eso es `arca-pkg-model`): recibe bytes
//! de un 7z y produce archivos seguros en disco. Toda la validación de
//! seguridad local está en [`crate::sanitize_entry_path`].

use std::collections::HashSet;
use std::io::{Read, Seek};

use arca_types::{ArcaError, Res};

use crate::path::RelPath;
use crate::sink::EntrySink;

/// Cada cuántos bytes se invoca el callback de progreso (spec 09 §4).
pub const PROGRESS_EVERY_BYTES: u64 = 256 * 1024;

/// Buffer de drenado de entradas saltadas (tamaño fijo, invariante O(1)).
const DRAIN_BUF_BYTES: usize = 256 * 1024;

/// Metadatos de una entrada del archivo (spec 09 §3: `{ path, size, crc?, is_dir }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// Nombre crudo de la entrada tal como está en el 7z (diagnóstico:
    /// puede contener paths que [`crate::sanitize_entry_path`] rechazaría).
    pub path: String,
    /// Tamaño descomprimido en bytes.
    pub size: u64,
    /// CRC32 del contenido si el paquete lo declara.
    pub crc: Option<u32>,
    /// ¿Es un directorio?
    pub is_dir: bool,
}

impl EntryInfo {
    /// Versión saneada del path (`None` si la entrada es sospechosa y no
    /// se debe extraer).
    pub fn safe_path(&self) -> Option<RelPath> {
        crate::sanitize_entry_path(&self.path)
    }
}

/// Plan de extracción (spec 09 §3).
///
/// - `wanted` vacío → se extrae todo.
/// - `wanted` con `reject_others = true` → **extracción selectiva**: solo
///   las entradas que coinciden con `wanted` (path exacto o subtree: `bin`
///   cubre `bin/wasm/app.wasm`). El resto se drena (para avanzar el stream
///   y verificar CRC) sin escribirse. Si falta algún `wanted` al terminar,
///   es error.
/// - `wanted` con `reject_others = false` → equivalente a extraer todo
///   (no se descarta nada); se mantiene por simetría con el contrato.
#[derive(Debug, Clone, Default)]
pub struct ExtractPlan {
    /// Entradas (o subtree raíz) deseadas.
    pub wanted: Vec<RelPath>,
    /// Si es `true`, las entradas fuera de `wanted` no se escriben.
    pub reject_others: bool,
}

impl ExtractPlan {
    /// Plan "extraer todo".
    pub fn all() -> Self {
        Self {
            wanted: Vec::new(),
            reject_others: false,
        }
    }

    /// Plan selectivo: solo `wanted` (las demás entradas no se escriben).
    pub fn selective(wanted: Vec<RelPath>) -> Self {
        Self {
            wanted,
            reject_others: true,
        }
    }

    /// Plan selectivo saneando cada path crudo; falla si alguno es inválido
    /// (mal paquete o bug del llamador).
    pub fn parse(wanted_raw: &[&str]) -> Res<Self> {
        let bad = || ArcaError::Internal("ExtractPlan: path solicitado inv\u{00e1}lido");
        let mut wanted = Vec::with_capacity(wanted_raw.len());
        for raw in wanted_raw {
            wanted.push(crate::sanitize_entry_path(raw).ok_or_else(bad)?);
        }
        Ok(Self::selective(wanted))
    }

    /// ¿Este plan extrae todo lo saneable?
    fn extracts_all(&self) -> bool {
        self.wanted.is_empty() || !self.reject_others
    }

    /// ¿`rel` coincide con el plan? (exacto o subtree de un `wanted`).
    fn wants(&self, rel: &RelPath, extracts_all: bool) -> bool {
        if extracts_all {
            return true;
        }
        let s = rel.as_str();
        self.wanted
            .iter()
            .any(|w| w.as_str() == s || s.starts_with(&format!("{}/", w.as_str())))
    }
}

/// Handle a un archivo 7z abierto desde un `Read + Seek` (spec 09 §3).
///
/// La fuente de datos se retiene (ownership) porque la extracción es
/// streaming: se re-busca (seek) por bloque mientras se decodifica, sin
/// cargar el paquete en memoria.
pub struct Archive<R: Read + Seek> {
    inner: sevenz_rust2::ArchiveReader<R>,
}

/// Convierte un error de `sevenz-rust2` al error canónico de Arca.
///
/// Mapeo (decisión documentada en worklog T05):
/// - `Io(e, _)` → `ArcaError::Io(e)`.
/// - todo lo demás (método no soportado, CRC, firma mala…) →
///   `ArcaError::Io(io::Error::new(InvalidData, e))`: conserva la cadena
///   completa del error original (p. ej.
///   `UnsupportedCompressionMethod("BCJ2")`, diagnóstico de spec 09 §5) en
///   lugar de colapsarlo a un `Internal(&'static str)` sin datos.
fn zerr(e: sevenz_rust2::Error) -> ArcaError {
    match e {
        sevenz_rust2::Error::Io(io_e, _) => ArcaError::Io(io_e),
        other => ArcaError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, other)),
    }
}

/// Estado compartido del contador de progreso de una extracción.
struct ProgressState {
    /// Bytes descomprimidos hasta ahora (escritos + drenados).
    done: u64,
    /// Último `done` en el que se emitió progreso.
    last_cb: u64,
    /// Total de bytes de stream a procesar (para la fracción).
    total: u64,
}

impl ProgressState {
    /// Registra `n` bytes y devuelve `Some(frac)` si toca emitir progreso
    /// (cada [`PROGRESS_EVERY_BYTES`]).
    fn advance(&mut self, n: u64) -> Option<f64> {
        self.done = self.done.saturating_add(n);
        if self.done.saturating_sub(self.last_cb) >= PROGRESS_EVERY_BYTES {
            self.last_cb = self.done;
            return Some(self.frac());
        }
        None
    }

    /// Fracción actual acotada a `[0,1]` (total 0 → 1.0).
    fn frac(&self) -> f64 {
        if self.total == 0 {
            1.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
}

/// Wrapper de `Read` que reporta bytes leídos al [`ProgressState`] e invoca
/// el callback cuando corresponde.
///
/// Así el progreso se emite cada ≥ 256 KiB **incluso dentro de una sola
/// entrada enorme** (el sink copia el archivo en bloques y cada lectura
/// pasa por aquí).
struct ProgressReader<'a> {
    inner: &'a mut dyn Read,
    state: &'a mut ProgressState,
    progress: &'a mut dyn FnMut(f64),
}

impl Read for ProgressReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            if let Some(frac) = self.state.advance(n as u64) {
                (self.progress)(frac);
            }
        }
        Ok(n)
    }
}

/// Reordena `files` para que cada bloque ocupe un rango de índices
/// CONTIGUO y exacto, y las entradas sin stream queden al final.
///
/// NOTA(agent) — R-04 (hallazgo real del corpus): `sevenz-rust2` 0.22.2
/// itera los archivos de cada bloque como el rango de índices
/// `[first_file .. first_file + num_substreams)`.
/// `py7zr` (y en general cualquier escritor que intercale entradas sin
/// stream —directorios— entre archivos con stream del mismo bloque)
/// rompe esa suposición: el rango se "come" un directorio y el ÚLTIMO
/// archivo del bloque queda FUERA del rango y se pierde en silencio
/// (reproducible: `tree.7z` del corpus perdía `ñandú/水.txt`).
///
/// El mapeo LÓGICO `stream_map.file_block_index` sí es correcto (el k-ésimo
/// archivo con stream pertenece al bloque dueño del k-ésimo substream),
/// así que esta función reconstruye el orden canónico:
/// `[streams del bloque 0, streams del bloque 1, …, entradas sin stream]`
/// usando SOLO API pública de `sevenz-rust2` (`Archive::read` +
/// `ArchiveReader::from_archive`). El orden de streams (y por tanto el
/// "manifest.toml primero" de docs/06 §2) se conserva.
fn normalize_stream_spans(mut a: sevenz_rust2::Archive) -> Res<sevenz_rust2::Archive> {
    let nblocks = a.blocks.len();
    if a.files.len() != a.stream_map.file_block_index.len() {
        return Err(ArcaError::Internal("stream_map inconsistente"));
    }
    // Substreams por bloque según el mapeo lógico (válido incluso con
    // entradas sin stream intercaladas).
    let mut streamed: Vec<Vec<usize>> = vec![Vec::new(); nblocks];
    let mut no_stream: Vec<usize> = Vec::new();
    for (i, f) in a.files.iter().enumerate() {
        let b = a.stream_map.file_block_index[i];
        if f.has_stream {
            match b {
                Some(b) if b < nblocks => streamed[b].push(i),
                _ => {
                    // Cabecera corrupta: un archivo con stream sin bloque.
                    return Err(ArcaError::Internal("entrada con stream sin bloque"));
                }
            }
        } else {
            no_stream.push(i);
        }
    }
    // Nuevo orden canónico.
    let mut order: Vec<usize> = Vec::with_capacity(a.files.len());
    let mut first: Vec<usize> = Vec::with_capacity(nblocks);
    for streams_b in &streamed {
        first.push(order.len());
        order.extend_from_slice(streams_b);
    }
    order.extend_from_slice(&no_stream);
    if order.len() != a.files.len() {
        return Err(ArcaError::Internal("reordenado inconsistente"));
    }

    // Reescribir `files` y los dos campos públicos del stream_map.
    let old_files = a.files.clone();
    let old_block_of = a.stream_map.file_block_index.clone();
    a.files = order.iter().map(|&i| old_files[i].clone()).collect();
    for (b, start) in first.iter().enumerate() {
        a.stream_map.block_first_file_index[b] = *start;
    }
    for (new_i, &old_i) in order.iter().enumerate() {
        a.stream_map.file_block_index[new_i] = if old_files[old_i].has_stream {
            old_block_of[old_i]
        } else {
            None
        };
    }
    Ok(a)
}

impl<R: Read + Seek> Archive<R> {
    /// Abre el archivo (lee y valida la cabecera 7z).
    ///
    /// Soporta cabeceras comprimidas y detecta cabeceras corruptas; los
    /// paquetes con cifrado AES devuelven error (v1 sin cifrado, docs/06 §6).
    ///
    /// El orden de `entries()` tras `open` es el canónico normalizado por
    /// [`normalize_stream_spans`]: primero las entradas con stream (en
    /// orden de bloque = orden de escritura), luego directorios/vacíos.
    pub fn open(reader: R) -> Res<Self> {
        let mut reader = reader;
        let password = sevenz_rust2::Password::empty();
        // Parse #1 (solo cabecera) + normalización del orden de archivos
        // (workaround R-04: ver normalize_stream_spans).
        let parsed = sevenz_rust2::Archive::read(&mut reader, &password).map_err(zerr)?;
        let normalized = normalize_stream_spans(parsed)?;
        let mut inner = sevenz_rust2::ArchiveReader::from_archive(normalized, reader, password);
        // Decodificación single-thread: acota la memoria (LZMA2-MT replica
        // el diccionario por hilo) y el installer no necesita paralelismo
        // por bloque (spec 09 §4: streaming O(1)).
        inner.set_thread_count(1);
        Ok(Self { inner })
    }

    /// Lista las entradas del archivo (sin descomprimir nada).
    pub fn entries(&self) -> Res<Vec<EntryInfo>> {
        let files = &self.inner.archive().files;
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            out.push(EntryInfo {
                path: f.name.clone(),
                size: f.size,
                crc: f.has_crc.then_some(f.crc as u32),
                is_dir: f.is_directory,
            });
        }
        Ok(out)
    }

    /// Extrae según `plan` hacia `sink`, informando progreso con `progress`.
    ///
    /// `progress` recibe la fracción `[0.0, 1.0]` de bytes descomprimidos;
    /// se invoca como mucho una vez cada [`PROGRESS_EVERY_BYTES`] y una
    /// final con `1.0` (spec 09 §4).
    ///
    /// Seguridad (spec 09 §4):
    /// - **pre-escaneo** de todas las entradas: cualquier path que
    ///   [`crate::sanitize_entry_path`] rechace, cualquier entrada *anti*
    ///   7z o nombre duplicado aborta la extracción **antes** de escribir
    ///   el primer byte (fail-fast: nunca media instalación de un paquete
    ///   malicioso).
    /// - cada path vuelve a pasar `sanitize_entry_path` justo antes de
    ///   abrirse en el sink.
    ///
    /// Memoria (spec 09 §4): el bucle copia por bloques de tamaño fijo
    /// (sink: 1 MiB; drenado: 256 KiB); nunca se retiene el contenido de
    /// una entrada completo. La única memoria proporcional que queda fuera
    /// de este crate es el diccionario LZMA2 del decoder (lo documenta
    /// [`crate::probe_features`]).
    pub fn extract(
        &mut self,
        plan: &ExtractPlan,
        sink: &mut dyn EntrySink,
        progress: &mut dyn FnMut(f64),
    ) -> Res<()> {
        // ---------- Fase 1: pre-escaneo (fail-fast) ----------
        let files = &self.inner.archive().files;
        let mut names: HashSet<String> = HashSet::with_capacity(files.len());
        let mut total_stream_bytes: u64 = 0;
        for f in files {
            if f.is_anti_item {
                return Err(ArcaError::Internal(
                    "entrada anti-7z no permitida en un paquete",
                ));
            }
            let sanitized = crate::sanitize_entry_path(&f.name).ok_or(ArcaError::Internal(
                "path de entrada rechazado por el sandbox",
            ))?;
            if !names.insert(sanitized.to_string()) {
                return Err(ArcaError::Internal("entrada duplicada en el paquete"));
            }
            if f.has_stream {
                total_stream_bytes = total_stream_bytes.saturating_add(f.size);
            }
        }

        let extracts_all = plan.extracts_all();
        let mut covered: HashSet<String> = HashSet::new();
        let mut st = ProgressState {
            done: 0,
            last_cb: 0,
            total: total_stream_bytes,
        };
        let mut drain_buf = vec![0u8; DRAIN_BUF_BYTES];
        let mut failure: Option<ArcaError> = None;
        // Red de seguridad R-04: ninguna entrada puede perderse en silencio.
        let total_entries = files.len();
        let mut visitados: usize = 0;

        progress(0.0);

        // ---------- Fase 2: extracción streaming ----------
        let each = |entry: &sevenz_rust2::ArchiveEntry,
                    reader: &mut dyn Read|
         -> Result<bool, sevenz_rust2::Error> {
            if failure.is_some() {
                // Aborto temprano tras un fallo anterior.
                return Ok(false);
            }
            visitados += 1;
            let Some(rel) = crate::sanitize_entry_path(entry.name()) else {
                failure = Some(ArcaError::Internal(
                    "path de entrada rechazado por el sandbox",
                ));
                return Ok(false);
            };
            if entry.is_anti_item() {
                failure = Some(ArcaError::Internal(
                    "entrada anti-7z no permitida en un paquete",
                ));
                return Ok(false);
            }
            let wanted = plan.wants(&rel, extracts_all);
            if wanted {
                covered.insert(rel.to_string());
            }

            if entry.is_directory() {
                if wanted {
                    if let Err(e) = sink.mkdir(&rel) {
                        failure = Some(e);
                        return Ok(false);
                    }
                }
                return Ok(true);
            }

            if wanted {
                // El progreso fluye a través del wrapper: el sink lee en
                // bloques y cada lectura actualiza `st` + dispara el
                // callback si toca.
                let mut pr = ProgressReader {
                    inner: reader,
                    state: &mut st,
                    progress: &mut *progress,
                };
                match sink.write_entry(&rel, &mut pr) {
                    Ok(n) => {
                        if n != entry.size() {
                            failure = Some(ArcaError::Internal(
                                "bytes escritos \u{2260} tama\u{00f1}o declarado",
                            ));
                            return Ok(false);
                        }
                    }
                    Err(e) => {
                        failure = Some(e);
                        return Ok(false);
                    }
                }
            } else {
                // Entrada no deseada: hay que DRENARLA por completo para
                // avanzar el bloque (sólidos) y que se verifique su CRC.
                loop {
                    match reader.read(&mut drain_buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Some(frac) = st.advance(n as u64) {
                                progress(frac);
                            }
                        }
                        Err(e) => {
                            failure = Some(zerr(sevenz_rust2::Error::from(e)));
                            return Ok(false);
                        }
                    }
                }
            }

            Ok(true)
        };

        let iter = self.inner.for_each_entries(each);
        if let Some(f) = failure {
            return Err(f);
        }
        iter.map_err(zerr)?;
        if visitados != total_entries {
            // Nunca debería pasar tras `normalize_stream_spans`, pero un
            // archivo con layout desconocido NO puede perder entradas en
            // silencio (R-04).
            return Err(ArcaError::Internal(
                "layout de streams no soportado: entradas no visitadas",
            ));
        }

        // ---------- Fase 3: post-condiciones ----------
        if !extracts_all {
            // Un `wanted` W está cubierto si se extrajo W exactamente o
            // cualquier cosa bajo W/ (subtree). Ej.: pedir "bin" queda
            // cubierto por "bin/wasm/app.wasm" aunque el 7z no tenga
            // entrada de directorio para "bin" (py7zr no las escribe con
            // write()).
            let missing = plan.wanted.iter().any(|w| {
                let ws = w.as_str();
                let prefijo = format!("{ws}/");
                !covered.contains(ws) && !covered.iter().any(|c| c.starts_with(&prefijo))
            });
            if missing {
                // Falta contenido pedido: paquete incompleto/incorrecto.
                return Err(ArcaError::Internal("wanted ausente en el paquete"));
            }
        }
        progress(1.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_selectivo_por_subtree() {
        let plan = ExtractPlan::parse(&["bin", "manifest.toml"]).unwrap();
        let yes = |s: &str| {
            let r = crate::sanitize_entry_path(s).unwrap();
            plan.wants(&r, false)
        };
        assert!(yes("bin"));
        assert!(yes("bin/wasm/app.wasm"));
        assert!(yes("bin/native-aarch64/app"));
        assert!(yes("manifest.toml"));
        assert!(!yes("assets/fonts/x.ttf"));
        assert!(!yes("bins"));
        assert!(!yes("binx/app"));
    }

    #[test]
    fn plan_parse_rechaza_paths_maliciosos() {
        for bad in ["../evil", "/abs", "a\\b"] {
            assert!(ExtractPlan::parse(&[bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn plan_todo_sin_wanted() {
        assert!(ExtractPlan::all().extracts_all());
        assert!(ExtractPlan::default().extracts_all());
    }

    #[test]
    fn progreso_avanza_por_bloques() {
        let mut st = ProgressState {
            done: 0,
            last_cb: 0,
            total: 1024 * 1024,
        };
        // 256 KB exactos → callback.
        assert!(st.advance(PROGRESS_EVERY_BYTES).is_some());
        // Bytes pequeños → sin callback.
        assert!(st.advance(1024).is_none());
        assert!(st.advance(1024).is_none());
        // Alcanzar otro umbral → callback con fracción creciente.
        let f = st.advance(PROGRESS_EVERY_BYTES - 2048).unwrap();
        assert!(f > 0.49 && f < 0.51, "{f}");
    }
}
