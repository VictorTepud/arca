//! Widgets mínimos del probe (T21 trae la biblioteca real sobre egui).
//!
//! [`Button`] es lo único que el demo necesita: hit-test + render con
//! estados (normal/pressed). Todo se pinta con [`Canvas`] — sin estados
//! ocultos ni alloc: el "estado pressed" lo aporta el caller (la sub-app
//! sabe qué dedo tiene encima).

use crate::canvas::{Canvas, Color};
use crate::font::cell_w;

/// Botón rectangular con etiqueta estática.
#[derive(Debug, Clone)]
pub struct Button {
    /// Esquina superior izquierda (px del framebuffer).
    pub x: i32,
    /// Coordenada Y (px).
    pub y: i32,
    /// Ancho (px).
    pub w: i32,
    /// Alto (px).
    pub h: i32,
    /// Etiqueta (fuente bitmap del probe; los desconocidos salen `?`).
    pub label: &'static str,
    /// Color base (normal).
    pub base: Color,
    /// Color de etiqueta.
    pub ink: Color,
}

impl Button {
    /// ¿El punto está dentro? (para hit-test de touch).
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Pinta el botón. `pressed` lo aplasta visualmente (host define la
    /// lógica de presión: down dentro = pressed hasta up).
    pub fn draw(&self, canvas: &mut Canvas, pressed: bool) {
        let body = if pressed {
            self.base.lerp(Color::rgb(255, 255, 255), 1, 3)
        } else {
            self.base
        };
        let radius = (self.h / 4).max(3);
        canvas.fill_round_rect(self.x, self.y, self.w, self.h, radius, body);
        // relieve: borde inferior oscuro, superior claro
        canvas.fill_rect(
            self.x + radius,
            self.y + self.h - 2,
            self.w - 2 * radius,
            2,
            Color::rgb(0, 0, 0),
        );
        // etiqueta centrada: escala 2 solo si el texto cabe holgado
        let chars = self.label.chars().count() as u32;
        let escala2_cabe = self.w >= (chars * cell_w() as u32 * 2) as i32 + 8 && self.h >= 36;
        let scale = if escala2_cabe { 2 } else { 1 };
        let tw = chars * cell_w() as u32 * scale;
        let tx = self.x + (self.w as u32).saturating_sub(tw) as i32 / 2;
        let ty = self.y
            + (self.h as u32).saturating_sub(16 * scale) as i32 / 2
            + if pressed { 1 } else { 0 };
        canvas.draw_text(
            tx.max(self.x + 2),
            ty.max(self.y + 2),
            self.label,
            scale,
            self.ink,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    struct Lienzo {
        buf: Box<[u8]>,
        w: u32,
        h: u32,
    }

    impl Lienzo {
        fn new(w: u32, h: u32) -> Self {
            Self {
                buf: vec![0u8; w as usize * h as usize * 4].into_boxed_slice(),
                w,
                h,
            }
        }

        fn canvas(&mut self) -> Canvas<'_> {
            Canvas::new_rgba(&mut self.buf, self.w, self.h).expect("geometría del test")
        }

        fn px(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
            let at = (y as usize * self.w as usize + x as usize) * 4;
            (
                self.buf[at],
                self.buf[at + 1],
                self.buf[at + 2],
                self.buf[at + 3],
            )
        }
    }

    fn btn() -> Button {
        Button {
            x: 10,
            y: 20,
            w: 80,
            h: 30,
            label: "Hola",
            base: Color::rgb(40, 90, 200),
            ink: Color::rgb(255, 255, 255),
        }
    }

    #[test]
    fn hit_test_bordes() {
        let b = btn();
        assert!(b.contains(10, 20));
        assert!(b.contains(89, 49));
        assert!(!b.contains(90, 49), "borde derecho exclusivo");
        assert!(!b.contains(9, 20));
        assert!(!b.contains(10, 50), "borde inferior exclusivo");
    }

    #[test]
    fn draw_pinta_cuerpo_y_borde() {
        let mut l = Lienzo::new(120, 60);
        l.canvas().fill(Color::rgb(0, 0, 0));
        let b = btn();
        b.draw(&mut l.canvas(), false);
        // centro del cuerpo (fila 35, col 50): azul base
        assert_eq!(l.px(50, 35), (40, 90, 200, 255), "cuerpo del botón");
        // borde inferior oscuro (fila 48 = y+h-2)
        assert_eq!(l.px(50, 48), (0, 0, 0, 255), "sombra inferior");
        // pressed = mezcla hacia blanco (1/3)
        b.draw(&mut l.canvas(), true);
        let (r, g, bl, _) = l.px(50, 35);
        assert_eq!(
            (r, g, bl),
            (
                40 + (255 - 40) / 3,
                90 + (255 - 90) / 3,
                200 + (255 - 200) / 3
            )
        );
    }
}
