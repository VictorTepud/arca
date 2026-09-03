#!/usr/bin/env python3
"""Harness de devapp-calc en modo teléfono, SIN teléfono.

Hermano de demo_qemu_check.py: ejecuta el binario REAL (aarch64 musl, con
footer ARCAAPP1 — el loader ignora los bytes extra) bajo qemu-user y
valida el contrato completo de la sub-app CALCULADORA:

  · protocolo: hello, frames con rotación de slots, pong, stats sane
    (regresión r9), exiting con reason=x (la X de la esquina) y exit 0.
  · fb: AFRM + geometría en el último frame de cada slot.
  · CALCULADORA: el estado del display es una función determinista del
    estado lógico (nada animado dentro del panel) → se hashea el panel:
    "1/0=" cambia el panel (Error), "C" lo restaura EXACTO al estado
    inicial, "7*6=" produce otro panel distinto (42), "C" vuelve a
    restaurar. Es una prueba de integración de input→estado→render sin
    leer píxeles de texto.

Uso:
    python3 scripts/calc_qemu_check.py [binario] [--qemu RUTA]

Por defecto: binario = target/aarch64-unknown-linux-musl/release/devapp-calc
             qemu   = qemu-aarch64-static (del PATH)
"""

from __future__ import annotations

import hashlib
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

DEFAULT_BIN = os.path.join(
    "target", "aarch64-unknown-linux-musl", "release", "devapp-calc"
)

# espejo del layout de devapp-calc (canvas de diseño 336×720, r13)
PANEL_X, PANEL_Y, PANEL_H = 12, 68, 132  # ·ui
GRID_M, GRID_GAP, GRID_TOP = 8, 8, 208   # ·ui
BARRA_BAJA = 52                            # ·ui
BOTONES = ["C", "%", "<", "/", "7", "8", "9", "*", "4", "5", "6", "-",
           "1", "2", "3", "+", "+/-", "0", ".", "="]


def region_len(w: int, h: int) -> int:
    frame_bytes = HDR + w * h * 4
    return SLOTS * (SLOT_HDR + frame_bytes)


def ui_mirror(w: int, h: int) -> int:
    """Espejo de ui_scale del calc r14 = ui_scale del demo r13:
    min(w/336, h/720) contra el canvas de diseño, half-up, clamp 1..4."""
    return max(1, min(4, int(min(w / 336, h / 720) + 0.5)))


def grid_mirror(w: int, h: int) -> tuple[int, int, int, int, int]:
    """Espejo de Calc::grid(): (x0, y0, bw, bh, gap)."""
    ui = ui_mirror(w, h)
    m, gap = 8 * ui, 8 * ui
    bw = max((w - 2 * m - 3 * gap) // 4, 24)
    top = 208 * ui
    libre = h - 52 * ui - 8 * ui - 8 * ui - 4 * gap - top
    bh = max(libre // 5, 16)
    return m, top, bw, bh, gap


def centro_boton(w: int, h: int, idx: int) -> tuple[int, int]:
    x0, y0, bw, bh, gap = grid_mirror(w, h)
    x = x0 + (idx % 4) * (bw + gap) + bw // 2
    y = y0 + (idx // 4) * (bh + gap) + bh // 2
    return x, y


def idx_de(etiqueta: str) -> int:
    return BOTONES.index(etiqueta)


def zona_x(w: int, h: int) -> tuple[int, int, int]:
    ui = ui_mirror(w, h)
    side = 40 * ui
    return w - 52 * ui, 6 * ui, side


class Calc:
    """Una corrida de devapp-calc bajo qemu con su fb propio y stdio."""

    def __init__(self, qemu: str, binario: str, w: int, h: int):
        self.w, self.h = w, h
        self.fb = tempfile.NamedTemporaryFile(
            prefix="arca-calc-check-", suffix=".bin", delete=False
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

    # ── framebuffer ────────────────────────────────────────────────

    def _latest_slot(self) -> int:
        """Slot válido (seq impar) con el seq más alto."""
        mejor, seq_max = -1, -1
        for slot in range(SLOTS):
            base = slot * (SLOT_HDR + HDR + self.w * self.h * 4)
            with open(self.fb.name, "rb") as f:
                f.seek(base)
                seq = struct.unpack("<Q", f.read(8))[0]
            if seq % 2 == 1 and seq > seq_max:
                mejor, seq_max = slot, seq
        if mejor < 0:
            raise AssertionError("ningún slot con seq impar (¿publicó algo?)")
        return mejor

    def fb_slot(self, slot: int) -> tuple[int, bytes]:
        base = slot * (SLOT_HDR + HDR + self.w * self.h * 4)
        with open(self.fb.name, "rb") as f:
            f.seek(base)
            seq = struct.unpack("<Q", f.read(8))[0]
            f.seek(base + SLOT_HDR)
            payload_hdr = f.read(HDR)
        return seq, payload_hdr

    def panel_hash(self) -> str:
        """SHA-256 del rect del display del último frame válido. El panel
        solo contiene funciones deterministas del estado (eco, entrada,
        historial): mismo estado ⇒ mismo hash."""
        ui = ui_mirror(self.w, self.h)
        px, py = PANEL_X * ui, PANEL_Y * ui
        pw, ph = self.w - 24 * ui, PANEL_H * ui
        slot = self._latest_slot()
        base = slot * (SLOT_HDR + HDR + self.w * self.h * 4)
        hsh = hashlib.sha256()
        with open(self.fb.name, "rb") as f:
            for y in range(py, py + ph):
                f.seek(base + SLOT_HDR + HDR + (y * self.w + px) * 4)
                hsh.update(f.read(pw * 4))
        return hsh.hexdigest()

    def cerrar(self) -> None:
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


def tocar(c: Calc, x: int, y: int) -> None:
    c.send({"event": "touch", "phase": "down", "x": x, "y": y, "t": 0})
    c.send({"event": "touch", "phase": "up", "x": x, "y": y, "t": 0})


def teclas(c: Calc, w: int, h: int, etiquetas: str) -> None:
    """Toca los botones cuyas etiquetas forman `etiquetas` (p.ej. "7*6=")."""
    for ch in etiquetas:
        tocar(c, *centro_boton(w, h, idx_de(ch)))


def main() -> int:
    bin_path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_BIN
    qemu = "qemu-aarch64-static"
    if "--qemu" in sys.argv:
        qemu = sys.argv[sys.argv.index("--qemu") + 1]
    if not os.path.exists(bin_path):
        print(f"[ERROR] no existe {bin_path}", file=sys.stderr)
        return 1

    check = Check()
    w, h = 720, 1536  # el Huawei real del usuario (r13: fb 1:1)
    print(f"— corrida calc {w}x{h} bajo {qemu} —")
    c = Calc(qemu, bin_path, w, h)
    try:
        # 1) protocolo base: hello + frames rotando
        time.sleep(1.5)
        check.that(len(c.events("hello")) == 1, "una línea hello")
        hello = c.events("hello")[0] if c.events("hello") else {}
        check.that(
            hello.get("w") == w and hello.get("h") == h,
            f"hello reporta la geometría ({hello.get('w')}x{hello.get('h')})",
        )

        # 2) ping → pong (heartbeat del host)
        for _ in range(3):
            c.send({"event": "ping"})
        time.sleep(0.3)
        check.that(len(c.events("pong")) >= 3, "ping → pong (x3)")

        # 3) estado determinista del display: Error y resultado
        h0 = c.panel_hash()
        teclas(c, w, h, "1/0=")
        time.sleep(0.4)
        he = c.panel_hash()
        check.that(he != h0, "1/0= cambia el display (Error)")
        teclas(c, w, h, "C")
        time.sleep(0.4)
        check.that(c.panel_hash() == h0, "C restaura el display EXACTO")

        teclas(c, w, h, "7*6=")
        time.sleep(0.4)
        h42 = c.panel_hash()
        check.that(h42 != h0 and h42 != he, "7*6= produce otro display (42)")
        teclas(c, w, h, "C")
        time.sleep(0.4)
        check.that(c.panel_hash() == h0, "C restaura de nuevo el display")

        # 4) espera a stats (120 frames ≈ 4 s; qemu puede tardar más)
        stats_deadline = time.time() + 8
        while time.time() < stats_deadline and not c.events("stats"):
            time.sleep(0.2)
        stats = c.events("stats")
        if stats:
            fps = [s.get("fps", -1) for s in stats]
            check.that(
                all(0 < f <= 500 for f in fps),
                f"fps sane (regresión r9): {fps[:4]}",
            )
        else:
            check.that(False, "stats presentes (120 frames)")

        # 5) cierre por la X de la esquina (r10)
        zx, zy, side = zona_x(w, h)
        tocar(c, zx + side // 2, zy + side // 2)
        rc = c.wait(timeout=15)
        check.that(rc == 0, f"exit 0 tras la X (rc={rc})")
        exiting = c.events("exiting")
        check.that(
            len(exiting) == 1 and exiting[0].get("reason") == "x",
            f"exiting reason=x ({[e.get('reason') for e in exiting]})",
        )
        check.that(not c.events("fatal"), "sin fatal")
        frames = c.events("frame")
        check.that(len(frames) > 60, f"frames publicados ({len(frames)})")
        slots = [f.get("slot") for f in frames]
        check.that(
            all(s in (0, 1) for s in slots) and 0 in slots and 1 in slots,
            "slots 0/1 y el double-buffer rota",
        )
        alt = all(
            slots[i] != slots[i + 1] for i in range(min(40, len(slots) - 1))
        )
        check.that(alt, "slots alternan frame a frame")
        for slot in (0, 1):
            if slot in slots:
                seq, hdr = c.fb_slot(slot)
                fw, fh = struct.unpack("<HH", hdr[8:12])
                check.that(
                    hdr[:4] == b"AFRM" and fw == w and fh == h and seq % 2 == 1,
                    f"fb slot {slot} AFRM {fw}x{fh} seq impar ({seq})",
                )

        # geometría del grid espejada (sanity del mirror contra el layout)
        x0, y0, bw, bh, gap = grid_mirror(w, h)
        ultimo = y0 + 4 * (bh + gap) + bh
        check.that(
            x0 >= 0 and bw > 0 and bh > 0 and ultimo <= h - 52 * ui_mirror(w, h),
            f"grid espejo: {bw}x{bh}, sin pisar la barra ({ultimo} ≤ {h - 52 * ui_mirror(w, h)})",
        )
    finally:
        c.cerrar()

    total = check.ok + check.fail
    print(f"\ncalc_qemu_check: {check.ok}/{total} OK")
    return 1 if check.fail else 0


if __name__ == "__main__":
    raise SystemExit(main())
