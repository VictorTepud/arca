//! Eventos de control del host → sub-app (probe F3a).
//!
//! El canal es stdio (líneas JSON compactas, el MISMO dialecto del probe
//! F0: ver `devapp-hello`). Este parser es a mano y sin alloc: extrae
//! slices de la línea prestada. Ante cualquier duda devuelve `None` y el
//! caller descarta la línea (fail-closed: un evento mal parseado es
//! mejor perderlo que inventarlo).
//!
//! Líneas que el HOST manda al hijo:
//! ```text
//! {"event":"touch","phase":"down","x":123,"y":456,"t":139333571}
//! {"event":"ping"}
//! {"event":"shutdown"}
//! ```
//! (El hijo → host usa su propio dialecto: frame/stats/pong/hello; ver
//! `devapp-demo`.)

/// Fase de un toque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Dedo puesto.
    Down,
    /// Dedo arrastrando.
    Move,
    /// Dedo levantado.
    Up,
}

/// Un evento de touch ya interpretado (coords en píxeles del FRAMEBUFFER
/// del hijo — el host ya escaló de pantalla a framebuffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    /// Fase del toque.
    pub phase: Phase,
    /// X en píxeles del framebuffer.
    pub x: u32,
    /// Y en píxeles del framebuffer.
    pub y: u32,
    /// Marca de tiempo del host (ms monótonos), si vino.
    pub t: Option<u64>,
}

/// Evento de control parseado de una línea de stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Toque en pantalla.
    Touch(Touch),
    /// Latido del host (el hijo responde pong: reuso del dialecto F0).
    Ping,
    /// Orden de apagado limpio (exit 0).
    Shutdown,
}

/// Parsea una línea de control. `None` = línea desconocida/corrupta.
#[must_use]
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    match field_str(line, "event")? {
        "touch" => {
            let phase = match field_str(line, "phase")? {
                "down" => Phase::Down,
                "move" => Phase::Move,
                "up" => Phase::Up,
                _ => return None,
            };
            let x = field_u64(line, "x")?;
            let y = field_u64(line, "y")?;
            if x > u32::MAX as u64 || y > u32::MAX as u64 {
                return None;
            }
            Some(Event::Touch(Touch {
                phase,
                x: x as u32,
                y: y as u32,
                t: field_u64(line, "t"),
            }))
        }
        "ping" => Some(Event::Ping),
        "shutdown" => Some(Event::Shutdown),
        _ => None,
    }
}

/// Extrae `"clave":"valor"` → `Some(valor)` (sin alloc: slice prestado).
fn field_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format_key(key);
    let at = find_key(line, &needle)?;
    let rest = &line[at..];
    let colon = rest.find(':')?;
    let mut s = &rest[colon + 1..];
    s = s.trim_start();
    if !s.starts_with('"') {
        return None;
    }
    let end = s[1..].find('"')?;
    Some(&s[1..1 + end])
}

/// Extrae `"clave":1234` → `Some(1234)`.
fn field_u64(line: &str, key: &str) -> Option<u64> {
    let needle = format_key(key);
    let at = find_key(line, &needle)?;
    let rest = &line[at..];
    let colon = rest.find(':')?;
    let s = rest[colon + 1..].trim_start();
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    s[..end].parse::<u64>().ok()
}

/// Busca la clave como `"key"` (con comillas) — evita falsos positivos
/// tipo `"next":"..."` cuando buscamos `"x"`.
fn find_key(line: &str, quoted_key: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = line[from..].find(quoted_key) {
        let at = from + rel;
        // debe estar delimitado por comillas a ambos lados del texto clave
        let after = at + quoted_key.len();
        let ok_before = at == 0 || line[..at].ends_with([',', '{']);
        let ok_after =
            line.get(after..after + 1) == Some(":") || line.get(after..after + 1) == Some("\"");
        if ok_before && ok_after {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// `"clave"` con comillas para [`find_key`].
fn format_key(key: &str) -> String {
    format!("\"{key}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_completo() {
        let ev = parse_line(r#"{"event":"touch","phase":"down","x":123,"y":456,"t":139333571}"#);
        assert_eq!(
            ev,
            Some(Event::Touch(Touch {
                phase: Phase::Down,
                x: 123,
                y: 456,
                t: Some(139_333_571),
            }))
        );
    }

    #[test]
    fn touch_sin_t_sigue_valido() {
        let ev = parse_line(r#"{"event":"touch","phase":"move","x":5,"y":6}"#);
        assert_eq!(
            ev,
            Some(Event::Touch(Touch {
                phase: Phase::Move,
                x: 5,
                y: 6,
                t: None,
            }))
        );
    }

    #[test]
    fn ping_y_shutdown() {
        assert_eq!(parse_line(r#"{"event":"ping"}"#), Some(Event::Ping));
        assert_eq!(
            parse_line(r#"  {"event":"shutdown"}  "#),
            Some(Event::Shutdown)
        );
    }

    #[test]
    fn fase_desconocida_descarta() {
        assert_eq!(
            parse_line(r#"{"event":"touch","phase":"cancel","x":1,"y":2}"#),
            None
        );
    }

    #[test]
    fn coordenadas_rotas_descartan() {
        assert_eq!(
            parse_line(r#"{"event":"touch","phase":"up","x":abc,"y":2}"#),
            None
        );
        assert_eq!(parse_line(r#"{"event":"touch","phase":"up","y":2}"#), None);
    }

    #[test]
    fn no_confunde_claves_parecidas() {
        // "x" como sufijo de otra clave ("max") NO debe matchear
        assert_eq!(
            parse_line(r#"{"event":"touch","phase":"down","max":9,"x":1,"y":2}"#),
            Some(Event::Touch(Touch {
                phase: Phase::Down,
                x: 1,
                y: 2,
                t: None,
            }))
        );
    }

    #[test]
    fn basura_y_eventos_de_salida_descartan() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("hola"), None);
        // líneas del HIJO (no del host) no son eventos de input:
        assert_eq!(parse_line(r#"{"event":"frame","seq":3}"#), None);
        assert_eq!(parse_line(r#"{"event":"pong","seq":3}"#), None);
    }
}
