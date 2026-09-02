#!/usr/bin/env python3
"""Empaqueta nombre + icono al final de un binario devapp (footer ARCAAPP1).

PROBLEMA QUE RESUELVE
---------------------
El lanzador (MainActivity) lista los binarios instalados en
``filesDir/exec``. Un ELF pelado solo tiene el nombre de archivo; para que
la app "venga con icono desde la compilación" (pedido r11) hace falta
metadatos que viajen DENTRO del binario.

SOLUCIÓN: un footer al final del archivo. El loader de ELF de Linux lee
los *program headers* y mapea los segmentos; los bytes que quedan tras el
último segmento los IGNORA — el binario sigue siendo ejecutable tal cual
(mismo truco que los autoextraíbles). qemu-user idéntico.

LAYOUT (desde el final del archivo hacia atrás)::

    [ nombre UTF-8 ][ u16 nombre_len ][ icono PNG ][ u32 icono_len ][ b"ARCAAPP1" ]

    · magic: 8 bytes "ARCAAPP1" al final absoluto.
    · icono_len: u32 LE justo antes del magic.
    · nombre_len: u16 LE justo antes del icono.
    · icono: PNG válido (o longitud 0 = sin icono → avatar con inicial).
    · nombre: UTF-8 (≤ 96 bytes).

El mismo layout lo parsea MainActivity.leerFooter() (espejo en Kotlin) y
scripts/demo_qemu_check.py NO lo toca (ejecuta el binario tal cual).

USO
---
    python3 scripts/empaqueta_app.py BINARIO --name "Demo Arca" \
        [--icono ruta/a/icono.png]

ARCAS secundarios: valida cabecera ELF y PNG, límites de tamaño (icono
≤ 256 KiB, nombre ≤ 96 bytes), y es IDEMPOTENTE: si el binario ya tiene
footer, lo quita antes de escribir el nuevo (re-empaquetar tras un
rebuild no acumula basura).
"""

from __future__ import annotations

import argparse
import struct
import sys

MAGIC = b"ARCAAPP1"
MAX_ICONO = 256 * 1024
MAX_NOMBRE = 96


def es_elf(data: bytes) -> bool:
    return data[:4] == b"\x7fELF"


def parse_footer(data: bytes) -> tuple[str, bytes | None] | None:
    """Lee el footer del final de `data` (o None si no hay/válido)."""
    if len(data) < 14:
        return None
    if data[-8:] != MAGIC:
        return None
    icono_len = struct.unpack("<I", data[-12:-8])[0]
    if icono_len > MAX_ICONO:
        return None
    icono_start = len(data) - 12 - icono_len
    if icono_start < 2:
        return None
    nombre_len = struct.unpack("<H", data[icono_start - 2 : icono_start])[0]
    if not 0 < nombre_len <= MAX_NOMBRE:
        return None
    nombre_start = icono_start - 2 - nombre_len
    if nombre_start < 0:
        return None
    nombre = data[nombre_start : icono_start - 2].decode("utf-8", "replace").strip()
    if not nombre:
        return None
    icono = data[icono_start : len(data) - 12] if icono_len else None
    return nombre, icono


def sin_footer(data: bytes) -> bytes:
    """Quita un footer previo (idempotencia): devuelve el ELF puro."""
    meta = parse_footer(data)
    if meta is None:
        return data
    nombre, icono = meta
    total = 8 + 4 + (len(icono) if icono else 0) + 2 + len(nombre.encode())
    return data[:-total]


def main() -> int:
    ap = argparse.ArgumentParser(description="Empaqueta footer ARCAAPP1 en un binario devapp")
    ap.add_argument("binario", help="ruta del ELF a empaquetar (se modifica in situ)")
    ap.add_argument("--name", required=True, help="nombre visible de la app (UTF-8, ≤ 96 bytes)")
    ap.add_argument("--icono", default=None, help="PNG con el icono (≤ 256 KiB), opcional")
    args = ap.parse_args()

    try:
        with open(args.binario, "rb") as f:
            data = f.read()
    except OSError as e:
        print(f"[ERROR] no pude leer {args.binario}: {e}", file=sys.stderr)
        return 1

    if not es_elf(data):
        print(f"[ERROR] {args.binario} no es un ELF", file=sys.stderr)
        return 1

    nombre_b = args.name.strip().encode("utf-8")
    if not nombre_b or len(nombre_b) > MAX_NOMBRE:
        print(f"[ERROR] nombre inválido (vacío o > {MAX_NOMBRE} bytes UTF-8)", file=sys.stderr)
        return 1

    icono_b = b""
    if args.icono:
        try:
            with open(args.icono, "rb") as f:
                icono_b = f.read()
        except OSError as e:
            print(f"[ERROR] no pude leer el icono {args.icono}: {e}", file=sys.stderr)
            return 1
        if not icono_b.startswith(b"\x89PNG"):
            print(f"[ERROR] {args.icono} no es un PNG", file=sys.stderr)
            return 1
        if len(icono_b) > MAX_ICONO:
            print(f"[ERROR] icono de {len(icono_b)} B (> {MAX_ICONO}): "
                  "redúcelo (192×192 va de sobra)", file=sys.stderr)
            return 1

    base = sin_footer(data)
    footer = (
        nombre_b
        + struct.pack("<H", len(nombre_b))
        + icono_b
        + struct.pack("<I", len(icono_b))
        + MAGIC
    )

    with open(args.binario, "wb") as f:
        f.write(base + footer)

    # verificación: releer y parsear
    with open(args.binario, "rb") as f:
        check = parse_footer(f.read())
    if check is None or check[0] != args.name.strip():
        print("[ERROR] el footer quedó mal escrito (no re-parsea)", file=sys.stderr)
        return 1

    print(f"[OK] {args.binario}: footer ARCAAPP1 "
          f"(nombre={args.name.strip()!r}, icono={len(icono_b)} B, "
          f"total {len(base) + len(footer)} B)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
