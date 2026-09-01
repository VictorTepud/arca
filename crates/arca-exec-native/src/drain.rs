//! drain (módulo de arca-exec-native): drenado de stdout/stderr de las sub-apps.
//!
//! Cada pipe tiene su hilo de drenaje que:
//! 1. lee línea por línea hasta EOF,
//! 2. guarda una copia cruda en `captura` (para que los tests verifiquen),
//! 3. re-emite la línea a stderr del supervisor con contexto añadido:
//!    `… app=dev.arca.ping canal="stderr"`.
//!
//! La línea de la sub-app ya viene formateada por SU logger (marca de tiempo,
//! nivel y target propios), así que aquí no la tocamos: solo agregamos al
//! final quién la dijo y por dónde llegó.

use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use arca_log::reemitir;

/// Quita las secuencias de color ANSI (ESC[…m) de una línea.
/// La terminal sigue viendo colores vía re-emisión; la copia capturada queda
/// limpia para que los tests (y futuras herramientas) puedan buscar texto plano.
fn sin_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some('[') = chars.next() {
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break; // fin de la secuencia
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Lanza el hilo de drenaje para un pipe de la sub-app.
pub fn lanzar<R: Read + Send + 'static>(
    pipe: R,
    app: String,
    instancia: u32,
    canal: &'static str,
    captura: Arc<Mutex<Vec<String>>>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("drain-{instancia}-{canal}"))
        .spawn(move || {
            arca_log::init();
            let mut r = BufReader::new(pipe);
            let mut linea = String::new();
            let canal_q = format!("\"{canal}\"");
            loop {
                linea.clear();
                match r.read_line(&mut linea) {
                    Ok(0) => break, // EOF: la sub-app cerró el pipe (murió)
                    Ok(_) => {
                        let limpia = linea.trim_end_matches(['\n', '\r']);
                        if limpia.is_empty() {
                            continue;
                        }
                        if let Ok(mut c) = captura.lock() {
                            c.push(sin_ansi(limpia));
                        }
                        reemitir(
                            limpia,
                            &[
                                ("app", app.as_str()),
                                ("canal", canal_q.as_str()),
                            ],
                        );
                    }
                    Err(e) => {
                        arca_log::log_warn!("arca::exec-native::drain", "error leyendo pipe",
                                            "canal" => canal, "error" => &e.to_string());
                        break;
                    }
                }
            }
        })
        .expect("lanzar hilo drain")
}
