#!/usr/bin/env python3
"""gen_logo.py — logo RGBA embebido de devapp-demo (96×96, "imagen" del demo).

Salida: crates/L3-devapps/devapp-demo/assets/logo.rgba (8 B de cabecera +
R,G,B,A crudos top-down). Dibujo procedural (sin assets de terceros):
cuadrado redondeado con degradado azul→teal, letra "A" blanca, borde
sutil y destellos alfa (ejercitan el blit con transparencia).
Regenerar: python3 crates/L3-devapps/devapp-demo/tools/gen_logo.py
"""
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets/logo.rgba"
S = 96


def lerp(a, b, t):
    return int(a + (b - a) * t)


def main() -> int:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Cuadrado redondeado con degradado vertical (dibujado por filas).
    top = (26, 98, 208)    # azul
    bot = (16, 173, 158)   # teal
    r = 20
    for y in range(4, S - 4):
        t = y / (S - 1)
        col = (lerp(top[0], bot[0], t), lerp(top[1], bot[1], t), lerp(top[2], bot[2], t), 255)
        x0, x1 = 4, S - 5
        for x in range(x0, x1 + 1):
            dx = min(x - (x0 + r), (x1 - r) - x)
            dy = min(y - (4 + r), (S - 5 - r) - y)
            if dx < 0 and dy < 0 and (dx * dx + dy * dy) > r * r:
                continue
            img.putpixel((x, y), col)

    # Letra "A" blanca (polígono grueso).
    ax, ay = S // 2, S // 2
    d.polygon(
        [(ax - 16, ay + 20), (ax - 6, ay - 22), (ax + 6, ay - 22),
         (ax + 16, ay + 20), (ax + 8, ay + 20), (ax + 4, ay + 6),
         (ax - 4, ay + 6), (ax - 8, ay + 20)],
        fill=(255, 255, 255, 255),
    )
    d.polygon(
        [(ax - 5, ay - 6), (ax + 5, ay - 6), (ax + 3, ay + 1), (ax - 3, ay + 1)],
        fill=(255, 255, 255, 255),
    )

    # Destellos (círculos alfa) — ejercicio del blit con alfa.
    for (cx, cy, rad, a) in [(26, 26, 6, 160), (70, 62, 4, 120), (38, 68, 3, 90)]:
        for x in range(cx - rad, cx + rad + 1):
            for y in range(cy - rad, cy + rad + 1):
                dist2 = (x - cx) ** 2 + (y - cy) ** 2
                if dist2 <= rad * rad:
                    falloff = 1 - dist2 / (rad * rad)
                    alpha = int(a * falloff)
                    pr, pg, pb, _ = img.getpixel((x, y))
                    img.putpixel((x, y), (pr, pg, pb, max(alpha, alpha)))

    # Borde sutil.
    d.rounded_rectangle([4, 4, S - 5, S - 5], radius=r, outline=(255, 255, 255, 70), width=2)

    with open(OUT, "wb") as f:
        f.write(struct.pack("<I", S))            # lado
        f.write(struct.pack("<I", S))            # lado (repetido: validación)
        f.write(img.tobytes())                   # RGBA top-down
    print(f"OK: {OUT} ({OUT.stat().st_size} B = 8 + 96*96*4)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
