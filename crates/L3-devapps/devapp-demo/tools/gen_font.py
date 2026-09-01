#!/usr/bin/env python3
"""gen_font.py — rasteriza DejaVuSans a una fuente bitmap para sdk-ui.

Salida: crates/L2-arca-sdk-ui/src/font_data.rs (solo datos, sin lógica).
Formato por glifo: celda CELL_W×CELL_H px; cada fila se empaqueta en
ROW_BYTES bytes big-endian (bit más alto = píxel más a la izquierda).
ASCII imprimible 32..=126 en un array contiguo + un array extra con el
castellano que el demo usa (á é í ó ú ñ ¿ ¡ y mayúsculas).

Tamaño 12 px con celda 10×16: mayúsculas ~7 px, descensores incluidos —
legible a escala 1 y cómodo a escala 2. Regla anti-glifo-vacío: si algún
carácter queda en blanco o no cabe en la celda, el script FALLA (una
letra invisible en el teléfono es un bug del host, no un aviso).
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[4]
OUT = ROOT / "crates/L2-arca-sdk-ui/src/font_data.rs"
FONT_PATH = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
SIZE = 12         # px de la fuente TrueType
CELL_W, CELL_H = 12, 16
ROW_BYTES = (CELL_W + 7) // 8   # 10 px → 2 bytes por fila

ASCII = "".join(chr(c) for c in range(32, 127))
EXTRA = "áéíóúñÁÉÍÓÚÑ¿¡"


def raster(char: str) -> list[int]:
    """Dibuja `char` y devuelve las CELL_H filas como enteros de CELL_W bits."""
    img = Image.new("L", (CELL_W * 2, CELL_H * 2), 0)
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT_PATH, SIZE)
    d.text((0, 0), char, fill=255, font=font)
    dark = sum(1 for px in img.getdata() if px >= 64)
    if char != " " and dark == 0:
        raise SystemExit(f"glifo vacío: {char!r} (font/size no sirven)")
    xs = [x for y in range(img.height) for x in range(img.width)
          if img.getpixel((x, y)) >= 64]
    if xs and max(xs) >= CELL_W:
        raise SystemExit(f"glifo más ancho que la celda: {char!r} (x hasta {max(xs)})")
    rows = []
    for y in range(CELL_H):
        row = 0
        for x in range(CELL_W):
            if img.getpixel((x, y)) >= 64:
                row |= 1 << ((CELL_W - 1) - x)
        rows.append(row)
    return rows


def pack(rows: list[int]) -> bytes:
    """Filas de CELL_W bits → ROW_BYTES bytes por fila (big-endian)."""
    out = bytearray()
    for row in rows:
        for k in range(ROW_BYTES):
            out.append((row >> (8 * (ROW_BYTES - 1 - k))) & 0xFF)
    return bytes(out)


def fmt(data: bytes) -> str:
    return "[" + ", ".join(f"0x{b:02X}" for b in data) + "]"


def main() -> int:
    glifos = [pack(raster(ch)) for ch in ASCII]
    extras = [(ch, pack(raster(ch))) for ch in EXTRA]

    lines = [
        "//! DATOS GENERADOS — no editar a mano.",
        f"//! Fuente: gen_font.py (DejaVuSans {SIZE} px → bitmap {CELL_W}×{CELL_H},",
        f"//! {ROW_BYTES} byte(s) por fila, bit más alto = píxel izquierdo).",
        "//! Regenerar: python3 crates/L3-devapps/devapp-demo/tools/gen_font.py",
        "",
        "/// Ancho de celda en píxeles (= avance monoespaciado).",
        f"pub const FONT_CELL_W: usize = {CELL_W};",
        "/// Alto de celda en píxeles.",
        f"pub const FONT_CELL_H: usize = {CELL_H};",
        "/// Bytes por fila de glifo (filas de CELL_W px, big-endian).",
        f"pub const FONT_ROW_BYTES: usize = {ROW_BYTES};",
        "",
        "/// Glifos ASCII 32..=126 (índice = código ASCII − 32).",
        f"pub const ASCII_FONT: [[u8; {ROW_BYTES * CELL_H}]; 95] = [",
    ]
    lines += ["    " + fmt(g) + "," for g in glifos]
    lines += ["];", ""]
    lines += [
        "/// Glifos extra (castellano del demo), emparejados con su carácter.",
        f"pub const EXTRA_FONT: &[(char, [u8; {ROW_BYTES * CELL_H}])] = &[",
    ]
    lines += [f"    ('{ch}', {fmt(g)})," for ch, g in extras]
    lines += ["];", ""]
    OUT.write_text("\n".join(lines), encoding="utf-8")
    print(f"OK: {OUT} ({len(glifos)} ASCII + {len(extras)} extra, "
          f"celda {CELL_W}x{CELL_H}, {ROW_BYTES} B/fila)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
