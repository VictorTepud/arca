//! arca-log (L0): mini-logger con el estilo visual de tracing.
//!
//! Salida (a stderr):
//! ```text
//! 0.000148260s  INFO arca::rt: apagado limpio code=0
//! ```
//! Sin dependencias externas: compila rápido y funciona sin red.
//! Configuración: variable de entorno `ARCA_LOG = error|warn|info|debug`
//! (por defecto `info`).

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

/// Nivel de un mensaje de registro.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nivel {
    Debug,
    Info,
    Warn,
    Error,
}

impl Nivel {
    /// (texto alineado a 5 caracteres, color ANSI)
    fn como_par(self) -> (&'static str, &'static str) {
        match self {
            Nivel::Debug => ("DEBUG", "\x1b[34m"),
            Nivel::Info => (" INFO", "\x1b[32m"),
            Nivel::Warn => (" WARN", "\x1b[33m"),
            Nivel::Error => ("ERROR", "\x1b[31m"),
        }
    }

    /// Umbral numérico para comparar contra el nivel configurado.
    fn umbral(self) -> u8 {
        match self {
            Nivel::Error => 0,
            Nivel::Warn => 1,
            Nivel::Info => 2,
            Nivel::Debug => 3,
        }
    }
}

static T0: OnceLock<Instant> = OnceLock::new();
static UMBRAL: OnceLock<u8> = OnceLock::new();

/// Inicializa el temporizador (tiempo desde el arranque del proceso) y lee
/// `ARCA_LOG`. Es idempotente: llamarla varias veces no rompe nada.
pub fn init() {
    let _ = T0.set(Instant::now());
    let cfg = std::env::var("ARCA_LOG").unwrap_or_else(|_| "info".into());
    let n = match cfg.to_ascii_lowercase().as_str() {
        "error" | "err" => 0,
        "warn" => 1,
        "debug" | "dbg" => 3,
        _ => 2,
    };
    let _ = UMBRAL.set(n);
}

fn umbral() -> u8 {
    *UMBRAL.get_or_init(|| 2)
}

fn t0() -> Instant {
    *T0.get_or_init(Instant::now)
}

/// Registra una línea propia de este proceso.
pub fn log(nivel: Nivel, target: &str, msg: &str, campos: &[(&str, &str)]) {
    if nivel.umbral() > umbral() {
        return;
    }
    let (texto, color) = nivel.como_par();
    let segs = t0().elapsed().as_secs_f64();

    let mut linea = String::with_capacity(96 + msg.len() + campos.len() * 24);
    use std::fmt::Write as _;
    let _ = write!(
        linea,
        "\x1b[2m{segs:.9}s\x1b[0m {color}{texto}\x1b[0m \x1b[2m{target}\x1b[0m: {msg}"
    );
    for (k, v) in campos {
        let _ = write!(linea, " \x1b[3m{k}\x1b[0m\x1b[2m=\x1b[0m{v}");
    }
    linea.push('\n');

    let _ = std::io::stderr().write_all(linea.as_bytes());
}

/// Re-emite una línea cruda que vino de una sub-app (ya formateada por SU
/// logger) añadiendo campos de contexto del supervisor (`app=… canal=…`).
///
/// Así la terminal muestra la línea original de la sub-app intacta y al final
/// sabemos de qué app y por qué canal llegó.
pub fn reemitir(linea: &str, campos: &[(&str, &str)]) {
    let mut l = String::with_capacity(linea.len() + campos.len() * 24);
    l.push_str(linea);
    for (k, v) in campos {
        use std::fmt::Write as _;
        let _ = write!(l, " \x1b[3m{k}\x1b[0m\x1b[2m=\x1b[0m{v}");
    }
    l.push('\n');
    let _ = std::io::stderr().write_all(l.as_bytes());
}

#[macro_export]
macro_rules! log_debug {
    ($target:expr, $msg:expr $(, $k:expr => $v:expr)* $(,)?) => {
        $crate::log($crate::Nivel::Debug, $target, $msg, &[$(($k, $v),)*])
    };
}

#[macro_export]
macro_rules! log_info {
    ($target:expr, $msg:expr $(, $k:expr => $v:expr)* $(,)?) => {
        $crate::log($crate::Nivel::Info, $target, $msg, &[$(($k, $v),)*])
    };
}

#[macro_export]
macro_rules! log_warn {
    ($target:expr, $msg:expr $(, $k:expr => $v:expr)* $(,)?) => {
        $crate::log($crate::Nivel::Warn, $target, $msg, &[$(($k, $v),)*])
    };
}

#[macro_export]
macro_rules! log_error {
    ($target:expr, $msg:expr $(, $k:expr => $v:expr)* $(,)?) => {
        $crate::log($crate::Nivel::Error, $target, $msg, &[$(($k, $v),)*])
    };
}
