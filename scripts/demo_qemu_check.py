#!/usr/bin/env python3
"""Harness del demo F3a en modo teléfono, SIN teléfono.

Ejecuta el binario REAL (aarch64 musl) bajo qemu-user y valida el contrato
completo de la sub-app: hello, frames con rotación de slots, stats con fps
sano (regresión r9: el underflow daba fps≈4.6e15), apagado limpio por
stdin y, desde r10, el botón de cierre X (hit-test + píxeles dibujados) y
la geometría alta ~664×1440 con la UI escalada.

Uso:
    python3 scripts/demo_qemu_check.py [binario] [--qemu RUTA]

Por defecto: binario = target/aarch64-unknown-linux-musl/release/devapp-demo
             qemu   = qemu-aarch64-static (del PATH)
"""

from __future__ import annotations

import json
import os
import struct
import subprocess
import sys
import tempfile
import threading
import time

SLOT_HDR = 16   # arca-shm: seq u64 + pad 8
SLOTS = 2       # double-buffer
HDR = 32        # arca-gfx-protocol
FRAME_MS = 33

DEFAULT_BIN = os.path.join(
    "target", "aarch64-unknown-linux-musl", "release", "devapp-demo"
)


def region_len(w: int, h: int) -> int:
    frame_bytes = HDR + w * h * 4
    return SLOTS * (SLOT_HDR + frame_bytes)


def zona_x(w: int, h: int) -> tuple[int, int, int]:
    """Espejo de ui_scale/zona_x del demo (diseño 720p, lado 40 en (w-52,6))."""
    ui = max(1, min(3, round(h / 720)))
    side = 40 * ui
    return w - 52 * ui, 6 * ui, side


class Demo:
    """Una corrida del demo bajo qemu con su fb propio y stdin/stdout."""

    def __init__(self, qemu: str, binario: str, w: int, h: int):
        self.w, self.h = w, h
        self.fb = tempfile.NamedTemporaryFile(
            prefix="arca-fb-check-", suffix=".bin", delete=False
        )
        self.fb.write(b"\0" * region_len(w, h))
        self.fb.close()
        self.lines: list[str] = []
        self.proc = subprocess.Popen(
            [qemu, binario],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            env={
                "ARCA_FB": self.fb.name,
                "ARCA_FB_W": str(w),
                "ARCA_FB_H": str(h),
                "HOME": os.environ.get("HOME", "/root"),
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "TMPDIR": tempfile.gettempdir(),
            },
        )
        self.reader = threading.Thread(target=self._pump, daemon=True)
        self.reader.start()

    def _pump(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.lines.append(line.rstrip("\n"))

    def send(self, obj: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(obj, separators=(",", ":")) + "\n")
        self.proc.stdin.flush()

    def wait(self, timeout: float) -> int:
        rc = self.proc.wait(timeout=timeout)
        self.reader.join(timeout=5)
        return rc

    def events(self, name: str) -> list[dict]:
        out = []
        for line in self.lines:
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if obj.get("event") == name:
                out.append(obj)
        return out

    def fb_slot(self, slot: int) -> tuple[int, bytes]:
        with open(self.fb.name, "rb") as f:
            f.seek(slot * (SLOT_HDR + HDR + self.w * self.h * 4))
            seq = struct.unpack("<Q", f.read(8))[0]
            f.seek(slot * (SLOT_HDR + HDR + self.w * self.h * 4) + SLOT_HDR)
            payload_hdr = f.read(HDR)
        return seq, payload_hdr

    def fb_pixel(self, x: int, y: int) -> tuple[int, int, int, int]:
        """RGBA del píxel del ÚLTIMO frame válido (seq impar) del slot dado."""
        for slot in range(SLOTS):
            base = slot * (SLOT_HDR + HDR + self.w * self.h * 4)
            with open(self.fb.name, "rb") as f:
                f.seek(base)
                seq = struct.unpack("<Q", f.read(8))[0]
                if seq % 2 != 1:
                    continue
                f.seek(base + SLOT_HDR + HDR + (y * self.w + x) * 4)
                r, g, b, a = struct.unpack("<BBBB", f.read(4))
                return r, g, b, a
        raise AssertionError("ningún slot con seq impar (¿publicó algo?)")

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        os.unlink(self.fb.name)


class Check:
    def __init__(self) -> None:
        self.ok = 0
        self.fail = 0

    def that(self, cond: bool, label: str) -> None:
        if cond:
            self.ok += 1
            print(f"  [OK] {label}")
        else:
            self.fail += 1
            print(f"  [FALLO] {label}")


def run_demo_for(
    check: Check, qemu: str, bin: str, w: int, h: int, secs: float, tag: str
) -> Demo:
    """Corre `secs` segundos y apaga con shutdown (contrato del host)."""
    print(f"— corrida {tag}: fb {w}x{h}, {secs:.0f} s, apagado por stdin —")
    d = Demo(qemu, bin, w, h)
    deadline = time.time() + secs
    while time.time() < deadline and d.proc.poll() is None:
        time.sleep(0.2)
    d.send({"event": "shutdown"})
    rc = d.wait(timeout=15)
    check.that(rc == 0, f"{tag}: exit 0 tras shutdown (rc={rc})")
    check.that(len(d.events("hello")) == 1, f"{tag}: una línea hello")
    hello = d.events("hello")[0] if d.events("hello") else {}
    check.that(
        hello.get("w") == w and hello.get("h") == h,
        f"{tag}: hello reporta la geometría ({hello.get('w')}x{hello.get('h')})",
    )
    frames = d.events("frame")
    check.that(len(frames) > 30, f"{tag}: frames publicados ({len(frames)})")
    slots = [f.get("slot") for f in frames]
    check.that(
        all(s in (0, 1) for s in slots), f"{tag}: slots siempre 0/1"
    )
    check.that(
        0 in slots and 1 in slots, f"{tag}: el double-buffer ROTA (0 y 1 vistos)"
    )
    alt = all(
        slots[i] != slots[i + 1] for i in range(min(40, len(slots) - 1))
    )
    check.that(alt, f"{tag}: slots alternan frame a frame")
    exiting = d.events("exiting")
    check.that(
        len(exiting) == 1 and exiting[0].get("reason") == "shutdown",
        f"{tag}: exiting reason=shutdown",
    )
    check.that(not d.events("fatal"), f"{tag}: sin fatal")
    # fb: AFRM + geometría en el último frame de cada slot publicado
    for slot in (0, 1):
        if slot in slots:
            seq, hdr = d.fb_slot(slot)
            magic = hdr[:4]
            fw, fh = struct.unpack("<HH", hdr[8:12])
            check.that(
                magic == b"AFRM" and fw == w and fh == h and seq % 2 == 1,
                f"{tag}: fb slot {slot} AFRM {fw}x{fh} seq impar ({seq})",
            )
    return d


def check_stats(check: Check, d: Demo, tag: str) -> None:
    stats = d.events("stats")
    print(f"— stats {tag}: {len(stats)} líneas —")
    if not stats:
        check.that(False, f"{tag}: stats presentes")
        return
    check.that(True, f"{tag}: stats presentes ({len(stats)} líneas)")
    fps = [s.get("fps", -1) for s in stats]
    check.that(
        all(0 < f <= 500 for f in fps),
        f"{tag}: fps sane (regresión r9: el underflow daba ~4.6e15): {fps[:6]}",
    )


def run_x_button(check: Check, qemu: str, bin: str, w: int, h: int, tag: str) -> None:
    """El botón X: un toque fuera NO mata; el toque en la X sí (exit 0)."""
    print(f"— corrida {tag}: botón X en {w}x{h} —")
    x, y, side = zona_x(w, h)
    cx, cy = x + side // 2, y + side // 2
    d = Demo(qemu, bin, w, h)
    # 1) maduración: que publique frames
    deadline = time.time() + 3.0
    while time.time() < deadline and d.proc.poll() is None and len(d.events("frame")) < 10:
        time.sleep(0.1)
    # 2) toque en el CENTRO (fuera de la X y de los botones): no debe morir
    d.send({"event": "touch", "phase": "down", "x": w // 2, "y": h // 3, "t": 1})
    d.send({"event": "touch", "phase": "up", "x": w // 2, "y": h // 3, "t": 2})
    time.sleep(1.0)
    check.that(
        d.proc.poll() is None,
        f"{tag}: un toque fuera de la X NO mata la sub-app",
    )
    # 3) la X se DIBUJA: el centro de la zona es un trazo blanco opaco
    r, g, b, a = d.fb_pixel(cx, cy)
    check.that(
        a == 255 and r > 180 and g > 180 and b > 180,
        f"{tag}: píxel del centro de la X es blanco opaco ({r},{g},{b},{a})",
    )
    # 4) toque en la X → exiting reason=x + exit 0
    d.send({"event": "touch", "phase": "down", "x": cx, "y": cy, "t": 3})
    try:
        rc = d.wait(timeout=10)
    except subprocess.TimeoutExpired:
        rc = -1
        d.proc.kill()
    check.that(rc == 0, f"{tag}: exit 0 tras tocar la X (rc={rc})")
    exiting = d.events("exiting")
    check.that(
        len(exiting) == 1 and exiting[0].get("reason") == "x",
        f"{tag}: exiting reason=x ({exiting})",
    )
    check.that(not d.events("sigterm"), f"{tag}: sin sigterm (cierre limpio interno)")
    d.close()


def main() -> int:
    args = sys.argv[1:]
    bin = DEFAULT_BIN
    qemu = "qemu-aarch64-static"
    i = 0
    while i < len(args):
        if args[i] == "--qemu" and i + 1 < len(args):
            qemu = args[i + 1]
            i += 2
        else:
            bin = args[i]
            i += 1

    if not os.path.isfile(bin):
        print(f"no existe {bin} — corre: cargo build -p devapp-demo "
              "--target aarch64-unknown-linux-musl --release")
        return 2
    if subprocess.run([qemu, "--version"], capture_output=True).returncode != 0:
        print(f"no puedo ejecutar {qemu} — pásalo con --qemu RUTA")
        return 2

    print(f"binario: {bin}\nqemu:    {qemu}")
    check = Check()

    # A) geometría chica de qemu (ui=1) con stats completos
    d = run_demo_for(check, qemu, bin, 160, 360, 16, "A(160x360)")
    check_stats(check, d, "A")
    d.close()

    # B) geometría r9 del teléfono (ui=1)
    d = run_demo_for(check, qemu, bin, 336, 720, 10, "B(336x720)")
    check_stats(check, d, "B")
    d.close()

    # C) geometría r10 del teléfono (ui=2: X más grande, UI escalada)
    d = run_demo_for(check, qemu, bin, 664, 1440, 12, "C(664x1440)")
    if len(d.events("frame")) >= 120:
        check_stats(check, d, "C")
    else:
        print("  [info] C: pocos frames bajo qemu para stats; se salta")
    d.close()

    # D) el botón X (ui=1)  E) el botón X (ui=2, la geometría real r10)
    run_x_button(check, qemu, bin, 336, 720, "D(336x720)")
    run_x_button(check, qemu, bin, 664, 1440, "E(664x1440)")

    total = check.ok + check.fail
    print(f"\n{check.ok}/{total} comprobaciones OK")
    if check.fail:
        print("HARNESS FALLÓ")
        return 1
    print("HARNESS OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
