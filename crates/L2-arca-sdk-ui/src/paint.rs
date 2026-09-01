//! Pintado de un frame de framebuffer completo dentro del payload de un
//! slot de `arca-shm`.
//!
//! Contrato del payload (L0 `arca-gfx-protocol`):
//! `[FrameHeader (32 B)][bitmap w×h×4]`. Esta función valida la geometría,
//! escribe la cabecera, presta el bitmap como [`Canvas`] y llama al cierre.
//! El `publish()` del slot lo hace el caller (arca-shm): aquí solo se
//! PINTA — separación deliberada para poder testear sin shm.

use arca_gfx_protocol::{FrameHeader, HDR_BYTES};
use thiserror::Error;

use crate::canvas::Canvas;

/// Re-export de conveniencia para que las apps solo importen `paint`.
pub use arca_gfx_protocol::FbError;
/// Errores de [`paint_frame`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PaintError {
    /// El payload no mide lo que la cabecera declara (geometría acordada
    /// con el host rota: NO publicar el frame).
    #[error("paint: payload de {got} B ≠ frame de {want} B (w={w}, h={h})")]
    SizeMismatch {
        /// Ancho declarado.
        w: u16,
        /// Alto declarado.
        h: u16,
        /// Bytes esperados (cabecera + bitmap).
        want: usize,
        /// Bytes del payload recibido.
        got: usize,
    },
}

/// Pinta un frame: valida tamaño, escribe cabecera y dibuja con el canvas
/// prestado. Sin alloc (la cabecera se serializa en pila).
///
/// ```text
/// paint_frame(payload, &hdr, |c| { c.fill(...); ... })  // → Ok(())
/// ```
pub fn paint_frame<F>(payload: &mut [u8], hdr: &FrameHeader, painter: F) -> Result<(), PaintError>
where
    F: FnOnce(&mut Canvas),
{
    let want = hdr.frame_bytes();
    let got = payload.len();
    if got != want {
        return Err(PaintError::SizeMismatch {
            w: hdr.width,
            h: hdr.height,
            want,
            got,
        });
    }
    let mut hbuf = [0u8; HDR_BYTES];
    hdr.encode_into(&mut hbuf);
    payload[..HDR_BYTES].copy_from_slice(&hbuf);
    let (h_bytes, px_bytes) = payload.split_at_mut(HDR_BYTES);
    debug_assert_eq!(h_bytes.len(), HDR_BYTES);
    // Invariante: ya validamos payload.len() == HDR + w*h*4, así que el
    // canvas NUNCA puede fallar aquí; el map_err es pura defensa.
    let mut canvas = Canvas::new_rgba(px_bytes, u32::from(hdr.width), u32::from(hdr.height))
        .map_err(|_| PaintError::SizeMismatch {
            w: hdr.width,
            h: hdr.height,
            want,
            got,
        })?;
    painter(&mut canvas);
    Ok(())
}

// (sin re-exports extra: las apps importan Canvas/Button de sus módulos —
// re-exportar aliases de types ajenos solo confunde el grafo de uso)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Color;
    use crate::font::cell_w;
    use crate::widgets::Button;

    #[test]
    fn pinta_cabecera_y_pixeles() {
        let hdr = FrameHeader::new_opaque(8, 4, 7, 123);
        let mut payload = vec![0u8; hdr.frame_bytes()];
        paint_frame(&mut payload, &hdr, |c| {
            c.fill(Color::rgb(9, 8, 7));
        })
        .expect("pinta");
        // cabecera al inicio
        assert_eq!(&payload[..4], b"AFRM");
        assert_eq!(payload[8..10], 8u16.to_le_bytes());
        // primer píxel
        assert_eq!(&payload[HDR_BYTES..HDR_BYTES + 4], &[9, 8, 7, 255]);
    }

    #[test]
    fn payload_de_tamano_equivocado_falla_cerrado() {
        let hdr = FrameHeader::new_opaque(8, 4, 7, 123);
        let mut payload = vec![0u8; hdr.frame_bytes() + 1];
        assert!(paint_frame(&mut payload, &hdr, |_| {}).is_err());
        let mut corto = vec![0u8; hdr.frame_bytes() - 1];
        assert!(paint_frame(&mut corto, &hdr, |_| {}).is_err());
    }

    #[test]
    fn el_canvas_dentro_ve_dimensiones_correctas() {
        let hdr = FrameHeader::new_opaque(11, 5, 1, 0);
        let mut payload = vec![0u8; hdr.frame_bytes()];
        let mut visto = (0u32, 0u32);
        paint_frame(&mut payload, &hdr, |c| {
            visto = (c.width(), c.height());
        })
        .expect("pinta");
        assert_eq!(visto, (11, 5));
    }

    #[test]
    fn compose_de_botones_cabe_en_el_payload() {
        // humo del demo: un botón dentro de un frame pintado por paint_frame
        let hdr = FrameHeader::new_opaque(64, 40, 1, 0);
        let mut payload = vec![0u8; hdr.frame_bytes()];
        let b = Button {
            x: 4,
            y: 10,
            w: 48,
            h: 20,
            label: "Ok",
            base: Color::rgb(30, 120, 90),
            ink: Color::rgb(255, 255, 255),
        };
        paint_frame(&mut payload, &hdr, |c| {
            c.fill(Color::rgb(12, 12, 14));
            b.draw(c, false);
        })
        .expect("pinta");
        // etiqueta dentro del área del botón (aprox centro)
        let cx = 4 + 48 / 2;
        let cy = 10 + 20 / 2;
        let at = HDR_BYTES + (cy * 64 + cx) * 4;
        assert_ne!(
            &payload[at..at + 3],
            &[12, 12, 14],
            "centro del botón no es fondo"
        );
        // ancho de "Ok" a escala 1 = 24 px — cabe en 48
        assert!(2 * cell_w() <= 48);
    }
}
