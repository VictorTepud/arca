//! Fuente bitmap del probe: lookup de glifos y medidas.
//!
//! Los datos viven en `font_data.rs` (generados por
//! `devapp-demo/tools/gen_font.py`); este módulo solo expone la lectura.
//! Avance monoespaciado = [`cell_w`] píxeles por carácter (las apps del
//! probe no hacen kerning: predictible > bonito).

mod font_data;

pub use font_data::{ASCII_FONT, EXTRA_FONT};

/// Ancho de celda = avance monoespaciado (px).
#[must_use]
pub const fn cell_w() -> usize {
    font_data::FONT_CELL_W
}

/// Alto de celda (px, incluye ascensores y descendentes).
#[must_use]
pub const fn cell_h() -> usize {
    font_data::FONT_CELL_H
}

/// Bytes por fila de glifo (filas de `cell_w()` bits, big-endian).
#[must_use]
pub const fn row_bytes() -> usize {
    font_data::FONT_ROW_BYTES
}

/// Un glifo: `row_bytes() * cell_h()` bytes, fila por fila, bit más alto
/// de cada fila = píxel más a la izquierda.
pub type Glyph = [u8; font_data::FONT_ROW_BYTES * font_data::FONT_CELL_H];

/// Glifo de `ch`, o `None` si la fuente no lo trae (el caller decide: el
/// canvas pinta `?` o salta). Sin alocar (todo está en estáticos).
#[must_use]
pub fn glyph(ch: char) -> Option<&'static Glyph> {
    if let Some(idx) = ascii_index(ch) {
        // Índice acotado por construcción (ascii_index valida 32..=126).
        return Some(&ASCII_FONT[idx]);
    }
    // Castellano extra: búsqueda lineal (14 entradas; sin HashMap para no
    // alocar en el binario estático del probe).
    EXTRA_FONT.iter().find(|(c, _)| *c == ch).map(|(_, g)| g)
}

/// Índice en [`ASCII_FONT`] si `ch` es ASCII imprimible.
const fn ascii_index(ch: char) -> Option<usize> {
    let c = ch as u32;
    // (comparación manual: `contains` de Range no es const-stable todavía)
    if c >= 32 && c <= 126 {
        Some((c - 32) as usize)
    } else {
        None
    }
}
/// Ancho de `n` caracteres a escala `scale` (px) — para centrar textos.
#[must_use]
pub const fn text_width(n: usize, scale: u32) -> u32 {
    n as u32 * cell_w() as u32 * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_completo_mapea() {
        for c in 32..=126u32 {
            let ch = char::from_u32(c).expect("ascii válido");
            assert!(glyph(ch).is_some(), "falta glifo {ch:?}");
        }
    }

    #[test]
    fn castellano_extra_mapea() {
        for ch in "áéíóúñÁÉÍÓÚÑ¿¡".chars() {
            assert!(glyph(ch).is_some(), "falta glifo extra {ch:?}");
        }
    }

    #[test]
    fn desconocido_no_mapea() {
        assert!(glyph('β').is_none());
        assert!(glyph('\t').is_none());
        assert!(glyph('\u{1F600}').is_none());
    }

    #[test]
    fn glifo_o_es_un_anillo_izq_der() {
        // 'o' renderizado: trazos laterales en col ~1 y ~6-7, hueco al centro
        // (columnas agregadas sobre todas las filas del glifo).
        let o = glyph('o').expect("o");
        let cols = columnas(os(o));
        assert!(cols[1] || cols[2], "o sin trazo izquierdo");
        assert!(cols[6] || cols[7], "o sin trazo derecho");
        // fila del "cintura" del anillo: col 4 vacía
        let fila = filas_de(os(o));
        let cintura = fila[7]; // '.#...##' → bit de col4 = 0
        assert_eq!(
            cintura & (1 << (font_data::FONT_CELL_W - 1 - 4)),
            0,
            "sin hueco"
        );
    }

    #[test]
    fn glifo_a_tiene_masa() {
        let a = glyph('A').expect("A");
        let cols = columnas(os(a));
        assert!(cols.iter().filter(|&&c| c).count() >= 4, "A muy flaca");
        assert!(cols[0] || cols[1], "A sin pata izquierda");
        assert!(cols[6] || cols[7] || cols[8], "A sin pata derecha");
    }

    /// Bytes de un glifo → filas como u16 (bits alineados a la izquierda).
    fn os(g: &Glyph) -> [u16; font_data::FONT_CELL_H] {
        let mut rows = [0u16; font_data::FONT_CELL_H];
        for (r, chunk) in g.chunks(font_data::FONT_ROW_BYTES).enumerate() {
            let mut bits = 0u16;
            for (k, b) in chunk.iter().enumerate() {
                bits |= u16::from(*b) << (8 * (font_data::FONT_ROW_BYTES - 1 - k));
            }
            rows[r] = bits;
        }
        rows
    }

    /// Filas → columnas con tinta (true = algún píxel en esa columna).
    fn columnas(rows: [u16; font_data::FONT_CELL_H]) -> [bool; font_data::FONT_CELL_W] {
        let mut cols = [false; font_data::FONT_CELL_W];
        for (x, col) in cols.iter_mut().enumerate() {
            let bit = 1 << (font_data::FONT_CELL_W - 1 - x);
            *col = rows.iter().any(|&row| row & bit != 0);
        }
        cols
    }

    /// Alias de [`os`] legible en el test de la A.
    fn filas_de(rows: [u16; font_data::FONT_CELL_H]) -> [u16; font_data::FONT_CELL_H] {
        rows
    }

    #[test]
    fn medidas_celdas() {
        assert_eq!(cell_w(), 12);
        assert_eq!(cell_h(), 16);
        assert_eq!(text_width(3, 2), 3 * 12 * 2);
    }
}
