#!/usr/bin/env python3
"""Genera assets/icono.png del devapp-demo (192×192, PNG puro).

Sin PIL: escribe el PNG a mano (zlib + struct — igual que gen_logo.py),
así el build no gana dependencias nuevas.

Diseño: cuadrado redondeado con degradado vertical teal (claro arriba,
oscuro abajo) + "A" blanca en pixel-art 5×7 (coherente con la estética
bitmap del SDK de F3a; en F5 habrá iconos vectoriales de verdad).

El PNG termina dentro del binario vía scripts/empaqueta_app.py
(footer ARCAAPP1) y el lanzador lo decodifica para el grid.
"""

from __future__ import annotations

import struct
import zlib

LADO = 192
RADIO = 42

# degradado: teal claro → teal oscuro (paleta del proyecto)
C_ARIBA = (20, 184, 166)   # #14B8A6
C_ABAJO = (11, 90, 85)     # #0B5A55
C_BLANCO = (235, 238, 245)

# 'A' en pixel-art 5 columnas × 7 filas
GLIFO = [
    "..#..",
    ".#.#.",
    "#...#",
    "#...#",
    "#####",
    "#...#",
    "#...#",
]
CELDA = 22   # px por pixel del glifo → 110×154, centrado en 192

SALIDA = "assets/icono.png"


def dentro_redondeado(x: int, y: int) -> bool:
    dx = min(x, LADO - 1 - x)
    dy = min(y, LADO - 1 - y)
    if dx >= RADIO or dy >= RADIO:
        return True
    return (dx * dx + dy * dy) <= RADIO * RADIO


def glifo_en(x: int, y: int) -> bool:
    gw = 5 * CELDA
    gh = 7 * CELDA
    ox = (LADO - gw) // 2
    oy = (LADO - gh) // 2
    if not (ox <= x < ox + gw and oy <= y < oy + gh):
        return False
    col = (x - ox) // CELDA
    fila = (y - oy) // CELDA
    return GLIFO[fila][col] == "#"


def main() -> int:
    filas: list[bytes] = []
    for y in range(LADO):
        fila = bytearray(b"\x00")  # filtro PNG: None
        for x in range(LADO):
            if not dentro_redondeado(x, y):
                fila += b"\x00\x00\x00\x00"
                continue
            t = y / (LADO - 1)
            r = round(C_ARIBA[0] + (C_ABAJO[0] - C_ARIBA[0]) * t)
            g = round(C_ARIBA[1] + (C_ABAJO[1] - C_ARIBA[1]) * t)
            b = round(C_ARIBA[2] + (C_ABAJO[2] - C_ARIBA[2]) * t)
            if glifo_en(x, y):
                fila += bytes((C_BLANCO[0], C_BLANCO[1], C_BLANCO[2], 255))
            else:
                fila += bytes((r, g, b, 255))
        filas.append(bytes(fila))

    raw = b"".join(filas)

    def chunk(tipo: bytes, datos: bytes) -> bytes:
        return (
            struct.pack(">I", len(datos))
            + tipo
            + datos
            + struct.pack(">I", zlib.crc32(tipo + datos) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", LADO, LADO, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")

    with open(SALIDA, "wb") as f:
        f.write(png)
    print(f"[OK] {SALIDA}: {LADO}x{LADO}, {len(png)} B")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
