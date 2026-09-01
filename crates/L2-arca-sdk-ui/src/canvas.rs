//! Rasterizador software RGBA8888 (top-down, stride = ancho*4).
//!
//! Pinta sobre un `&mut [u8]` prestado (el payload del slot de shm en el
//! probe F3a): **cero alocaciones** y clipping en todas las primitivas —
//! un rect fuera de pantalla se recorta, nunca paniquea (las coords vienen
//! de un host que puede mandar cualquier cosa).

use crate::font::{cell_w, glyph, row_bytes};
use thiserror::Error;

/// Color RGBA8888 (los 4 bytes en el orden exacto del framebuffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    /// Rojo 0-255.
    pub r: u8,
    /// Verde 0-255.
    pub g: u8,
    /// Azul 0-255.
    pub b: u8,
    /// Alfa 0-255 (255 = opaco).
    pub a: u8,
}

impl Color {
    /// Color opaco.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Mezcla lineal `self`→`other` en `t/t_max` (sin alloc, para degradados).
    /// `t_max` 0 se trata como 1 (degenerado: t=0).
    pub const fn lerp(self, other: Self, t: u32, t_max: u32) -> Self {
        let tm = if t_max == 0 { 1 } else { t_max };
        Self {
            r: mix_u8(self.r, other.r, t, tm),
            g: mix_u8(self.g, other.g, t, tm),
            b: mix_u8(self.b, other.b, t, tm),
            a: mix_u8(self.a, other.a, t, tm),
        }
    }
}

/// Mezcla un canal: `a + (b-a)*t/tm`, saturado (const fn de ayuda).
const fn mix_u8(a: u8, b: u8, t: u32, tm: u32) -> u8 {
    let d = (b as u32).saturating_sub(a as u32);
    let v = (a as u32).saturating_add(d.saturating_mul(t) / tm);
    if v > 255 {
        255
    } else {
        v as u8
    }
}

/// Imagen RGBA8888 prestada (top-down, sin padding): lo que blitean los
/// demos (logo embebido) y lo que lee el host del payload de un slot.
#[derive(Debug, Clone, Copy)]
pub struct RgbaImg<'a> {
    /// Ancho en píxeles.
    pub w: u32,
    /// Alto en píxeles.
    pub h: u32,
    /// Píxeles (w×h×4, R,G,B,A).
    pub data: &'a [u8],
}

impl<'a> RgbaImg<'a> {
    /// Envuelve un buffer RGBA (valida que el tamaño cuadre: fail-closed).
    pub fn new(w: u32, h: u32, data: &'a [u8]) -> Option<Self> {
        if w == 0 || h == 0 || data.len() != w as usize * h as usize * 4 {
            return None;
        }
        Some(Self { w, h, data })
    }
}

/// Errores de [`Canvas`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanvasError {
    /// El buffer no mide `w*h*4` (contrato RGBA8888 sin padding).
    #[error("canvas: buffer de {got} B ≠ {w}×{h}×4 = {want} B")]
    BadSize {
        /// Ancho declarado.
        w: u32,
        /// Alto declarado.
        h: u32,
        /// Bytes esperados.
        want: usize,
        /// Bytes recibidos.
        got: usize,
    },
}

/// Lienzo RGBA8888 sobre memoria prestada.
pub struct Canvas<'a> {
    buf: &'a mut [u8],
    w: u32,
    h: u32,
}

impl<'a> Canvas<'a> {
    /// Envuelve `buf` como bitmap `w×h`. Error si el tamaño no cuadra
    /// (fail-closed: un canvas cojo escribiría fuera del slot).
    pub fn new_rgba(buf: &'a mut [u8], w: u32, h: u32) -> Result<Self, CanvasError> {
        if w == 0 || h == 0 {
            return Err(CanvasError::BadSize {
                w,
                h,
                want: 0,
                got: buf.len(),
            });
        }
        let want = w as usize * h as usize * 4;
        if buf.len() != want {
            return Err(CanvasError::BadSize {
                w,
                h,
                want,
                got: buf.len(),
            });
        }
        Ok(Self { buf, w, h })
    }

    /// Ancho en píxeles.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.w
    }

    /// Alto en píxeles.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.h
    }

    /// Rellena TODO el lienzo con un color opaco (el fondo del frame).
    /// El alfa del color se ignora a propósito: un fondo translúcido no
    /// tiene sentido sin composición previa (no hay "debajo").
    pub fn fill(&mut self, color: Color) {
        let px = [color.r, color.g, color.b, 255];
        for chunk in self.buf.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    /// Relleno vertical degradado (per-row lerp): la base del fondo animado.
    pub fn fill_vgrad(&mut self, top: Color, bottom: Color) {
        let hmax = self.h.saturating_sub(1);
        for y in 0..self.h {
            let c = top.lerp(bottom, y, hmax.max(1));
            let row = y as usize * self.w as usize * 4;
            for x in 0..self.w as usize {
                let at = row + x * 4;
                self.buf[at] = c.r;
                self.buf[at + 1] = c.g;
                self.buf[at + 2] = c.b;
                self.buf[at + 3] = 255;
            }
        }
    }

    /// Rectángulo sólido con clipping total. Coordenadas negativas válidas
    /// (el caller anima cosas que entran/salen de pantalla).
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: Color) {
        if w <= 0 || h <= 0 {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.w as i32);
        let y1 = (y + h).min(self.h as i32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        if color.a == 255 {
            let px = [color.r, color.g, color.b, 255];
            for yy in y0..y1 {
                let row = yy as usize * self.w as usize * 4;
                for xx in x0..x1 {
                    let at = row + xx as usize * 4;
                    self.buf[at..at + 4].copy_from_slice(&px);
                }
            }
        } else {
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let at = (yy as usize * self.w as usize + xx as usize) * 4;
                    self.blend_at(at, color);
                }
            }
        }
    }

    /// Contorno de rectángulo (grosor en px, hacia adentro).
    pub fn draw_rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, thick: i32, color: Color) {
        if w <= 0 || h <= 0 || thick <= 0 {
            return;
        }
        let t = thick.min(w / 2).max(1).min(h / 2).max(1);
        self.fill_rect(x, y, w, t, color); // top
        self.fill_rect(x, y + h - t, w, t, color); // bottom
        self.fill_rect(x, y + t, t, h - 2 * t, color); // left
        self.fill_rect(x + w - t, y + t, t, h - 2 * t, color); // right
    }

    /// Rectángulo redondeado sólido (radio ≤ min(w,h)/2, recortado solo a
    /// píxeles del disco de esquina — sin alocar spans).
    pub fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: Color) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = radius.clamp(0, w.min(h) / 2);
        if r == 0 {
            self.fill_rect(x, y, w, h, color);
            return;
        }
        let r2 = (r * r) as f32;
        for yy in y..y + h {
            for xx in x..x + w {
                // Distancia al núcleo del rect (0 si estamos en la banda
                // central): solo las esquinas se recortan contra el disco.
                let dx = if xx < x + r {
                    x + r - xx
                } else if xx > x + w - 1 - r {
                    xx - (x + w - 1 - r)
                } else {
                    0
                };
                let dy = if yy < y + r {
                    y + r - yy
                } else if yy > y + h - 1 - r {
                    yy - (y + h - 1 - r)
                } else {
                    0
                };
                if dx > 0 && dy > 0 {
                    let d2 = (dx * dx + dy * dy) as f32;
                    if d2 > r2 {
                        continue;
                    }
                }
                self.set_pixel(xx, yy, color);
            }
        }
    }

    /// Disco (círculo relleno) con centro y radio en píxeles.
    pub fn fill_disc(&mut self, cx: i32, cy: i32, radius: i32, color: Color) {
        if radius <= 0 {
            return;
        }
        let r2 = radius * radius;
        for yy in cy - radius..=cy + radius {
            for xx in cx - radius..=cx + radius {
                let dx = xx - cx;
                let dy = yy - cy;
                if dx * dx + dy * dy <= r2 {
                    self.set_pixel(xx, yy, color);
                }
            }
        }
    }

    /// Píxel con clipping (noop fuera del lienzo). El bloque básico de todo
    /// lo demás.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let at = (y as usize * self.w as usize + x as usize) * 4;
        if color.a == 255 {
            self.buf[at] = color.r;
            self.buf[at + 1] = color.g;
            self.buf[at + 2] = color.b;
            self.buf[at + 3] = 255;
        } else if color.a > 0 {
            self.blend_at(at, color);
        }
    }

    /// Blit RGBA (top-down) con mezcla por alfa. `src` puede traer
    /// transparencias (el logo del demo usa destellos suaves).
    pub fn blit(&mut self, x: i32, y: i32, img: RgbaImg) {
        if img.w == 0 || img.h == 0 {
            return;
        }
        for row in 0..img.h as i32 {
            for col in 0..img.w as i32 {
                let sat = (row * img.w as i32 + col) as usize * 4;
                if sat + 4 > img.data.len() {
                    return; // src corto: fail-safe (no crash)
                }
                let c = Color {
                    r: img.data[sat],
                    g: img.data[sat + 1],
                    b: img.data[sat + 2],
                    a: img.data[sat + 3],
                };
                if c.a == 0 {
                    continue;
                }
                self.set_pixel(x + col, y + row, c);
            }
        }
    }

    /// Blit escalado nearest-neighbor (destino `dw×dh` desde el tamaño de
    /// `img`). El "zoom" del demo (logo 96→144) sin interpolar: barato y
    /// honesto.
    pub fn blit_scaled(&mut self, x: i32, y: i32, dw: i32, dh: i32, img: RgbaImg) {
        if dw <= 0 || dh <= 0 || img.w == 0 || img.h == 0 || img.data.is_empty() {
            return;
        }
        for dy in 0..dh {
            let sy = ((dy as u64 * img.h as u64) / dh as u64) as u32;
            for dx in 0..dw {
                let sx = ((dx as u64 * img.w as u64) / dw as u64) as u32;
                let sat = (sy as usize * img.w as usize + sx as usize) * 4;
                if sat + 4 > img.data.len() {
                    return;
                }
                let c = Color {
                    r: img.data[sat],
                    g: img.data[sat + 1],
                    b: img.data[sat + 2],
                    a: img.data[sat + 3],
                };
                if c.a == 0 {
                    continue;
                }
                self.set_pixel(x + dx, y + dy, c);
            }
        }
    }

    /// Texto bitmap con escala (1 = 12×16 px/char). Pinta `?` por glifo
    /// desconocido. Devuelve el ancho pintado (px) para encadenar.
    pub fn draw_text(&mut self, x: i32, y: i32, text: &str, scale: u32, color: Color) -> u32 {
        let mut pen = x;
        for ch in text.chars() {
            let g = glyph(ch).or_else(|| glyph('?'));
            if let Some(g) = g {
                for (ry, rowb) in g.chunks(row_bytes()).enumerate() {
                    let mut bits = 0u16;
                    for (k, b) in rowb.iter().enumerate() {
                        bits |= u16::from(*b) << (8 * (row_bytes() - 1 - k));
                    }
                    for rx in 0..cell_w() {
                        if bits & (1 << (cell_w() - 1 - rx)) != 0 {
                            let px = pen + rx as i32 * scale as i32;
                            let py = y + ry as i32 * scale as i32;
                            if scale == 1 {
                                self.set_pixel(px, py, color);
                            } else {
                                self.fill_rect(px, py, scale as i32, scale as i32, color);
                            }
                        }
                    }
                }
            }
            pen += cell_w() as i32 * scale as i32;
        }
        (pen - x).max(0) as u32
    }

    /// Mezcla por alfa sobre `at` (precondición: dentro del buffer).
    fn blend_at(&mut self, at: usize, c: Color) {
        if at + 4 > self.buf.len() {
            return;
        }
        let a = c.a as u32;
        let inv = 255 - a;
        let dst = &self.buf[at..at + 4];
        let dr = u32::from(dst[0]);
        let dg = u32::from(dst[1]);
        let db = u32::from(dst[2]);
        self.buf[at] = ((a * c.r as u32 + inv * dr) / 255) as u8;
        self.buf[at + 1] = ((a * c.g as u32 + inv * dg) / 255) as u8;
        self.buf[at + 2] = ((a * c.b as u32 + inv * db) / 255) as u8;
        self.buf[at + 3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lienzo de test: dueño del buffer; presta el [`Canvas`] por préstamo.
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

        /// Canvas prestado sobre el buffer propio (una draw por statement).
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

    #[test]
    fn tamano_incorrecto_se_rechaza() {
        let mut buf = [0u8; 16];
        assert!(Canvas::new_rgba(&mut buf, 3, 3).is_err()); // 36 ≠ 16
        assert!(Canvas::new_rgba(&mut buf, 2, 2).is_ok());
        assert!(Canvas::new_rgba(&mut buf, 0, 4).is_err());
    }

    #[test]
    fn fill_rellena_todo_opaco() {
        let mut l = Lienzo::new(4, 4);
        l.canvas().fill(Color::rgb(10, 20, 30));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(l.px(x, y), (10, 20, 30, 255));
            }
        }
    }

    #[test]
    fn fill_rect_clipea_fuera_de_pantalla() {
        let mut l = Lienzo::new(10, 10);
        l.canvas().fill(Color::rgb(0, 0, 0));
        l.canvas().fill_rect(-5, -5, 10, 10, Color::rgb(255, 0, 0)); // esquina sup-izq
        l.canvas().fill_rect(20, 20, 5, 5, Color::rgb(0, 255, 0)); // fuera total: noop
        l.canvas().fill_rect(5, 5, 100, 2, Color::rgb(0, 0, 255)); // sobredimensionado
        assert_eq!(l.px(0, 0), (255, 0, 0, 255));
        assert_eq!(
            l.px(4, 4),
            (255, 0, 0, 255),
            "(4,4) dentro del rojo clipeado"
        );
        assert_eq!(l.px(5, 5), (0, 0, 255, 255));
        assert_eq!(l.px(5, 6), (0, 0, 255, 255));
        assert_eq!(l.px(9, 6), (0, 0, 255, 255), "esquina inf-der del azul");
        assert_eq!(
            l.px(9, 9),
            (0, 0, 0, 255),
            "el azul mide 2 filas: (9,9) fuera"
        );
        assert_eq!(l.px(0, 9), (0, 0, 0, 255), "fila 9 fuera del rect azul");
    }

    #[test]
    fn disco_y_contorno() {
        let mut l = Lienzo::new(21, 21);
        l.canvas().fill(Color::rgb(0, 0, 0));
        l.canvas().fill_disc(10, 10, 5, Color::rgb(255, 255, 255));
        assert_eq!(l.px(10, 10), (255, 255, 255, 255), "centro");
        assert_eq!(l.px(5, 10), (255, 255, 255, 255), "borde izq");
        assert_eq!(l.px(4, 10), (0, 0, 0, 255), "fuera del disco");
        assert_eq!(l.px(0, 0), (0, 0, 0, 255), "esquina lejos");
        l.canvas()
            .draw_rect_outline(2, 2, 17, 17, 1, Color::rgb(0, 255, 0));
        assert_eq!(l.px(2, 2), (0, 255, 0, 255));
        assert_eq!(l.px(3, 3), (0, 0, 0, 255), "dentro del contorno");
    }

    #[test]
    fn blit_con_alfa_mezcla_correcto() {
        let mut l = Lienzo::new(4, 2);
        l.canvas().fill(Color::rgb(100, 100, 100));
        let rojo50 = RgbaImg::new(1, 1, &[255u8, 0, 0, 128]).expect("1x1");
        l.canvas().blit(0, 0, rojo50);
        let (r, g, bl, _) = l.px(0, 0);
        assert_eq!((r, g, bl), (177, 49, 49), "(255*128+100*127)/255 por canal");
        let opaco = RgbaImg::new(1, 1, &[10u8, 20, 30, 255]).expect("1x1");
        l.canvas().blit(2, 0, opaco);
        assert_eq!(l.px(2, 0), (10, 20, 30, 255));
        let invisible = RgbaImg::new(1, 1, &[10u8, 20, 30, 0]).expect("1x1");
        l.canvas().blit(2, 0, invisible);
        assert_eq!(l.px(2, 0), (10, 20, 30, 255), "alfa 0 = noop");
    }

    #[test]
    fn blit_escalado_2x_duplica_pantalla() {
        let mut l = Lienzo::new(8, 4);
        l.canvas().fill(Color::rgb(0, 0, 0));
        // fuente 2×2: (0,0) rojo, (1,0) verde, fila 1 igual
        let src = [
            255u8, 0, 0, 255, 0, 255, 0, 255, //
            255u8, 0, 0, 255, 0, 255, 0, 255,
        ];
        let img = RgbaImg::new(2, 2, &src).expect("2x2");
        l.canvas().blit_scaled(0, 0, 4, 2, img);
        assert_eq!(l.px(0, 0), (255, 0, 0, 255));
        assert_eq!(l.px(1, 0), (255, 0, 0, 255), "duplicado en x");
        assert_eq!(l.px(2, 0), (0, 255, 0, 255));
        assert_eq!(l.px(2, 1), (0, 255, 0, 255), "duplicado en y");
    }

    #[test]
    fn rgba_img_rechaza_datos_cortos() {
        assert!(RgbaImg::new(2, 2, &[0u8; 15]).is_none());
        assert!(RgbaImg::new(0, 2, &[0u8; 8]).is_none());
        assert!(RgbaImg::new(1, 1, &[0u8; 4]).is_some());
    }

    #[test]
    fn texto_pinta_pixeles_y_avanza() {
        let mut l = Lienzo::new(60, 20);
        l.canvas().fill(Color::rgb(0, 0, 0));
        let w = l
            .canvas()
            .draw_text(0, 2, "II", 1, Color::rgb(255, 255, 255));
        assert_eq!(w, 2 * 12, "2 celdas de 12 px");
        let mut encendidos = 0;
        for y in 0..20u32 {
            for x in 0..60u32 {
                if l.px(x, y).0 == 255 {
                    encendidos += 1;
                }
            }
        }
        assert!(encendidos > 10, "texto invisible ({encendidos} px)");
    }

    #[test]
    fn degradado_extremos_y_medio() {
        let mut l = Lienzo::new(3, 9);
        l.canvas()
            .fill_vgrad(Color::rgb(0, 0, 0), Color::rgb(90, 90, 90));
        assert_eq!(l.px(0, 0).0, 0);
        assert_eq!(l.px(0, 8).0, 90);
        let mid = l.px(0, 4).0;
        assert!((35..=55).contains(&mid), "fila media ≈ 45 (fue {mid})");
    }

    #[test]
    fn rect_redondeado_no_pinta_esquinas() {
        let mut l = Lienzo::new(20, 20);
        l.canvas().fill(Color::rgb(0, 0, 0));
        l.canvas()
            .fill_round_rect(0, 0, 20, 20, 6, Color::rgb(255, 255, 255));
        assert_eq!(l.px(10, 10), (255, 255, 255, 255), "centro");
        assert_eq!(l.px(0, 0), (0, 0, 0, 255), "esquina recortada");
        assert_eq!(l.px(19, 19), (0, 0, 0, 255), "esquina opuesta");
        assert_eq!(
            l.px(3, 0),
            (0, 0, 0, 255),
            "fila 0: solo la banda plana [r,w-r)"
        );
        assert_eq!(l.px(6, 0), (255, 255, 255, 255), "inicio de la banda plana");
        assert_eq!(
            l.px(10, 0),
            (255, 255, 255, 255),
            "centro de la banda plana"
        );
    }
}
