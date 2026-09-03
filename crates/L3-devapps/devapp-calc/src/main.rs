//! `devapp-calc` — calculadora completa de la fase F3a (sub-app nativa).
//!
//! Primer ejemplo "de verdad" del contenedor: mismas reglas que
//! `devapp-demo` (ELF estático-PIE sin NDK, framebuffer compartido con
//! seqlock, stdio JSON) pero con un motor de cálculo propio:
//!
//! ```text
//!   host Kotlin (DemoActivity)          este binario (hijo)
//!   ─ crea filesDir/arca-fb.bin ─────── ARCA_FB / ARCA_FB_W / ARCA_FB_H
//!   ─ spawn (ProcessBuilder) ────────── attach (FrameFile)
//!   ─ touch → stdin JSON ────────────── input::parse_line → tecla
//!   ← stdout {"event":"frame"} ──────── render CPU + publish (seqlock)
//!   ─ mmap lee frame ─→ SurfaceView ── píxeles en pantalla
//! ```
//!
//! # Qué calcula
//!
//! Aritmética **decimal exacta** (mantisa `i64` × escala, intermedios en
//! `i128` — ver `calc.rs`): `0.1+0.2 = 0.3` sin basura binaria.
//! Precedencia de operadores (× ÷ antes de + −), porcentaje, signo,
//! borrar carácter, corrección de operador apilado y encadenado desde el
//! resultado. División por cero y desborde → "Error" (recuperable con C).
//!
//! # Layout (canvas de diseño 336×720, escala `ui` por AMBAS dimensiones)
//!
//! ```text
//!  0..56    barra de título "Calculadora" + X de cierre (r10)
//! 68..200   panel display: eco de expresión / entrada-resultado /
//!           historial ("12+3*4=24")
//! 208..     grid 4×5: C % < / · 7 8 9 * · 4 5 6 - · 1 2 3 + ·
//!           +/- 0 . =      (anclado al fondo, sobre la barra inferior)
//! h-52..    barra inferior: pista de uso + telemetría fps/frames
//! ```
//!
//! # Protocolo stdout (idéntico al demo — dialecto F0 extendido)
//!
//! ```text
//! {"event":"hello","ts":…,"pid":…,"w":…,"h":…}
//! {"event":"frame","seq":N,"slot":S}        ← por frame (pacing del blit)
//! {"event":"stats","frames":N,"fps":K}      ← cada 120 frames
//! {"event":"pong","seq":N}                  ← respuesta al ping del host
//! {"event":"exiting","reason":"shutdown|…","frames":N}
//! {"event":"sigterm","seq":N}               ← handler async-signal-safe
//! {"event":"fatal","error":"…"}             ← antes de exit(1)
//! ```
//!
//! stdin (host → hijo): touch/ping/shutdown (ver `arca-sdk-ui::input`).
//! SIGTERM → línea final + `_exit(0)` en ≤100 ms (contrato devapp-hello).
//!
//! `--selftest` corre en PC sin teléfono: valida el pipeline seqlock de
//! punta a punta Y recorre escenarios de cálculo (precedencia, error,
//! encadenado). Salida 0 = OK.

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arca_gfx_protocol::{rgba_frame_bytes, FrameHeader};
use arca_sdk_ui::{paint_frame, parse_line, Button, Canvas, Color, Event, Phase, Touch};
use arca_shm::{region_len, FrameFile};

mod calc;

use calc::{eval, Dec, EcoNum, Entrada, FMT_BUF, MAX_NUMS, MAX_OPS};

/// Período de frame objetivo (30 fps: barato para blit de Kotlin).
const FRAME_MS: u64 = 33;

/// Frames entre líneas de stats (~4 s).
const STATS_CADA: u32 = 120;

/// Botones del grid (4 columnas × 5 filas).
const N_BOTONES: usize = 20;

/// Etiquetas del grid, por índice (orden de lectura: filas de 4).
const ETIQUETAS: [&str; N_BOTONES] = [
    "C", "%", "<", "/", "7", "8", "9", "*", "4", "5", "6", "-", "1", "2", "3", "+", "+/-", "0",
    ".", "=",
];

/// Capacidad del eco de expresión / historial (clipea entradas gigantes).
const ECO_CAP: usize = 44;

/// Frames publicados (para la línea final del handler de señal).
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Tecla lógica de la calculadora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tecla {
    /// Dígito 0..9.
    Digito(u8),
    /// Punto decimal.
    Punto,
    /// Operador binario: `+ - * /` (bytes ASCII).
    Op(u8),
    /// Igual: evalúa.
    Igual,
    /// Limpieza total.
    C,
    /// Borrar último carácter.
    Borrar,
    /// Alternar signo.
    Signo,
    /// Porcentaje (x/100).
    Pct,
}

/// Estado completo de la calculadora (una sola estructura: sin alloc por
/// frame ni por tecla — todos los buffers son de capacidad fija).
struct Calc {
    /// Ancho del framebuffer.
    w: u16,
    /// Alto del framebuffer.
    h: u16,
    /// Escala de UI contra el canvas de diseño 336×720 (r13).
    ui: u32,
    /// Frames publicados.
    frames: u32,
    /// FPS medido del último stats.
    last_fps: u32,
    /// Respuestas pong emitidas.
    pings: u32,
    /// Botones presionados (visual).
    pressed: [bool; N_BOTONES],
    /// Señal de salida limpia (motivo).
    exit: Option<&'static str>,
    // ── motor de cálculo ──
    /// Números apilados de la expresión (n+1 números para n operadores).
    nums: [EcoNum; MAX_NUMS],
    /// Operadores apilados.
    ops: [u8; MAX_OPS],
    /// Cantidad de operadores apilados.
    n_ops: usize,
    /// Número en edición (línea principal).
    ent: Entrada,
    /// El usuario editó la entrada desde el último operador (distingue
    /// "apilar número" de "corregir operador").
    ent_tipeada: bool,
    /// La entrada muestra un resultado (dígito reinicia, operador encadena).
    en_resultado: bool,
    /// Error matemático en pantalla (solo C o un dígito reviven).
    err: bool,
    /// La expresión está completa (el último número de `nums` pertenece a
    /// ella): solo tras congelar en '='; el eco lo muestra entero.
    expr_completa: bool,
    /// Historial "12+3*4=24" (línea del panel).
    hist: [u8; ECO_CAP],
    /// Bytes válidos del historial.
    hist_len: usize,
}

impl Calc {
    fn new(w: u16, h: u16) -> Self {
        let mut c = Calc {
            w,
            h,
            ui: ui_scale(w, h),
            frames: 0,
            last_fps: (1000 / FRAME_MS) as u32,
            pings: 0,
            pressed: [false; N_BOTONES],
            exit: None,
            nums: [EcoNum::de_dec(Dec::ZERO); MAX_NUMS],
            ops: [b'+'; MAX_OPS],
            n_ops: 0,
            ent: Entrada::nueva(),
            ent_tipeada: false,
            en_resultado: false,
            err: false,
            expr_completa: false,
            hist: [0u8; ECO_CAP],
            hist_len: 0,
        };
        c.reset();
        c
    }

    /// Limpieza total: expresión, entrada, error e historial.
    fn reset(&mut self) {
        self.nums[0] = EcoNum::de_dec(Dec::ZERO);
        self.n_ops = 0;
        self.ent = Entrada::nueva();
        self.ent_tipeada = false;
        self.en_resultado = false;
        self.err = false;
        self.expr_completa = false;
        self.hist_len = 0;
    }

    /// Expresión nueva manteniendo el historial (tras resultado o dígito
    /// que reinicia).
    fn nueva_expr(&mut self) {
        self.nums[0] = EcoNum::de_dec(Dec::ZERO);
        self.n_ops = 0;
        self.ent = Entrada::nueva();
        self.ent_tipeada = false;
        self.en_resultado = false;
        self.err = false;
        self.expr_completa = false;
    }

    /// Geometría del grid: (x0, y0, ancho_bot, alto_bot, gap).
    ///
    /// Anclado al fondo (encima de la barra inferior de 52·ui) y con piso
    /// de 16 px por botón: en fbs degenerados (qemu 160×360) el grid puede
    /// pisar la barra, que se pinta DESPUÉS y lo tapa limpio.
    fn grid(&self) -> (i32, i32, i32, i32, i32) {
        let ui = self.ui as i32;
        let m = 8 * ui;
        let gap = 8 * ui;
        let bw = ((self.w as i32 - 2 * m - 3 * gap) / 4).max(24);
        let top = 208 * ui;
        let libre = self.h as i32 - 52 * ui - 8 * ui - 8 * ui - 4 * gap - top;
        let bh = (libre / 5).max(16);
        (m, top, bw, bh, gap)
    }

    /// Botones del grid (stack, sin alloc: se reconstruye por frame).
    fn botones(&self) -> [Button; N_BOTONES] {
        let (x0, y0, bw, bh, gap) = self.grid();
        std::array::from_fn(|i| Button {
            x: x0 + (i as i32 % 4) * (bw + gap),
            y: y0 + (i as i32 / 4) * (bh + gap),
            w: bw,
            h: bh,
            label: ETIQUETAS[i],
            base: color_rol(i),
            ink: Color::rgb(255, 255, 255),
        })
    }

    /// Aplica un evento del host (ping/shutdown/touch).
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
                let (x, y) = (t.x as i32, t.y as i32);
                match t.phase {
                    Phase::Down => {
                        // botón X (cierre): la sub-app se apaga limpia desde
                        // adentro (r10, mismo contrato que el demo)
                        if x_hit(self.w as i32, self.ui as i32, x, y) {
                            self.exit = Some("x");
                            return;
                        }
                        let bots = self.botones();
                        for (i, b) in bots.iter().enumerate() {
                            if b.contains(x, y) {
                                self.pressed[i] = true;
                                self.accion(i);
                            }
                        }
                    }
                    // la calculadora no arrastra: sin fase Move
                    Phase::Move => {}
                    Phase::Up => {
                        self.pressed = [false; N_BOTONES];
                    }
                }
            }
        }
    }

    /// Índice de botón → tecla lógica.
    fn accion(&mut self, i: usize) {
        let k = match i {
            0 => Tecla::C,
            1 => Tecla::Pct,
            2 => Tecla::Borrar,
            3 => Tecla::Op(b'/'),
            4..=6 => Tecla::Digito(7 + (i as u8 - 4)), // 7 8 9
            7 => Tecla::Op(b'*'),
            8..=10 => Tecla::Digito(4 + (i as u8 - 8)), // 4 5 6
            11 => Tecla::Op(b'-'),
            12..=14 => Tecla::Digito(1 + (i as u8 - 12)), // 1 2 3
            15 => Tecla::Op(b'+'),
            16 => Tecla::Signo,
            17 => Tecla::Digito(0),
            18 => Tecla::Punto,
            19 => Tecla::Igual,
            _ => return,
        };
        self.tecla(k);
    }

    /// Máquina de estados de una tecla.
    fn tecla(&mut self, k: Tecla) {
        match k {
            Tecla::C => self.reset(),
            Tecla::Digito(d) => {
                if self.err {
                    self.reset();
                }
                if self.en_resultado {
                    self.nueva_expr();
                }
                self.ent.digito(d);
                self.ent_tipeada = true;
            }
            Tecla::Punto => {
                if self.err {
                    self.reset();
                }
                if self.en_resultado {
                    self.nueva_expr();
                }
                self.ent.punto();
                self.ent_tipeada = true;
            }
            Tecla::Op(op) => {
                if self.err {
                    return; // Error: solo C (o un dígito) revive
                }
                // Cap de la expresión: ops[..MAX_OPS] y el final de '=' en
                // nums[..n_ops+1] deben caber (n_ops ≤ 14 al congelar).
                if self.n_ops >= MAX_OPS {
                    return;
                }
                if self.en_resultado {
                    // encadena desde el resultado: 2+3=* → 5*…
                    let r = self.ent.valor().unwrap_or(Dec::ZERO);
                    self.nueva_expr();
                    self.nums[0] = EcoNum::de_dec(r);
                    self.ops[0] = op;
                    self.n_ops = 1;
                    self.ent = Entrada::nueva();
                    self.ent_tipeada = false;
                } else if !self.ent_tipeada && self.n_ops > 0 {
                    // nada tecleado tras el operador: lo CORRIGE (2+* → 2*)
                    self.ops[self.n_ops - 1] = op;
                } else {
                    // congela la entrada como el número n_ops (la PRIMERA
                    // cifra de la expresión vive en nums[0]: sin cero
                    // fantasma inicial — 2*3 debe ser 6, no 0)
                    let eco = EcoNum::de_texto(self.ent.texto())
                        .unwrap_or_else(|| EcoNum::de_dec(Dec::ZERO));
                    self.nums[self.n_ops] = eco;
                    self.ops[self.n_ops] = op;
                    self.n_ops += 1;
                    self.expr_completa = false;
                    self.ent = Entrada::nueva();
                    self.ent_tipeada = false;
                }
            }
            Tecla::Igual => {
                if self.err {
                    return;
                }
                if self.n_ops == 0 {
                    if self.en_resultado {
                        return; // = repetido: sin repetición (documentado)
                    }
                    // número solo: se normaliza como resultado
                    let r = self.ent.valor().unwrap_or(Dec::ZERO);
                    let mut buf = [0u8; FMT_BUF];
                    self.ent.poner(r.fmt(&mut buf));
                    self.en_resultado = true;
                    return;
                }
                // expresión completa: congela el final y evalúa
                let final_num =
                    EcoNum::de_texto(self.ent.texto()).unwrap_or_else(|| EcoNum::de_dec(Dec::ZERO));
                self.nums[self.n_ops] = final_num;
                self.expr_completa = true; // el eco muestra la expresión entera
                let mut eco = [0u8; ECO_CAP];
                let mut n = self.volcar_expr(&mut eco);
                n = push_bytes(&mut eco, n, b"=");
                match eval(&self.nums[..self.n_ops + 1], &self.ops[..self.n_ops]) {
                    Ok(r) => {
                        let mut rb = [0u8; FMT_BUF];
                        let rs = r.fmt(&mut rb);
                        n = push_bytes(&mut eco, n, rs.as_bytes());
                        self.set_hist(&eco[..n]);
                        self.ent.poner(rs);
                        self.en_resultado = true;
                        self.n_ops = 0;
                        self.expr_completa = false;
                    }
                    Err(_) => {
                        // conserva la expresión congelada para contexto: el
                        // eco muestra "12/0" y el historial "12/0=Error"
                        n = push_bytes(&mut eco, n, b"Error");
                        self.set_hist(&eco[..n]);
                        self.err = true;
                    }
                }
            }
            Tecla::Borrar => {
                if self.err {
                    return; // en Error el borrado no revive (como C-less)
                }
                if self.en_resultado {
                    self.en_resultado = false; // el resultado se vuelve editable
                }
                self.ent.borrar();
                self.ent_tipeada = true;
            }
            Tecla::Signo => {
                if self.err {
                    return;
                }
                self.en_resultado = false;
                self.ent.negar();
                self.ent_tipeada = true;
            }
            Tecla::Pct => {
                if self.err {
                    return;
                }
                let Some(v) = self.ent.valor() else { return };
                match v.percent() {
                    Ok(r) => {
                        let mut rb = [0u8; FMT_BUF];
                        self.ent.poner(r.fmt(&mut rb));
                        self.ent_tipeada = true;
                        self.en_resultado = true; // el % ES un resultado
                    }
                    Err(_) => self.err = true,
                }
            }
        }
    }

    /// Historial: copia a capacidad fija.
    fn set_hist(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(ECO_CAP);
        self.hist[..n].copy_from_slice(&bytes[..n]);
        self.hist_len = n;
    }

    /// Vuelca el eco de la expresión congelada (sin la entrada en edición,
    /// salvo `expr_completa`). Devuelve los bytes escritos (0 si no hay
    /// operadores).
    fn volcar_expr(&self, out: &mut [u8]) -> usize {
        if self.n_ops == 0 {
            return 0;
        }
        let mut n = push_bytes(out, 0, self.nums[0].texto().as_bytes());
        for i in 0..self.n_ops {
            n = push_bytes(out, n, &[self.ops[i]]);
            // el número posterior a ops[i] solo si ya está congelado
            if i + 1 < self.n_ops || self.expr_completa {
                n = push_bytes(out, n, self.nums[i + 1].texto().as_bytes());
            }
        }
        n
    }
}

/// Color de botón por índice del grid (rol visual).
fn color_rol(i: usize) -> Color {
    match i {
        0 => Color::rgb(170, 60, 60),                // C: rojo apagado
        1 | 2 | 16 => Color::rgb(74, 82, 100),       // % < +/-: pizarra clara
        3 | 7 | 11 | 15 => Color::rgb(38, 122, 222), // + - * /: azul del demo
        19 => Color::rgb(22, 152, 118),              // =: verde del demo
        _ => Color::rgb(52, 58, 72),                 // dígitos y .: pizarra
    }
}

/// Pinta un botón con escalón tipográfico 3→2→1 (los botones de la
/// calculadora son más grandes que los del demo: la escala 3 cabe).
/// Mismo relieve que `Button::draw` (pressed = mezcla al blanco 1/3,
/// sombra inferior de 2 px) pero con texto más grande.
fn dibujar_boton(c: &mut Canvas, b: &Button, pressed: bool) {
    let body = if pressed {
        b.base.lerp(Color::rgb(255, 255, 255), 1, 3)
    } else {
        b.base
    };
    let r = (b.h / 5).max(3);
    c.fill_round_rect(b.x, b.y, b.w, b.h, r, body);
    c.fill_rect(b.x + r, b.y + b.h - 2, b.w - 2 * r, 2, Color::rgb(0, 0, 0));
    let chars = b.label.chars().count() as i32;
    let mut scale = 1;
    if b.w >= chars * 36 + 8 && b.h >= 48 {
        scale = 3;
    } else if b.w >= chars * 24 + 8 && b.h >= 36 {
        scale = 2;
    }
    let tw = chars * 12 * scale;
    let tx = b.x + (b.w - tw) / 2;
    let ty = b.y + (b.h - 16 * scale) / 2 + i32::from(pressed);
    c.draw_text(
        tx.max(b.x + 2),
        ty.max(b.y + 2),
        b.label,
        scale as u32,
        b.ink,
    );
}

/// Copia `s` al final de `out[at..]` (sin alloc; recorta al buffer).
fn push_bytes(out: &mut [u8], at: usize, s: &[u8]) -> usize {
    let room = out.len().saturating_sub(at).min(s.len());
    out[at..at + room].copy_from_slice(&s[..room]);
    at + room
}

// ---------------------------------------------------------------------------
// draw: pinta el frame completo (coords clipean dentro del canvas)
// ---------------------------------------------------------------------------

impl Calc {
    fn draw(&self, c: &mut Canvas) {
        let w = self.w as i32;
        let h = self.h as i32;
        let ui = self.ui as i32;
        let ui32 = self.ui;

        // Fondo: degradado oscuro estático (la calculadora no anima).
        c.fill_vgrad(Color::rgb(18, 21, 28), Color::rgb(9, 11, 16));

        // Barra superior translúcida + título.
        c.fill_rect(
            0,
            0,
            w,
            56 * ui,
            Color {
                a: 150,
                ..Color::rgb(8, 10, 16)
            },
        );
        c.draw_text(
            12 * ui,
            6 * ui,
            "Calculadora",
            2 * ui32,
            Color::rgb(240, 242, 250),
        );
        c.draw_text(
            12 * ui,
            38 * ui,
            "sub-app nativa, decimal exacto",
            ui32,
            Color::rgb(150, 160, 180),
        );

        // Botón de cierre (X): esquina superior derecha de la SUB-APP.
        draw_x(c, w, ui);

        // Panel del display.
        let px = 12 * ui;
        let py = 68 * ui;
        let pw = w - 24 * ui;
        let ph = 132 * ui;
        c.fill_round_rect(px, py, pw, ph, 10 * ui, Color::rgb(24, 28, 38));
        c.draw_rect_outline(px, py, pw, ph, ui, Color::rgb(45, 52, 68));

        // Línea 1: eco de la expresión apilada ("12+3*").
        let mut eco = [0u8; ECO_CAP];
        let eco_n = self.volcar_expr(&mut eco);
        if eco_n > 0 {
            // no cabe → cola de la expresión (lo más reciente) con '~'
            // delante; sin alloc: recorte en el MISMO buffer, de atrás
            // para adelante (todo el eco es ASCII: cortar en byte es seguro)
            let cols = ((pw - 20 * ui).max(1) / (12 * ui)) as usize;
            let total = eco_n;
            let mostrar: &str = if total <= cols {
                core::str::from_utf8(&eco[..total]).unwrap_or("?")
            } else {
                let ini = total.saturating_sub(cols.saturating_sub(1));
                // desplaza la cola un byte a la derecha y pone '~'
                eco.copy_within(ini..total, 1);
                eco[0] = b'~';
                core::str::from_utf8(&eco[..total - ini + 1]).unwrap_or("?")
            };
            c.draw_text(
                px + 10 * ui,
                py + 12 * ui,
                mostrar,
                ui32,
                Color::rgb(150, 160, 180),
            );
        }

        // Línea 2: entrada / resultado (grande, alineada a la derecha).
        let principal = if self.err { "Error" } else { self.ent.texto() };
        let chars = principal.chars().count() as i32;
        let avail = (pw - 20 * ui).max(1);
        let base = if chars * 36 * ui <= avail {
            3
        } else if chars * 24 * ui <= avail {
            2
        } else {
            1
        };
        let tw = chars * 12 * base * ui;
        let tx = (px + pw - 10 * ui) - tw;
        let ty = py + (96 - 16 * base) * ui;
        let color = if self.err {
            Color::rgb(255, 120, 110)
        } else {
            Color::rgb(240, 242, 250)
        };
        c.draw_text(
            tx.max(px + 8 * ui),
            ty,
            principal,
            base as u32 * ui32,
            color,
        );

        // Línea 3: historial ("12+3*4=24").
        if self.hist_len > 0 {
            if let Ok(s) = core::str::from_utf8(&self.hist[..self.hist_len]) {
                c.draw_text(
                    px + 10 * ui,
                    py + 104 * ui,
                    s,
                    ui32,
                    Color::rgb(110, 200, 180),
                );
            }
        }

        // Grid de botones.
        let bots = self.botones();
        for (i, b) in bots.iter().enumerate() {
            dibujar_boton(c, b, self.pressed[i]);
        }

        // Barra inferior: pista de uso + telemetría (encima del grid si
        // un fb degenerado los solapa: se pinta al final).
        c.fill_rect(
            0,
            h - 52 * ui,
            w,
            52 * ui,
            Color {
                a: 170,
                ..Color::rgb(8, 10, 16)
            },
        );
        let pista = if self.err {
            "Error: toca C para seguir"
        } else if self.en_resultado {
            "resultado: operador encadena, digito reinicia"
        } else {
            "C limpia, < borra, X cierra"
        };
        c.draw_text(12 * ui, h - 44 * ui, pista, ui32, Color::rgb(150, 160, 180));
        // telemetría compacta sin alloc (mismo helper del demo)
        let mut fb = [0u8; 12];
        let fps_s = format_buf(&mut fb, self.last_fps);
        let mut linea = [0u8; 40];
        let mut ln = push_bytes(&mut linea, 0, b"fps:");
        ln = push_bytes(&mut linea, ln, fps_s.as_bytes());
        ln = push_bytes(&mut linea, ln, b"  frames:");
        let mut fb2 = [0u8; 12];
        let fr_s = format_buf(&mut fb2, self.frames);
        ln = push_bytes(&mut linea, ln, fr_s.as_bytes());
        if let Ok(s) = core::str::from_utf8(&linea[..ln]) {
            c.draw_text(12 * ui, h - 24 * ui, s, ui32, Color::rgb(190, 200, 215));
        }
    }
}

// ---------------------------------------------------------------------------
// Escala de UI + botón de cierre X (mismo contrato que devapp-demo;
// duplicado a propósito — probes desechables, se unifica en arca-rt F3b)
// ---------------------------------------------------------------------------

/// Factor de escala de la UI contra el canvas de DISEÑO del layout
/// (336 columnas × 720 filas — las coordenadas que `draw` codifica:
/// título 56, panel 68..200, grid 208…, barra baja 52).
///
/// r13: se escala por AMBAS dimensiones con min(w/336, h/720) — en el
/// Huawei real del usuario (fb 1:1 de 720×1536) da ui=2; nunca escala
/// más que el eje más ajustado. Mínimo 1 (los fbs chicos de qemu
/// conservan el layout original).
#[must_use]
fn ui_scale(w: u16, h: u16) -> u32 {
    let s = (w as f32 / 336.0).min(h as f32 / 720.0).round() as u32;
    s.clamp(1, 4)
}

/// Zona táctil/dibujado del botón de cierre (diseño: lado 40 en (w-52, 6)).
/// Devuelve (x, y, lado).
#[must_use]
fn zona_x(w: i32, ui: i32) -> (i32, i32, i32) {
    let side = 40 * ui;
    (w - 52 * ui, 6 * ui, side)
}

/// ¿El toque (x, y) cae dentro del botón X? (hit-test puro, testeable).
#[must_use]
fn x_hit(w: i32, ui: i32, x: i32, y: i32) -> bool {
    let (rx, ry, side) = zona_x(w, ui);
    x >= rx && x < rx + side && y >= ry && y < ry + side
}

/// Pinta el botón de cierre: chip redondeado translúcido + X blanca.
fn draw_x(c: &mut Canvas, w: i32, ui: i32) {
    let (x, y, side) = zona_x(w, ui);
    let r = (side / 5).max(2);
    c.fill_round_rect(
        x,
        y,
        side,
        side,
        r,
        Color {
            a: 170,
            ..Color::rgb(8, 10, 16)
        },
    );
    c.draw_rect_outline(x, y, side, side, ui.max(1), Color::rgb(120, 130, 150));
    let pad = side / 4;
    let inner = (side - 2 * pad).max(4);
    let t = 4 * ui;
    let mut i = 0;
    while i + t <= inner {
        c.fill_rect(x + pad + i, y + pad + i, t, t, Color::rgb(235, 238, 245));
        c.fill_rect(
            x + pad + inner - i - t,
            y + pad + i,
            t,
            t,
            Color::rgb(235, 238, 245),
        );
        i += t;
    }
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

/// FPS medido: (Δframes × 1000) / Δt en ms (delta real + saturación:
/// ver la regresión r9 del demo — underflow de u64 en el logcat).
#[must_use]
fn fps_medida(delta_frames: u64, dt_ms: u64) -> u64 {
    delta_frames.saturating_mul(1000) / dt_ms.max(1)
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
    // INTEGRIDAD: solo write(2) + _exit(2) (async-signal-safe).
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
// main + bucle del probe (misma cadencia y watchdog del demo)
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

/// Bucle principal (modo teléfono, lanzado por DemoActivity).
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
    let frame_bytes = rgba_frame_bytes(w, h);
    let region = FrameFile::open(std::path::Path::new(&fb_path), frame_bytes)
        .map_err(|e| format!("attach {fb_path}: {e}"))?;

    install_signal_handlers()?;
    let pid = unsafe { libc::getpid() };
    let t0 = mono_ms()?;
    emit_line(&format!(
        "{{\"event\":\"hello\",\"ts\":{t0},\"pid\":{pid},\"w\":{w},\"h\":{h}}}"
    ));

    let mut calc = Calc::new(w, h);
    let mut rx_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut stdin_open = true;
    let mut slot = 0usize;
    let mut next = t0;
    let mut stats_t0 = t0;
    let mut stats_f0 = 0u32;

    loop {
        // 1) esperar hasta el próximo tick vigilando stdin (drenaje
        //    SIEMPRE no-bloqueante cuando wait==0: ver r11 del demo)
        let now = mono_ms()?;
        let wait = next.saturating_sub(now).min(500) as i32;
        if stdin_open {
            if poll_stdin(if wait > 0 { wait } else { 0 })? {
                match read_stdin(&mut rx_buf)? {
                    StdinEvent::Data => {
                        for line in drain_lines(&mut rx_buf) {
                            let s = String::from_utf8_lossy(&line).into_owned();
                            if let Some(ev) = parse_line(&s) {
                                calc.evento(ev);
                            }
                        }
                    }
                    StdinEvent::Closed => stdin_open = false,
                    StdinEvent::NoData => {}
                }
            }
        } else if wait > 0 {
            std::thread::sleep(Duration::from_millis(wait as u64));
        }

        // 2) tick de frame (la calculadora es estática: sin física, el
        //    render refleja el estado tras las teclas)
        let now = mono_ms()?;
        if now >= next {
            let hdr = FrameHeader::new_opaque(w, h, calc.frames.wrapping_add(1), now);
            {
                let slots = region.slots();
                let mut guard = slots
                    .begin_write(slot)
                    .map_err(|e| format!("begin_write: {e}"))?;
                let payload = guard.payload();
                paint_frame(payload, &hdr, |c| calc.draw(c)).map_err(|e| format!("paint: {e}"))?;
                guard.publish().map_err(|e| format!("publish: {e}"))?;
            }
            calc.frames += 1;
            FRAMES.store(calc.frames as u64, Ordering::Relaxed);
            emit_line(&format!(
                "{{\"event\":\"frame\",\"seq\":{},\"slot\":{}}}",
                calc.frames, slot
            ));
            slot ^= 1;
            // Cadencia anclada al calendario (`next += FRAME_MS`); si nos
            // atrasamos >200 ms resincronizamos (r9 del demo: la rama vieja
            // era código muerto).
            if now.saturating_sub(next) > 200 {
                next = now;
            }
            next += FRAME_MS;

            // stats cada STATS_CADA frames
            if calc.frames % STATS_CADA == 0 {
                let dfr = (calc.frames as u64).saturating_sub(stats_f0 as u64);
                let dt = now.saturating_sub(stats_t0).max(1);
                let fps = fps_medida(dfr, dt);
                calc.last_fps = u32::try_from(fps).unwrap_or(u32::MAX);
                emit_line(&format!(
                    "{{\"event\":\"stats\",\"frames\":{},\"fps\":{}}}",
                    calc.frames, fps
                ));
                stats_t0 = now;
                stats_f0 = calc.frames;
            }
        }

        // 3) salida limpia
        if let Some(reason) = calc.exit {
            emit_line(&format!(
                "{{\"event\":\"exiting\",\"reason\":\"{reason}\",\"frames\":{}}}",
                calc.frames
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
// --selftest: pipeline render→publish→read + escenarios de cálculo (PC)
// ---------------------------------------------------------------------------

/// Secuencia de teclas sobre un `Calc` fresco (helper del selftest/tests).
fn t(c: &mut Calc, keys: &[Tecla]) {
    for k in keys {
        c.tecla(*k);
    }
}

fn selftest() -> i32 {
    const W: u16 = 320;
    const H: u16 = 240;
    let mut fallos = 0u32;

    // ── parte A: pipeline seqlock de punta a punta (como el demo) ──
    let path = std::env::temp_dir().join("arca-calc-selftest.bin");
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

    let mut calc = Calc::new(W, H);
    let mut out = vec![0u8; frame_bytes];
    let mut slot = 0usize;

    for i in 0..5u64 {
        if i == 2 {
            // un toque sobre el botón "7" (índice 4 del grid, fila 1):
            // valida input→estado→render usando su propia geometría
            let (bx, by) = {
                let b = &calc.botones()[4];
                (b.x + b.w / 2, b.y + b.h / 2)
            };
            calc.evento(Event::Touch(Touch {
                phase: Phase::Down,
                x: bx as u32,
                y: by as u32,
                t: Some(i * FRAME_MS),
            }));
            calc.evento(Event::Touch(Touch {
                phase: Phase::Up,
                x: bx as u32,
                y: by as u32,
                t: Some(i * FRAME_MS),
            }));
        }
        // marcador determinista: rect opaco de color conocido arriba
        let marca = Color::rgb(10 * i as u8 + 5, 200, 150);
        let hdr = FrameHeader::new_opaque(W, H, i as u32 + 1, i * FRAME_MS);
        let ok = {
            let slots = writer.slots();
            match slots.begin_write(slot) {
                Ok(mut guard) => {
                    let payload = guard.payload();
                    match paint_frame(payload, &hdr, |c| {
                        calc.draw(c);
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
    // el toque del frame 2 debe haber tecleado un "7"
    if calc.ent.texto() != "7" {
        eprintln!(
            "selftest: el toque en el grid no tecleó '7' (ent={})",
            calc.ent.texto()
        );
        fallos += 1;
    }
    let _ = std::fs::remove_file(&path);

    // ── parte B: escenarios del motor (a través de la máquina de teclas) ──
    use Tecla::{Borrar, Digito, Igual, Op, Pct, Punto, Signo};
    let escenarios: &[(&str, &[Tecla], &str)] = &[
        ("7*6=42", &[Digito(7), Op(b'*'), Digito(6), Igual], "42"),
        (
            "12+3*4=24 (precedencia)",
            &[
                Digito(1),
                Digito(2),
                Op(b'+'),
                Digito(3),
                Op(b'*'),
                Digito(4),
                Igual,
            ],
            "24",
        ),
        (
            "10-2-3=5",
            &[
                Digito(1),
                Digito(0),
                Op(b'-'),
                Digito(2),
                Op(b'-'),
                Digito(3),
                Igual,
            ],
            "5",
        ),
        (
            "0.1+0.2=0.3 exacto",
            &[Punto, Digito(1), Op(b'+'), Punto, Digito(2), Igual],
            "0.3",
        ),
        ("50%=0.5", &[Digito(5), Digito(0), Pct], "0.5"),
        (
            "-5+3=-2 (signo)",
            &[Digito(5), Signo, Op(b'+'), Digito(3), Igual],
            "-2",
        ),
        (
            "123<+4=16 (borrar)",
            &[
                Digito(1),
                Digito(2),
                Digito(3),
                Borrar,
                Op(b'+'),
                Digito(4),
                Igual,
            ],
            "16",
        ),
        (
            "2+3+=5 (op final)",
            &[Digito(2), Op(b'+'), Digito(3), Op(b'+'), Igual],
            "5",
        ),
        (
            "2+*3=6 (corrige op)",
            &[Digito(2), Op(b'+'), Op(b'*'), Digito(3), Igual],
            "6",
        ),
        (
            "2+3=*4=20 (encadena)",
            &[
                Digito(2),
                Op(b'+'),
                Digito(3),
                Igual,
                Op(b'*'),
                Digito(4),
                Igual,
            ],
            "20",
        ),
    ];
    for (nombre, keys, esperado) in escenarios {
        let mut c = Calc::new(W, H);
        t(&mut c, keys);
        if c.ent.texto() != *esperado {
            eprintln!("selftest: {nombre}: ent={} ≠ {esperado}", c.ent.texto());
            fallos += 1;
        }
    }

    // error y recuperación
    let mut c = Calc::new(W, H);
    t(&mut c, &[Digito(1), Op(b'/'), Digito(0), Igual]);
    if !c.err {
        eprintln!("selftest: 1/0 debe dar Error");
        fallos += 1;
    }
    t(&mut c, &[Digito(5), Igual]);
    if c.err || c.ent.texto() != "5" {
        eprintln!(
            "selftest: tras Error, 5= debe dar 5 (got {})",
            c.ent.texto()
        );
        fallos += 1;
    }

    // historial "12+3=15"
    let mut c = Calc::new(W, H);
    t(&mut c, &[Digito(1), Digito(2), Op(b'+'), Digito(3), Igual]);
    let hist = core::str::from_utf8(&c.hist[..c.hist_len]).unwrap_or("?");
    if hist != "12+3=15" {
        eprintln!("selftest: historial {hist:?} ≠ \"12+3=15\"");
        fallos += 1;
    }

    if fallos == 0 {
        println!("selftest: OK (5 frames seqlock + 10 escenarios + error/hist)");
        println!("selftest: pipeline render→publish→read y motor decimal verificados");
        0
    } else {
        eprintln!("selftest: FALLÓ ({fallos} comprobaciones)");
        1
    }
}

// ---------------------------------------------------------------------------
// Tests (geometría + máquina de teclas + helpers copiados del demo)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{fps_medida, ui_scale, x_hit, zona_x, Calc, Tecla, ECO_CAP, STATS_CADA};

    /// Mismos bordes que el demo (r13): por ambas dimensiones.
    #[test]
    fn ui_scale_bordes() {
        assert_eq!(ui_scale(320, 240), 1); // selftest
        assert_eq!(ui_scale(160, 360), 1); // fb chico de qemu
        assert_eq!(ui_scale(336, 720), 1); // canvas de diseño
        assert_eq!(ui_scale(720, 1536), 2); // Huawei real (r12 daba 3 = enorme)
        assert_eq!(ui_scale(664, 1440), 2); // geometría r10 (r11 daba 3)
        assert_eq!(ui_scale(1049, 2160), 3); // fb 1:1 FHD
        assert_eq!(ui_scale(1080, 2160), 3);
        assert_eq!(ui_scale(2048, 1040), 1); // ancha: no estalla horizontal
        assert_eq!(ui_scale(432, 4096), 1); // alta y angosta: no estalla
        assert_eq!(ui_scale(3072, 6144), 4); // cap
        assert_eq!(ui_scale(0, 0), 1); // absurdo pero sin pánico
    }

    #[test]
    fn fps_medida_bordes() {
        assert_eq!(fps_medida(120, 4000), 30);
        assert_eq!(fps_medida(60, 2000), 30);
        assert_eq!(fps_medida(0, 4000), 0);
        assert_eq!(fps_medida(120, 0), 120_000); // dt degenerado
        assert_eq!(fps_medida(u64::MAX, 1), u64::MAX); // satura, no wrap
    }

    /// La aritmética del stats tal como la computa el bucle (regresión r9).
    #[test]
    fn stats_fps_no_hace_underflow() {
        let mut stats_f0: u32 = 0;
        for k in 1..=20u32 {
            let frames = STATS_CADA * k;
            let dfr = (frames as u64).saturating_sub(stats_f0 as u64);
            let fps = fps_medida(dfr, 3960); // 120 frames × 33 ms
            assert_eq!(fps, 30, "intervalo {k}");
            stats_f0 = frames;
        }
    }

    /// r10: la X siempre cae dentro del canvas y el hit-test coincide con
    /// la zona dibujada en todas las geometrías del host/qemu.
    #[test]
    fn zona_x_dentro_del_canvas_y_hit() {
        for (w, h) in [
            (160, 360),
            (336, 720),
            (498, 1080),
            (666, 1440),
            (720, 1536), // Huawei real (r13)
            (1049, 2160),
            (1080, 2222),
        ] {
            let ui = ui_scale(w, h) as i32;
            let w = w as i32;
            let h = h as i32;
            let (x, y, side) = zona_x(w, ui);
            assert!(x >= 0 && y >= 0, "({w}x{h}) zona negativa: {x},{y}");
            assert!(
                x + side <= w && y + side <= h,
                "({w}x{h}) zona fuera del canvas: {x}+{side}>{w} o {y}+{side}>{h}"
            );
            assert!(x_hit(w, ui, x + side / 2, y + side / 2), "({w}x{h}) centro");
            assert!(!x_hit(w, ui, 0, 0), "({w}x{h}) el origen no es la X");
        }
    }

    /// El grid de 20 botones cabe en el canvas en todas las geometrías; en
    /// las reales (≥ diseño) además queda sobre la barra inferior.
    #[test]
    fn botones_dentro_del_canvas() {
        for (w, h) in [
            (160u16, 360u16), // qemu degenerado: solo "dentro del canvas"
            (336, 720),       // diseño
            (720, 1536),      // Huawei real
            (1049, 2160),     // FHD 1:1
            (1080, 2222),
        ] {
            let c = Calc::new(w, h);
            let ui = c.ui as i32;
            let (_x0, y0, bw, bh, gap) = c.grid();
            assert!(bw > 0 && bh > 0, "({w}x{h}) botón degenerado {bw}x{bh}");
            for i in 0..20 {
                let b = &c.botones()[i];
                assert!(
                    b.x >= 0 && b.y >= 0,
                    "({w}x{h}) botón {i} negativo: {},{}",
                    b.x,
                    b.y
                );
                assert!(
                    b.x + b.w <= w as i32 && b.y + b.h <= h as i32,
                    "({w}x{h}) botón {i} fuera: {}+{}>{w} o {}+{}>{h}",
                    b.x,
                    b.w,
                    b.y,
                    b.h
                );
            }
            if w as i32 >= 336 * ui {
                // geometría sana: el grid no pisa la barra inferior
                let ultimo = y0 + 4 * (bh + gap) + bh;
                assert!(
                    ultimo <= h as i32 - 52 * ui,
                    "({w}x{h}) grid {ultimo} pisa la barra {}",
                    h as i32 - 52 * ui
                );
            }
        }
    }

    /// El panel del display no degenera (ancho/alto positivos).
    #[test]
    fn panel_display_valido() {
        for (w, h) in [(160u16, 360u16), (336, 720), (720, 1536), (1080, 2222)] {
            let c = Calc::new(w, h);
            let ui = c.ui as i32;
            let pw = w as i32 - 24 * ui;
            let ph = 132 * ui;
            assert!(pw > 0 && ph > 0, "({w}x{h}) panel degenerado {pw}x{ph}");
            assert!(68 * ui + ph <= h as i32, "({w}x{h}) panel fuera del canvas");
        }
    }

    /// Los escenarios del motor (espejo del selftest, en cargo test).
    #[test]
    fn teclas_escenarios() {
        let casos: &[(&[Tecla], &str)] = &[
            (
                &[
                    Tecla::Digito(7),
                    Tecla::Op(b'*'),
                    Tecla::Digito(6),
                    Tecla::Igual,
                ],
                "42",
            ),
            (
                &[
                    Tecla::Digito(1),
                    Tecla::Digito(2),
                    Tecla::Op(b'+'),
                    Tecla::Digito(3),
                    Tecla::Op(b'*'),
                    Tecla::Digito(4),
                    Tecla::Igual,
                ],
                "24",
            ),
            (
                &[
                    Tecla::Punto,
                    Tecla::Digito(1),
                    Tecla::Op(b'+'),
                    Tecla::Punto,
                    Tecla::Digito(2),
                    Tecla::Igual,
                ],
                "0.3",
            ),
            (&[Tecla::Digito(5), Tecla::Digito(0), Tecla::Pct], "0.5"),
            (
                &[
                    Tecla::Digito(5),
                    Tecla::Signo,
                    Tecla::Op(b'+'),
                    Tecla::Digito(3),
                    Tecla::Igual,
                ],
                "-2",
            ),
        ];
        for (keys, esperado) in casos {
            let mut c = Calc::new(336, 720);
            for k in keys.iter() {
                c.tecla(*k);
            }
            assert_eq!(c.ent.texto(), *esperado, "teclas {keys:?}");
        }
    }

    /// Error por división por cero + recuperación con dígito y con C.
    #[test]
    fn error_div0_y_recuperacion() {
        let mut c = Calc::new(336, 720);
        for k in [
            Tecla::Digito(1),
            Tecla::Op(b'/'),
            Tecla::Digito(0),
            Tecla::Igual,
        ] {
            c.tecla(k);
        }
        assert!(c.err, "1/0 debe marcar Error");
        // el eco conserva la expresión fallida congelada ("1/0")
        let mut eco = [0u8; ECO_CAP];
        let n = c.volcar_expr(&mut eco);
        assert_eq!(
            core::str::from_utf8(&eco[..n]).unwrap_or("?"),
            "1/0",
            "el eco de error debe mostrar la expresión"
        );
        // dígito revive
        c.tecla(Tecla::Digito(9));
        assert!(!c.err && c.ent.texto() == "9");
        // C también
        for k in [Tecla::Op(b'/'), Tecla::Digito(0), Tecla::Igual] {
            c.tecla(k);
        }
        assert!(c.err);
        c.tecla(Tecla::C);
        assert!(!c.err && c.ent.texto() == "0" && c.n_ops == 0);
    }

    /// Desborde: 10^18 × 10^18 → Error (el tope del decimal exacto).
    #[test]
    fn error_overflow() {
        let mut c = Calc::new(336, 720);
        // 999999999999999999 ~ 10^18 (18 dígitos, cota de entrada 19)
        for _ in 0..18 {
            c.tecla(Tecla::Digito(9));
        }
        c.tecla(Tecla::Op(b'*'));
        for _ in 0..18 {
            c.tecla(Tecla::Digito(9));
        }
        c.tecla(Tecla::Igual);
        assert!(c.err, "10^18×10^18 debe dar Error (desborde)");
    }

    /// El eco de expresión nunca excede su buffer y el historial respeta
    /// la capacidad (entradas largas clipean, no desbordan).
    #[test]
    fn eco_y_historial_acotados() {
        let mut c = Calc::new(336, 720);
        // llena la expresión al máximo (15 operadores) y evalúa
        for _ in 0..15 {
            c.tecla(Tecla::Digito(9));
            c.tecla(Tecla::Op(b'+'));
        }
        c.tecla(Tecla::Digito(9));
        c.tecla(Tecla::Igual);
        assert!(c.hist_len <= ECO_CAP);
        assert_eq!(c.ent.texto(), "144"); // 16 nueves sumados
        let mut eco = [0u8; ECO_CAP];
        assert!(c.volcar_expr(&mut eco) <= ECO_CAP);
    }

    /// Cota de la expresión: el operador 16 se ignora (capacidad fija).
    #[test]
    fn cota_de_expresion() {
        let mut c = Calc::new(336, 720);
        for _ in 0..20 {
            c.tecla(Tecla::Digito(1));
            c.tecla(Tecla::Op(b'+'));
        }
        // 15 operadores caben (ops[..MAX_OPS] + final en nums[15]); el resto
        // se ignora
        assert_eq!(c.n_ops, 15);
    }

    /// Pct sobre resultado encadena y sobre entrada edita.
    #[test]
    fn pct_estados() {
        let mut c = Calc::new(336, 720);
        for k in [
            Tecla::Digito(2),
            Tecla::Digito(0),
            Tecla::Digito(0),
            Tecla::Pct,
        ] {
            c.tecla(k);
        }
        assert_eq!(c.ent.texto(), "2");
        // tras %, un dígito reinicia (en_resultado)
        c.tecla(Tecla::Digito(7));
        assert_eq!(c.ent.texto(), "7");
    }
}
