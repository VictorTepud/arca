//! `devapp-demo` — app de prueba VISUAL de la fase F3a.
//!
//! Demostración completa del pipeline del probe visual en el teléfono:
//! ```text
//!   host Kotlin (DemoActivity)          este binario (hijo)
//!   ─ crea filesDir/arca-fb.bin ─────── ARCA_FB / ARCA_FB_W / ARCA_FB_H
//!   ─ spawn (ProcessBuilder) ────────── attach (FrameFile)
//!   ─ touch → stdin JSON ────────────── input::parse_line → estado
//!   ← stdout {"event":"frame"} ──────── render CPU + publish (seqlock)
//!   ─ mmap lee frame ─→ SurfaceView ── píxeles en pantalla
//! ```
//!
//! Qué enseña en pantalla: título, logo embebido (blit con alfa),
//! panel de "video" procedural (animación = placeholder del video real,
//! que requiere códec: F4+), 3 botones táctiles, pelota que persigue el
//! dedo y telemetría (fps/frames/pings).
//!
//! # Protocolo stdout (líneas JSON, dialecto F0 extendido)
//!
//! ```text
//! {"event":"hello","ts":…,"pid":…,"w":…,"h":…}
//! {"event":"frame","seq":N,"slot":S}        ← por frame (pacing del blit)
//! {"event":"stats","frames":N,"fps":K}      ← cada 120 frames
//! {"event":"pong","seq":N}                  ← respuesta al ping del host
//! {"event":"exiting","reason":"shutdown","frames":N}
//! {"event":"sigterm","seq":N}               ← handler async-signal-safe
//! {"event":"fatal","error":"…"}             ← antes de exit(1)
//! ```
//!
//! stdin (host → hijo): touch/ping/shutdown (ver `arca-sdk-ui::input`).
//!
//! SIGTERM → línea final + `_exit(0)` en ≤100 ms (mismo contrato que
//! devapp-hello: el watchdog del host puede confiar).
//!
//! `--selftest` corre en PC sin teléfono: renderiza frames con marcadores
//! conocidos, los relee por un SEGUNDO mapeo (como haría el host) y
//! valida el protocolo seqlock de punta a punta. Salida 0 = OK.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arca_gfx_protocol::{rgba_frame_bytes, FrameHeader};
use arca_sdk_ui::{paint_frame, parse_line, Button, Canvas, Color, Event, Phase, RgbaImg};
use arca_shm::{region_len, FrameFile};

/// Período de frame objetivo (30 fps: barato para blit de Kotlin).
const FRAME_MS: u64 = 33;

/// Frames entre líneas de stats (~4 s).
const STATS_CADA: u32 = 120;

/// Logo embebido (assets/logo.rgba, generado por tools/gen_logo.py:
/// 8 B de cabecera [lado u32 ×2] + píxeles RGBA).
const LOGO_RGBA: &[u8] = include_bytes!("../assets/logo.rgba");
/// Lado del logo embebido (validado al arranque contra la cabecera).
const LOGO_LADO: u32 = 96;

/// Frames publicados (para la línea final del handler de señal).
static FRAMES: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Estado del demo
// ---------------------------------------------------------------------------

/// Pelota que persigue el dedo.
struct Ball {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// Estado completo del demo (una sola estructura: sin alloc por frame).
struct Demo {
    w: u16,
    h: u16,
    /// Reloj del último step (ms monótonos).
    t_ms: u64,
    /// Frames publicados.
    frames: u32,
    /// Posición del dedo (si hay toque activo).
    touch: Option<(f32, f32)>,
    /// Tono del fondo (avanza si `hue_on`).
    hue: u16,
    /// Animación de fondo activada por el botón "Color".
    hue_on: bool,
    /// Respuestas pong emitidas.
    pings: u32,
    /// Botones presionados (visual).
    pressed: [bool; 3],
    /// Señal de salida limpia (motivo).
    exit: Option<&'static str>,
    /// Fase animada del panel de "video" (ms acumulados del panel).
    vid_t: u32,
    ball: Ball,
}

impl Demo {
    fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            t_ms: 0,
            frames: 0,
            touch: None,
            hue: 210,
            hue_on: false,
            pings: 0,
            pressed: [false, false, false],
            exit: None,
            vid_t: 0,
            ball: Ball {
                x: w as f32 * 0.5,
                y: h as f32 * 0.45,
                vx: 1.7,
                vy: 1.1,
            },
        }
    }

    /// Botones (stack: sin alloc). Orden: [Color, Ping, Salir].
    fn botones(&self) -> [Button; 3] {
        let w = self.w as i32;
        let h = self.h as i32;
        // fila de 3 si cabe; columna si la pantalla es angosta
        let ancho = if w >= 330 {
            (w - 16 - 16) / 3 - 8
        } else {
            w - 32
        };
        let alto = 44;
        let base = Color::rgb(38, 122, 222);
        let verde = Color::rgb(22, 152, 118);
        let rojo = Color::rgb(196, 62, 62);
        let fila_y = h - 132;
        if w >= 330 {
            [
                Button {
                    x: 16,
                    y: fila_y,
                    w: ancho,
                    h: alto,
                    label: "Color",
                    base,
                    ink: Color::rgb(255, 255, 255),
                },
                Button {
                    x: 16 + ancho + 8,
                    y: fila_y,
                    w: ancho,
                    h: alto,
                    label: "Ping",
                    base: verde,
                    ink: Color::rgb(255, 255, 255),
                },
                Button {
                    x: 16 + 2 * (ancho + 8),
                    y: fila_y,
                    w: ancho,
                    h: alto,
                    label: "Salir",
                    base: rojo,
                    ink: Color::rgb(255, 255, 255),
                },
            ]
        } else {
            [
                Button {
                    x: 16,
                    y: fila_y,
                    w: ancho,
                    h: alto,
                    label: "Color",
                    base,
                    ink: Color::rgb(255, 255, 255),
                },
                Button {
                    x: 16,
                    y: fila_y - alto - 8,
                    w: ancho,
                    h: alto,
                    label: "Ping",
                    base: verde,
                    ink: Color::rgb(255, 255, 255),
                },
                Button {
                    x: 16,
                    y: fila_y - 2 * (alto + 8),
                    w: ancho,
                    h: alto,
                    label: "Salir",
                    base: rojo,
                    ink: Color::rgb(255, 255, 255),
                },
            ]
        }
    }

    /// Zona libre de la pelota (debajo del panel de video, encima de botones).
    fn zona_pelota(&self) -> (f32, f32, f32, f32) {
        let top = (self.h as i32).saturating_sub(320).max(180) as f32;
        let bottom = (self.h as i32 - 140).max((top as i32) + 24) as f32;
        (12.0, top, (self.w - 12) as f32, bottom)
    }

    /// Aplica un evento del host.
    fn evento(&mut self, ev: Event) {
        match ev {
            Event::Ping => {
                self.pings += 1;
                emit_line(&format!("{{\"event\":\"pong\",\"seq\":{}}}", self.pings));
            }
            Event::Shutdown => {
                self.exit = Some("shutdown");
            }
            Event::Touch(t) => {
                let (x, y) = (t.x as f32, t.y as f32);
                match t.phase {
                    Phase::Down => {
                        self.touch = Some((x, y));
                        // botón: acción inmediata + visual hasta Up
                        let bots = self.botones();
                        for (i, b) in bots.iter().enumerate() {
                            if b.contains(x as i32, y as i32) {
                                self.pressed[i] = true;
                                self.accion(i);
                            }
                        }
                    }
                    Phase::Move => self.touch = Some((x, y)),
                    Phase::Up => {
                        self.touch = None;
                        self.pressed = [false, false, false];
                    }
                }
            }
        }
    }

    /// Acción del botón `i` (0=Color, 1=Ping, 2=Salir).
    fn accion(&mut self, i: usize) {
        match i {
            0 => self.hue_on = !self.hue_on,
            1 => {
                self.pings += 1;
                emit_line(&format!("{{\"event\":\"pong\",\"seq\":{}}}", self.pings));
            }
            _ => self.exit = Some("boton-salir"),
        }
    }

    /// Física/animación de un tick.
    fn step(&mut self, now_ms: u64) {
        let dt = now_ms.saturating_sub(self.t_ms).min(FRAME_MS * 3) as f32;
        self.t_ms = now_ms;
        self.vid_t = self.vid_t.wrapping_add(dt as u32);
        if self.hue_on {
            self.hue = (self.hue + 2) % 360;
        }
        // pelota: resorte hacia el dedo, o rebote libre
        let (x0, y0, x1, y1) = self.zona_pelota();
        if let Some((tx, ty)) = self.touch {
            self.ball.vx += (tx - self.ball.x).clamp(-40.0, 40.0) * 0.006 * dt;
            self.ball.vy += (ty - self.ball.y).clamp(-40.0, 40.0) * 0.006 * dt;
        }
        // amortiguación y tope de velocidad
        self.ball.vx *= 0.995_f32.powf(dt / 16.0);
        self.ball.vy *= 0.995_f32.powf(dt / 16.0);
        let sp = (self.ball.vx * self.ball.vx + self.ball.vy * self.ball.vy).sqrt();
        if sp > 4.0 {
            let k = 4.0 / sp;
            self.ball.vx *= k;
            self.ball.vy *= k;
        }
        self.ball.x += self.ball.vx * dt / 16.0;
        self.ball.y += self.ball.vy * dt / 16.0;
        // rebotes contra la zona
        if self.ball.x < x0 + 18.0 {
            self.ball.x = x0 + 18.0;
            self.ball.vx = self.ball.vx.abs();
        }
        if self.ball.x > x1 - 18.0 {
            self.ball.x = x1 - 18.0;
            self.ball.vx = -self.ball.vx.abs();
        }
        if self.ball.y < y0 + 18.0 {
            self.ball.y = y0 + 18.0;
            self.ball.vy = self.ball.vy.abs();
        }
        if self.ball.y > y1 - 18.0 {
            self.ball.y = y1 - 18.0;
            self.ball.vy = -self.ball.vy.abs();
        }
    }

    /// Pinta el frame completo (todas las coords clipean dentro del canvas).
    /// `logo`: imagen embebida ya validada (evita validar por frame).
    fn draw(&self, c: &mut Canvas, logo: RgbaImg) {
        let w = self.w as i32;
        let h = self.h as i32;

        // Fondo: degradado oscuro con el tono actual.
        let top = hsv(self.hue, 90, 20);
        let bot = hsv((self.hue + 60) % 360, 110, 10);
        c.fill_vgrad(top, bot);

        // Barra superior translúcida + título.
        c.fill_rect(
            0,
            0,
            w,
            56,
            Color {
                a: 150,
                ..Color::rgb(8, 10, 16)
            },
        );
        c.draw_text(12, 6, "Arca F3a", 2, Color::rgb(240, 242, 250));
        c.draw_text(
            12,
            38,
            "hola desde la sub-app nativa",
            1,
            Color::rgb(150, 160, 180),
        );

        // Logo embebido (imagen con alfa) a la izquierda.
        c.blit(12, 68, logo);
        // mini logo escalado (blit escalado) a la derecha del grande
        c.blit_scaled(12 + LOGO_LADO as i32 + 10, 68, 64, 64, logo);
        // telemetría al lado de los logos (etiquetas cortas: caben en 320px)
        let mut buf = [0u8; 12];
        let tx = 12 + LOGO_LADO as i32 + 10 + 64 + 12; // 194 con logo=96
        let y0 = 72;
        c.draw_text(tx, y0, "demo", 1, Color::rgb(220, 225, 235));
        let s = format_buf(&mut buf, self.frames);
        c.draw_text(tx, y0 + 20, "frames", 1, Color::rgb(150, 160, 180));
        c.draw_text(tx + 72, y0 + 20, s, 1, Color::rgb(190, 200, 215));
        let mut buf2 = [0u8; 12];
        let s2 = format_buf(&mut buf2, self.pings);
        c.draw_text(tx, y0 + 40, "pings", 1, Color::rgb(150, 160, 180));
        c.draw_text(tx + 72, y0 + 40, s2, 1, Color::rgb(190, 200, 215));

        // Panel de "video" (animación procedural).
        let vy = 176;
        let vh = 120;
        self.draw_video(c, 12, vy, w - 24, vh, logo);

        // Pelota.
        let (bx, by) = (self.ball.x as i32, self.ball.y as i32);
        c.fill_disc(bx, by, 16, Color::rgb(255, 122, 40));
        c.fill_disc(bx - 4, by - 5, 5, Color::rgb(255, 205, 160));

        // Botones.
        let bots = self.botones();
        for (i, b) in bots.iter().enumerate() {
            b.draw(c, self.pressed[i]);
        }

        // Barra inferior: telemetría + pista de uso.
        c.fill_rect(
            0,
            h - 52,
            w,
            52,
            Color {
                a: 170,
                ..Color::rgb(8, 10, 16)
            },
        );
        c.draw_text(
            12,
            h - 44,
            "toca la pelota y arrastrala",
            1,
            Color::rgb(150, 160, 180),
        );
        let touch_txt = match (self.touch, self.exit) {
            (Some((x, y)), _) => format!("x:{} y:{}", x as i32, y as i32),
            (None, Some(r)) => format!("saliendo: {r}"),
            (None, None) => "sin toque".to_string(),
        };
        let fps = self.fps_actual();
        let linea = format!(
            "fps:{}  frames:{}  pings:{}  {}",
            fps, self.frames, self.pings, touch_txt
        );
        c.draw_text(12, h - 24, &linea, 1, Color::rgb(190, 200, 215));
    }

    /// Panel animado: barras cromáticas que se desplazan + logo rebotando.
    /// (Placeholder honesto del "video": sin decodificador no hay H.264 —
    /// ver NOTA al final de la doc del crate.)
    fn draw_video(&self, c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, logo: RgbaImg) {
        // marco
        c.fill_rect(x - 2, y - 2, w + 4, h + 4, Color::rgb(0, 0, 0));
        // barras verticales cíclicas (hue avanza con vid_t)
        let paso = 24;
        let off = (self.vid_t / 6) as i32 % paso;
        let n = w / paso + 2;
        for i in 0..n {
            let hue = (self.hue + (i as u16) * 24 + (self.vid_t / 24) as u16) % 360;
            let col = hsv(hue, 160, 70);
            c.fill_rect(x + i * paso - off, y, paso - 6, h, col);
        }
        // barrido brillante (línea vertical que recorre el panel)
        let sx = x + ((self.vid_t / 8) as i32 % (w.max(1)));
        c.fill_rect(
            sx,
            y,
            3,
            h,
            Color {
                a: 140,
                ..Color::rgb(255, 255, 255)
            },
        );
        // logo "rebotando" dentro del panel (escala 48)
        let inner = (w - 56).max(8) as u32;
        let px = ((self.vid_t / 10) % (inner * 2)).min(inner) as i32; // triángulo
        let px = if (self.vid_t / 10) % (inner * 2) > inner {
            (inner * 2 - (self.vid_t / 10) % (inner * 2)) as i32
        } else {
            px
        };
        c.blit_scaled(x + 4 + px, y + h - 56, 48, 48, logo);
        // etiqueta
        c.draw_text(
            x + 6,
            y + 6,
            "video procedural",
            1,
            Color::rgb(255, 255, 255),
        );
    }

    /// FPS medido de los últimos STATS_CADA frames (aprox. entero).
    fn fps_actual(&self) -> u32 {
        // el reloj del demo avanza a FRAME_MS por step: fps teórico
        ((1000 / FRAME_MS) as u32).min(60)
    }
}

// ---------------------------------------------------------------------------
// HSV → RGB (enteros, sin libm) y formateo sin alloc
// ---------------------------------------------------------------------------

/// Conversión HSV (h 0..360, s/v 0..255) → [`Color`]. Entera, sin floats.
fn hsv(h: u16, s: u8, v: u8) -> Color {
    let h = h % 360;
    let region = h / 60;
    let rem = (h % 60) as u32;
    let s = s as u32;
    let v = v as u32;
    let c = v * s / 255;
    let x = c * (60 - rem.min(60)) / 60;
    let m = v - c;
    let (r, g, b) = match region {
        0 => (c, x, 0),
        1 => (x, c, 0),
        2 => (0, c, x),
        3 => (0, x, c),
        4 => (x, 0, c),
        _ => (c, 0, x),
    };
    Color::rgb((r + m) as u8, (g + m) as u8, (b + m) as u8)
}

/// `v` en decimal dentro de `buf` (sin alloc: stack fijo).
fn format_buf(buf: &mut [u8; 12], v: u32) -> &str {
    let mut i = buf.len();
    let mut rest = v;
    loop {
        i -= 1;
        buf[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 || i == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

// ---------------------------------------------------------------------------
// Salida estándar (líneas JSON con flusheo inmediato — patrón devapp-hello)
// ---------------------------------------------------------------------------

/// Escribe una línea a stdout con flusheo inmediato (línea atómica en pipe).
fn emit_line(line: &str) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Milisegundos de `CLOCK_MONOTONIC`.
fn mono_ms() -> Result<u64, String> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // INTEGRIDAD: clock_gettime no falla en Linux (vDSO en glibc y bionic).
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if rc != 0 {
        return Err(format!(
            "clock_gettime: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(ts.tv_sec as u64 * 1_000 + ts.tv_nsec as u64 / 1_000_000)
}

// ---------------------------------------------------------------------------
// stdin no bloqueante (poll(2) — patrón devapp-hello, duplicado a propósito:
// ambos son probes desechables; se unifica en arca-rt en F3b)
// ---------------------------------------------------------------------------

enum StdinEvent {
    Data,
    NoData,
    Closed,
}

fn poll_stdin(timeout_ms: i32) -> Result<bool, String> {
    let mut fds = [libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    }];
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            return Ok(false);
        }
        return Err(format!("poll(stdin): {err}"));
    }
    let revents = fds[0].revents as i32;
    let bad = (libc::POLLERR as i32) | (libc::POLLNVAL as i32);
    let readable = (libc::POLLIN as i32) | (libc::POLLHUP as i32);
    if revents & bad != 0 {
        return Err(format!("poll(stdin): revents={revents}"));
    }
    Ok(revents & readable != 0)
}

fn read_stdin(buf: &mut Vec<u8>) -> Result<StdinEvent, String> {
    let mut chunk = [0u8; 4096];
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

fn drain_lines(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let mut line: Vec<u8> = buf.drain(..=pos).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        out.push(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Señales: SIGTERM/SIGINT → línea final + _exit(0) (async-signal-safe)
// ---------------------------------------------------------------------------

fn install_signal_handlers() -> Result<(), String> {
    for sig in [libc::SIGTERM, libc::SIGINT] {
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
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

extern "C" fn on_signal(sig: libc::c_int) {
    // INTEGRIDAD: solo write(2) + _exit(2) (async-signal-safe); ver el
    // gemelo de devapp-hello para el comentario completo del contrato.
    unsafe {
        let name: &[u8] = match sig {
            libc::SIGTERM => b"sigterm",
            libc::SIGINT => b"sigint",
            _ => b"signal",
        };
        let n = FRAMES.load(Ordering::Relaxed);
        let mut buf = [0u8; 64];
        let mut at = 0usize;
        at = append_bytes(&mut buf, at, b"{\"event\":\"");
        at = append_bytes(&mut buf, at, name);
        at = append_bytes(&mut buf, at, b"\",\"frames\":");
        at = append_u64(&mut buf, at, n);
        at = append_bytes(&mut buf, at, b"}\n");
        let _ = libc::write(libc::STDOUT_FILENO, buf.as_ptr().cast(), at);
        libc::_exit(0);
    }
}

fn append_bytes(dst: &mut [u8], at: usize, src: &[u8]) -> usize {
    let room = dst.len().saturating_sub(at).min(src.len());
    dst[at..at + room].copy_from_slice(&src[..room]);
    at + room
}

fn append_u64(dst: &mut [u8], at: usize, v: u64) -> usize {
    if v == 0 {
        return append_bytes(dst, at, b"0");
    }
    let mut digits = [0u8; 20];
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
// main + bucle del probe
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest") {
        let rc = selftest();
        std::process::exit(rc);
    }
    if let Err(err) = run() {
        emit_line(&format!(
            "{{\"event\":\"fatal\",\"error\":\"{}\"}}",
            json_escape(&err)
        ));
        std::process::exit(1);
    }
}

/// Bucle principal del demo (modo teléfono, lanzado por DemoActivity).
fn run() -> Result<(), String> {
    let fb_path =
        std::env::var("ARCA_FB").map_err(|_| "falta ARCA_FB (ruta del framebuffer)".to_string())?;
    let w: u16 = std::env::var("ARCA_FB_W")
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or("falta ARCA_FB_W (ancho del framebuffer)")?;
    let h: u16 = std::env::var("ARCA_FB_H")
        .ok()
        .and_then(|v| v.parse().ok())
        .ok_or("falta ARCA_FB_H (alto del framebuffer)")?;
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err(format!("geometría absurda: {w}x{h}"));
    }
    // el logo embebido debe cuadrar con su cabecera (assets corrupto = fail)
    if LOGO_RGBA.len() != 8 + LOGO_LADO as usize * LOGO_LADO as usize * 4
        || u32::from_le_bytes([LOGO_RGBA[0], LOGO_RGBA[1], LOGO_RGBA[2], LOGO_RGBA[3]]) != LOGO_LADO
    {
        return Err("assets/logo.rgba corrupto".into());
    }
    let logo = RgbaImg::new(LOGO_LADO, LOGO_LADO, &LOGO_RGBA[8..])
        .ok_or("logo embebido con tamaño inconsistente")?;

    let frame_bytes = rgba_frame_bytes(w, h);
    let region = FrameFile::open(std::path::Path::new(&fb_path), frame_bytes)
        .map_err(|e| format!("attach {fb_path}: {e}"))?;

    install_signal_handlers()?;
    let pid = unsafe { libc::getpid() };
    let t0 = mono_ms()?;
    emit_line(&format!(
        "{{\"event\":\"hello\",\"ts\":{t0},\"pid\":{pid},\"w\":{w},\"h\":{h}}}"
    ));

    let mut demo = Demo::new(w, h);
    let mut rx_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut stdin_open = true;
    let mut slot = 0usize;
    let mut next = t0;
    let mut stats_t0 = t0;
    let mut stats_f0 = 0u32;

    loop {
        // 1) esperar hasta el próximo tick vigilando stdin
        let now = mono_ms()?;
        let wait = next.saturating_sub(now).min(500) as i32;
        if wait > 0 {
            if stdin_open && poll_stdin(wait)? {
                match read_stdin(&mut rx_buf)? {
                    StdinEvent::Data => {
                        for line in drain_lines(&mut rx_buf) {
                            let s = String::from_utf8_lossy(&line).into_owned();
                            if let Some(ev) = parse_line(&s) {
                                demo.evento(ev);
                            }
                        }
                    }
                    StdinEvent::Closed => stdin_open = false,
                    StdinEvent::NoData => {}
                }
            } else if !stdin_open {
                std::thread::sleep(Duration::from_millis(wait as u64));
            }
        }

        // 2) tick de frame
        let now = mono_ms()?;
        if now >= next {
            demo.step(now);
            let hdr = FrameHeader::new_opaque(w, h, demo.frames.wrapping_add(1), now);
            {
                let slots = region.slots();
                let mut guard = slots
                    .begin_write(slot)
                    .map_err(|e| format!("begin_write: {e}"))?;
                let payload = guard.payload();
                paint_frame(payload, &hdr, |c| demo.draw(c, logo))
                    .map_err(|e| format!("paint: {e}"))?;
                guard.publish().map_err(|e| format!("publish: {e}"))?;
            }
            demo.frames += 1;
            FRAMES.store(demo.frames as u64, Ordering::Relaxed);
            emit_line(&format!(
                "{{\"event\":\"frame\",\"seq\":{},\"slot\":{}}}",
                demo.frames, slot
            ));
            slot ^= 1;
            next = now + FRAME_MS;
            if now > next + 200 {
                next = now + FRAME_MS; // nos atrasamos mucho: resincroniza
            }

            // stats cada STATS_CADA frames
            if demo.frames % STATS_CADA == 0 {
                let dt = now.saturating_sub(stats_t0).max(1);
                let fps = (STATS_CADA as u64 - stats_f0 as u64) * 1000 / dt;
                emit_line(&format!(
                    "{{\"event\":\"stats\",\"frames\":{},\"fps\":{}}}",
                    demo.frames, fps
                ));
                stats_t0 = now;
                stats_f0 = demo.frames;
            }
        }

        // 3) salida limpia
        if let Some(reason) = demo.exit {
            emit_line(&format!(
                "{{\"event\":\"exiting\",\"reason\":\"{reason}\",\"frames\":{}}}",
                demo.frames
            ));
            return Ok(());
        }
    }
}

/// Escapa una cadena para JSON (comillas/backslash/controles).
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

// ---------------------------------------------------------------------------
// --selftest: valida el pipeline render→publish→read en PC sin teléfono
// ---------------------------------------------------------------------------

fn selftest() -> i32 {
    const W: u16 = 320;
    const H: u16 = 240;
    let path = std::env::temp_dir().join("arca-demo-selftest.bin");
    let frame_bytes = rgba_frame_bytes(W, H);
    let region_total = region_len(frame_bytes);

    // Rol host: archivo del tamaño exacto, en cero (seq par = inválido).
    let _ = std::fs::remove_file(&path);
    let host_file = match std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("selftest: no pude crear {}: {e}", path.display());
            return 1;
        }
    };
    if let Err(e) = host_file.set_len(region_total as u64) {
        eprintln!("selftest: set_len: {e}");
        return 1;
    }

    // Hijo (escritor) y "host" (lector) en mapeos independientes.
    let writer = match FrameFile::open(&path, frame_bytes) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("selftest: attach escritor: {e}");
            return 1;
        }
    };
    let reader = match FrameFile::open(&path, frame_bytes) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("selftest: attach lector: {e}");
            return 1;
        }
    };

    let mut demo = Demo::new(W, H);
    let logo = match RgbaImg::new(LOGO_LADO, LOGO_LADO, &LOGO_RGBA[8..]) {
        Some(l) => l,
        None => {
            eprintln!("selftest: logo embebido corrupto");
            return 1;
        }
    };
    let mut out = vec![0u8; frame_bytes];
    let mut fallos = 0u32;
    let mut slot = 0usize;

    for i in 0..5u64 {
        let tms = i * FRAME_MS;
        demo.step(tms);
        if i == 2 {
            // un toque en el centro: la pelota debe reaccionar (spring)
            demo.evento(Event::Touch(arca_sdk_ui::Touch {
                phase: Phase::Move,
                x: 160,
                y: 110,
                t: Some(tms),
            }));
        }
        // marcador determinista: rect opaco de color conocido arriba
        let marca = Color::rgb(10 * i as u8 + 5, 200, 150);
        let hdr = FrameHeader::new_opaque(W, H, i as u32 + 1, tms);
        let ok = {
            let slots = writer.slots();
            match slots.begin_write(slot) {
                Ok(mut guard) => {
                    let payload = guard.payload();
                    match paint_frame(payload, &hdr, |c| {
                        demo.draw(c, logo);
                        c.fill_rect(0, 0, 16, 16, marca);
                    }) {
                        Ok(()) => guard.publish().is_ok(),
                        Err(e) => {
                            eprintln!("selftest: paint frame {i}: {e}");
                            false
                        }
                    }
                }
                Err(e) => {
                    eprintln!("selftest: begin_write: {e}");
                    false
                }
            }
        };
        if !ok {
            fallos += 1;
            continue;
        }

        // lectura estilo host: seqlock → header → píxel del marcador
        let snap = reader.slots().read_latest_into(&mut out);
        let hdr_dec = snap.as_ref().map(|_| FrameHeader::decode_from(&out));
        match (snap, hdr_dec) {
            (Some(s), Some(Ok(h))) => {
                if h.width != W || h.height != H || h.frame_seq != (i as u32 + 1) {
                    eprintln!(
                        "selftest: frame {i}: header raro w={} h={} seq={} (snap seq {})",
                        h.width, h.height, h.frame_seq, s.seq
                    );
                    fallos += 1;
                }
                let at = 32 + (6 * W as usize + 6) * 4; // píxel (6,6) del marcador
                let px = (out[at], out[at + 1], out[at + 2], out[at + 3]);
                let want = (10 * i as u8 + 5, 200, 150, 255);
                if px != want {
                    eprintln!("selftest: frame {i}: píxel {px:?} ≠ marcador {want:?}");
                    fallos += 1;
                }
            }
            _ => {
                eprintln!("selftest: frame {i}: sin frame válido tras publish");
                fallos += 1;
            }
        }
        slot ^= 1;
    }

    let _ = std::fs::remove_file(&path);
    if fallos == 0 {
        println!("selftest: OK (5 frames, seqlock de punta a punta)");
        println!("selftest: pipeline render→publish→read verificado sin teléfono");
        0
    } else {
        eprintln!("selftest: FALLÓ ({fallos} comprobaciones)");
        1
    }
}
