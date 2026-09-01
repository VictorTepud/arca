//! `arca-sdk-ui` — dibujo y widgets para las apps del probe F3a.
//!
//! Capa L2 · unsafe: **no** (100 % Rust seguro: pinta sobre `&mut [u8]`).
//! Contrato completo: `specs/arca-23-*.md` del blueprint (T21); esto es la
//! mitad de bajo nivel que el probe visual necesita ANTES de egui: un
//! canvas software RGBA, fuente bitmap, parser de input y botones.
//!
//! # Por qué canvas software y no GPU (decisión F3a)
//!
//! Las sub-apps del probe son ELF estáticos-PIE **sin NDK** (la restricción
//! que hace viable la grieta de targetSdk 28): linkear GLES/Vulkan sería
//! reintroducir el NDK por la ventana. El pixel a píxel en CPU + un
//! framebuffer compartido (`arca-shm`) es el mínimo que demuestra el
//! pipeline completo C→H→pantalla; el pipeline real (meshes, egui) llega
//! en F3b sobre esta misma fsm.
//!
//! # Reglas del path de frame (AGENTS §7)
//!
//! Ningún método de [`Canvas`] alocа: solo recorre `&mut [u8]` existente.
//! El parser de input sí puede ver `&str` prestados (path de control, no
//! de frame).
//!
//! # Piezas
//!
//! - [`Canvas`]: rasterizador (rects, discos, blits con alfa, texto).
//! - [`font`]: glifos bitmap 12×16 (DejaVu rasterizada, ver `font_data`).
//! - [`input`]: eventos del host (touch/ping/shutdown) desde líneas JSON.
//! - [`widgets`]: [`Button`] con hit-test y estados.
//! - [`paint`]: pinta un frame completo de framebuffer (cabecera + bitmap)
//!   en el payload de un slot de `arca-shm`.

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod canvas;
pub mod font;
pub mod input;
pub mod paint;
pub mod widgets;

pub use canvas::{Canvas, Color, RgbaImg};
pub use font::{cell_h, cell_w, glyph, Glyph};
pub use input::{parse_line, Event, Phase, Touch};
pub use paint::paint_frame;
pub use widgets::Button;
