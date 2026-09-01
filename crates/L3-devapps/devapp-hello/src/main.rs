//! `devapp-hello` — binario de probe de viabilidad de la fase F0 (gate GO/NO-GO).
//!
//! Demuestra en un dispositivo real la **grieta de Termux** (blueprint
//! `docs/01-restricciones-android.md` §2): un APK con `targetSdk 28` extrae
//! este binario a su `filesDir` (`/data/data/<pkg>/files`) y lo ejecuta con
//! `fork`+`exec`. Si el proceso hijo emite heartbeats por stdout, la ruta A
//! (procesos ELF nativos — backend principal de Arca) es viable en ese
//! dispositivo y el proyecto sigue el roadmap tal cual; si el `execve` falla
//! con `EACCES`/`EPERM`, hay que pivotar al backend WASM (docs/12, fase F5).
//!
//! # Protocolo de stdout (una línea JSON compacta por evento, flusheo inmediato)
//!
//! ```text
//! {"event":"hello","ts":123456,"pid":1234,"uid":10245,"gid":10245,"cwd":"/data/user/0/dev.arca.probe/files","argv0":"/data/user/0/dev.arca.probe/files/devapp-hello"}
//! {"ts":123956,"pid":1234,"seq":1}
//! {"event":"pong","seq":1}
//! {"event":"sigterm","seq":6}
//! ```
//!
//! - `ts` = milisegundos de `CLOCK_MONOTONIC` (desde el arranque del sistema;
//!   correlacionable con el uptime del dispositivo).
//! - Heartbeat cada 500 ms (`{"ts":…,"pid":…,"seq":…}`).
//! - `SIGTERM`/`SIGINT` → línea final `{"event":"sigterm","seq":…}` + `_exit(0)`
//!   en ≤ 100 ms: el handler es async-signal-safe (ver [`on_signal`]).
//! - stdin **opcional y no bloqueante** (vía `poll(2)`): una línea `ping`
//!   responde `{"event":"pong",…}` (pre-simulación mínima de AIPC). El EOF no
//!   termina el proceso — solo deja de vigilarse el fd (evita un poll-busy).
//!
//! # Cómo probarlo en PC / Android
//!
//! Ver `README.md` del crate: en PC `timeout 3 ./devapp-hello`; en Android lo
//! lanza el APK `host-probe/` (Kotlin, targetSdk 28) y su stdout se ve en
//! `adb logcat -s ArcaProbe`.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::sleep;
use std::time::Duration;

/// Período del heartbeat, en ms (contrato de la tarea T02).
const HEARTBEAT_MS: u64 = 500;

/// Tamaño de lectura por turno de stdin (líneas de control de AIPC serán cortas).
const STDIN_CHUNK: usize = 4096;

/// Último `seq` de heartbeat emitido. Lo escribe el bucle principal y lo lee
/// el handler de señal para componer la línea final.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn main() {
    if let Err(err) = run() {
        // Último recurso: diagnóstico por stderr (stdout puede estar roto).
        let line = format!(
            "{{\"event\":\"fatal\",\"error\":\"{}\"}}",
            json_escape(&err)
        );
        let _ = writeln!(std::io::stderr(), "{line}");
        std::process::exit(1);
    }
}

/// Bucle principal del probe: heartbeat cada 500 ms + stdin no bloqueante.
fn run() -> Result<(), String> {
    install_signal_handlers()?;

    // INVARIANTE: getpid() no puede fallar; el pid no cambia durante la vida
    // del proceso (no hacemos fork).
    let pid = unsafe { libc::getpid() };
    emit_line(&hello_line(pid)?)?;

    let mut next_beat = mono_ms()?;
    let mut seq: u64 = 0;
    let mut rx_buf: Vec<u8> = Vec::with_capacity(STDIN_CHUNK);
    let mut stdin_open = true;

    loop {
        let now = mono_ms()?;
        if now >= next_beat {
            seq += 1;
            SEQ.store(seq, Ordering::Relaxed);
            emit_line(&heartbeat_line(now, pid, seq))?;
            next_beat = now + HEARTBEAT_MS;
        }

        // Esperamos como máximo hasta el próximo heartbeat; el handler de
        // SIGTERM/SIGINT puede cortar la espera en cualquier momento.
        let wait_ms = (next_beat - now).min(i32::MAX as u64) as i32;
        if !stdin_open {
            sleep(Duration::from_millis(wait_ms as u64));
            continue;
        }

        if poll_stdin(wait_ms)? {
            match read_stdin(&mut rx_buf)? {
                StdinEvent::Data => {
                    for line in drain_lines(&mut rx_buf) {
                        if line == b"ping" {
                            // Pre-simulación mínima de AIPC: eco pong con el
                            // seq del último heartbeat.
                            emit_line(&pong_line(seq))?;
                        }
                        // Líneas desconocidas: se ignoran (protocolo aún sin definir).
                    }
                }
                StdinEvent::Closed => {
                    // EOF (host cerró su lado, o stdin nunca fue un pipe):
                    // el probe sigue vivo; solo dejamos de vigilar el fd.
                    stdin_open = false;
                }
                StdinEvent::NoData => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Líneas del protocolo (JSON compacto, construidas a mano: sin deps de serde
// — este binario debe cross-compilarse estático con la mínima superficie).
// ---------------------------------------------------------------------------

/// Línea de heartbeat: `{"ts":<ms mono>,"pid":<pid>,"seq":<n>}`.
fn heartbeat_line(ts_ms: u64, pid: libc::pid_t, seq: u64) -> String {
    format!("{{\"ts\":{ts_ms},\"pid\":{pid},\"seq\":{seq}}}")
}

/// Respuesta pong (pre-simulación de AIPC): `{"event":"pong","seq":<n>}`.
fn pong_line(seq: u64) -> String {
    format!("{{\"event\":\"pong\",\"seq\":{seq}}}")
}

/// Línea de arranque: identidad del proceso (`uid`, `gid`, `cwd`, `argv0`).
fn hello_line(pid: libc::pid_t) -> Result<String, String> {
    let ts = mono_ms()?;
    // INVARIANTE: getuid()/getgid() son syscalls que no pueden fallar.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let argv0 = std::env::args().next();
    let cwd_json = json_opt_str(cwd.as_deref());
    let argv0_json = json_opt_str(argv0.as_deref());
    Ok(format!(
        "{{\"event\":\"hello\",\"ts\":{ts},\"pid\":{pid},\"uid\":{uid},\"gid\":{gid},\
         \"cwd\":{cwd_json},\"argv0\":{argv0_json}}}"
    ))
}

/// Escribe una línea a stdout con flusheo inmediato.
///
/// INTEGRIDAD (invariante del probe): cada línea sale con una única llamada a
/// `write(2)` (stdout es un `LineWriter` y nuestras líneas son ≪ `PIPE_BUF`),
/// por lo que la escritura es atómica en pipes y el handler de señal nunca
/// puede intercalar su línea final a mitad de una línea del bucle principal.
fn emit_line(line: &str) -> Result<(), String> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("stdout: {e}"))
}

/// Escapa un `&str` para incrustarlo en JSON (comillas, backslash, controles).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Codifica una `Option` como JSON: `null` o `"cadena escapada"`.
fn json_opt_str(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{}\"", json_escape(s)),
        None => "null".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Señales: SIGTERM/SIGINT → línea final + _exit(0) en ≤ 100 ms.
// ---------------------------------------------------------------------------

/// Registra el handler de [`on_signal`] para `SIGTERM` y `SIGINT`.
///
/// `SIGINT` además de `SIGTERM` para poder probar en PC con Ctrl+C.
/// `SIGPIPE` ya lo ignora la runtime de Rust (si el host cierra el pipe de
/// stdout, el bucle principal lo reporta como error en vez de morir callado).
fn install_signal_handlers() -> Result<(), String> {
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // INTEGRIDAD: sigaction(2) con máscara bloqueada vacía y flags a 0.
        // `zeroed()` deja `sa_mask` a cero = conjunto vacío en glibc/bionic,
        // y evita depender del tipo exacto de `sigset_t` por plataforma.
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            // Doble cast puntero→usize: así lo exige el lint `function_casts_as_integer`.
            act.sa_sigaction = on_signal as *const () as usize;
            act.sa_flags = 0;
            if libc::sigaction(sig, &act, std::ptr::null_mut()) != 0 {
                return Err(format!(
                    "sigaction({sig}): {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

/// Handler de `SIGTERM`/`SIGINT`: imprime la línea final y termina limpio.
///
/// NOTA(agent T02): el handler hace el trabajo él mismo (`write(2)` + `_exit`)
/// en vez de solo marcar un flag, porque el bucle principal puede estar
/// dormido hasta 500 ms y la tarea exige salir en ≤ 100 ms.
extern "C" fn on_signal(sig: libc::c_int) {
    // INTEGRIDAD (async-signal-safety):
    // 1. Solo se usan primitivas async-signal-safe: `write(2)` y `_exit(2)`.
    // 2. El buffer es de pila y el número se formatea a mano: cero `malloc`,
    //    cero stdio, cero locks (ver `append_bytes`/`append_u64`).
    // 3. `SEQ` se lee con `Ordering::Relaxed`: es informativo ("último
    //    heartbeat visto") y no participa de ninguna otra sincronización.
    // 4. Todo stdout previo ya se flusheó línea a línea (invariante de
    //    `emit_line`), así que `_exit(0)` no descarta datos pendientes.
    // 5. La línea del handler y las del bucle son escrituras `write(2)`
    //    únicas y < PIPE_BUF → no hay intercalado parcial.
    unsafe {
        let name: &[u8] = match sig {
            libc::SIGTERM => b"sigterm",
            libc::SIGINT => b"sigint",
            _ => b"signal",
        };
        let seq = SEQ.load(Ordering::Relaxed);
        let mut buf = [0u8; 64];
        let mut at = 0usize;
        at = append_bytes(&mut buf, at, b"{\"event\":\"");
        at = append_bytes(&mut buf, at, name);
        at = append_bytes(&mut buf, at, b"\",\"seq\":");
        at = append_u64(&mut buf, at, seq);
        at = append_bytes(&mut buf, at, b"}\n");
        // Si write() fallara (p. ej. stdout ya cerrado) no hay recuperación
        // posible dentro de un handler: salimos igualmente con 0 (señal limpia).
        let _ = libc::write(libc::STDOUT_FILENO, buf.as_ptr().cast(), at);
        libc::_exit(0);
    }
}

/// Copia `src` en `dst[at..]` **sin alocar** (async-signal-safe). Devuelve el
/// nuevo offset; si no cabe, trunca (invariante: 64 bytes de buffer y línea
/// final de ≤ 47 bytes hacen el caso inalcanzable).
fn append_bytes(dst: &mut [u8], at: usize, src: &[u8]) -> usize {
    let room = dst.len().saturating_sub(at).min(src.len());
    dst[at..at + room].copy_from_slice(&src[..room]);
    at + room
}

/// Formatea `v` en decimal en `dst[at..]` **sin alocar** (async-signal-safe).
fn append_u64(dst: &mut [u8], at: usize, v: u64) -> usize {
    if v == 0 {
        return append_bytes(dst, at, b"0");
    }
    let mut digits = [0u8; 20]; // u64::MAX ocupa 20 dígitos
    let mut i = digits.len();
    let mut rest = v;
    while rest > 0 {
        i -= 1;
        digits[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
    append_bytes(dst, at, &digits[i..])
}

// ---------------------------------------------------------------------------
// stdin no bloqueante: poll(2) + read(2) crudos (compatible con el futuro
// AIPC: el host nos mandará líneas de control por este pipe).
// ---------------------------------------------------------------------------

/// Resultado de un turno de lectura de stdin.
enum StdinEvent {
    /// Llegaron bytes (ya acumulados en el buffer de líneas).
    Data,
    /// Llegaron bytes pero el turno fue interrumpido (EINTR/EAGAIN): nada nuevo.
    NoData,
    /// EOF: no hay más stdin.
    Closed,
}

/// Espera hasta `timeout_ms` a que stdin tenga datos. `Ok(true)` = legible.
fn poll_stdin(timeout_ms: i32) -> Result<bool, String> {
    let mut fds = [libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    }];
    // INTEGRIDAD: poll(2) sobre fd 0. Puede volver EINTR (el handler de señal
    // habría terminado el proceso antes, así que aquí es benigno). POLLHUP se
    // trata como "legible": el read() posterior devolverá 0 (EOF) y el bucle
    // desactivará el fd sin girar en busy-loop.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            return Ok(false);
        }
        return Err(format!("poll(stdin): {err}"));
    }
    // El tipo de los flags de poll difiere entre plataformas (i16/i32):
    // normalizamos a i32 antes de combinarlos.
    let revents = fds[0].revents as i32;
    let bad = (libc::POLLERR as i32) | (libc::POLLNVAL as i32);
    let readable = (libc::POLLIN as i32) | (libc::POLLHUP as i32);
    if revents & bad != 0 {
        return Err(format!("poll(stdin): revents={revents}"));
    }
    Ok(revents & readable != 0)
}

/// Lee un turno de stdin al acumulador `buf` (solo llamar tras POLLIN).
fn read_stdin(buf: &mut Vec<u8>) -> Result<StdinEvent, String> {
    let mut chunk = [0u8; STDIN_CHUNK];
    // INTEGRIDAD: read(2) sobre fd 0 inmediatamente después de un POLLIN
    // positivo — hay al menos un byte disponible, por lo que no bloquea.
    let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
    if n > 0 {
        buf.extend_from_slice(&chunk[..n as usize]);
        Ok(StdinEvent::Data)
    } else if n == 0 {
        Ok(StdinEvent::Closed)
    } else {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) | Some(libc::EAGAIN) => Ok(StdinEvent::NoData),
            _ => Err(format!("read(stdin): {err}")),
        }
    }
}

/// Extrae del acumulador las líneas completas (separadas por `\n`, sin `\r`
/// final) y deja el resto parcial en `buf`.
fn drain_lines(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=pos).collect();
        line.pop(); // '\n'
        if line.last() == Some(&b'\r') {
            line.pop(); // '\r' de finales CRLF
        }
        out.push(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Reloj monótono.
// ---------------------------------------------------------------------------

/// Milisegundos de `CLOCK_MONOTONIC` (desde el arranque del sistema).
fn mono_ms() -> Result<u64, String> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // INTEGRIDAD: clock_gettime(CLOCK_MONOTONIC) no falla en Linux (glibc y
    // bionic lo resuelven por vDSO); se comprueba el retorno por robustez.
    unsafe {
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) != 0 {
            return Err(format!(
                "clock_gettime: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(ts.tv_sec as u64 * 1_000 + ts.tv_nsec as u64 / 1_000_000)
}

// ---------------------------------------------------------------------------
// Tests del protocolo (las piezas triviales, pero verificables).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_es_json_compacto() {
        assert_eq!(
            heartbeat_line(1000, 42, 3),
            "{\"ts\":1000,\"pid\":42,\"seq\":3}"
        );
    }

    #[test]
    fn pong_lleva_seq() {
        assert_eq!(pong_line(7), "{\"event\":\"pong\",\"seq\":7}");
    }

    #[test]
    fn json_escape_cubre_comillas_backslash_y_controles() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
        assert_eq!(json_escape(" plano "), " plano ");
    }

    #[test]
    fn json_opt_str_null_o_cadena_escapada() {
        assert_eq!(json_opt_str(None), "null");
        assert_eq!(json_opt_str(Some("x\"y")), "\"x\\\"y\"");
    }

    #[test]
    fn drain_lines_extrae_completas_y_deja_parcial() {
        let mut buf = b"ping\npin".to_vec();
        let lines = drain_lines(&mut buf);
        assert_eq!(lines, vec![b"ping".to_vec()]);
        assert_eq!(buf, b"pin");

        let mut buf = b"a\r\nb\n\n".to_vec();
        let lines = drain_lines(&mut buf);
        assert_eq!(lines, vec![b"a".to_vec(), b"b".to_vec(), Vec::new()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn append_u64_formatea_decimal_sin_alloc() {
        let mut b = [0u8; 24];
        let at = append_u64(&mut b, 0, 0);
        assert_eq!(&b[..at], b"0");

        let at = append_u64(&mut b, 0, u64::MAX);
        assert_eq!(&b[..at], b"18446744073709551615");

        // Tras un prefijo respeta el offset.
        let mut b = [0u8; 24];
        let at = append_bytes(&mut b, 0, b"x=");
        let at = append_u64(&mut b, at, 17);
        assert_eq!(&b[..at], b"x=17");
    }
}
