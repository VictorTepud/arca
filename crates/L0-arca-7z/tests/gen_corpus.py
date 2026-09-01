#!/usr/bin/env python3
"""Generador del corpus de tests de arca-7z (T05).

Crea `tests/corpus/` con:
  - `src/`: archivos fuente DETERMINISTAS (el test Rust compara bytes).
  - `*.7z`: archivos 7z REALES generados con py7zr (implementación
    independiente del formato 7z; en este entorno no hay binario 7-Zip:
    `which 7z 7zz 7za p7zip` → vacío. Desvío documentado en worklog T05).
  - `manifest.txt`: mapa archivo→entradas→fuentes para el test Rust
    (formato simple para no meter serde_json en el crate).

Ejecutar:  python3 tests/gen_corpus.py   (py7zr instalado en /home/z/.venv)
"""
import hashlib
import os
import random
import shutil
import sys

import py7zr
from py7zr import (
    FILTER_ARM, FILTER_ARMTHUMB, FILTER_BROTLI, FILTER_BZIP2, FILTER_COPY,
    FILTER_CRYPTO_AES256_SHA256, FILTER_DEFLATE, FILTER_DELTA, FILTER_IA64,
    FILTER_LZMA, FILTER_LZMA2, FILTER_POWERPC, FILTER_PPMD, FILTER_SPARC,
    FILTER_X86, FILTER_ZSTD,
)

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.join(HERE, "corpus")
SRC = os.path.join(CORPUS, "src")

BIG_BYTES = 500 * 1024 * 1024  # 500 MiB para el test de memoria


# ---------------------------------------------------------------- fuentes
def rng(seed: int) -> random.Random:
    return random.Random(seed)


def gen_text(path: str, size: int = 100_000) -> None:
    r = rng(1)
    words = ["arco", "isla", "peso", "luna", "vela", "roca", "seda", "reno",
             "tren", "lago", "ave", "mar", "sol", "flor", "cruz", "pan"]
    out = []
    n = 0
    while n < size:
        line = " ".join(r.choice(words) for _ in range(r.randint(4, 9)))
        out.append(f"{n:08d} {line}\n")
        n += len(line)
    with open(path, "w", encoding="utf-8") as f:
        f.write("".join(out)[:size])


def gen_elfish(path: str, size: int = 200_000) -> None:
    """Binario con patrón de código x86 (call/ret/mov) para BCJ X86."""
    r = rng(2)
    buf = bytearray()
    while len(buf) < size:
        op = r.random()
        if op < 0.35:  # call rel32
            buf += b"\xe8" + r.getrandbits(32).to_bytes(4, "little")
            buf += b"\x55\x48\x89\xe5"  # push rbp; mov rbp,rsp
        elif op < 0.6:  # jmp rel32
            buf += b"\xe9" + r.getrandbits(32).to_bytes(4, "little")
        else:
            buf += bytes(r.getrandbits(8) for _ in range(r.randint(4, 16)))
    with open(path, "wb") as f:
        f.write(bytes(buf[:size]))


def gen_armish(path: str, size: int = 200_000) -> None:
    """Binario con patrón ARM/Thumb (condiciones 0xE...) para BCJ ARM."""
    r = rng(3)
    buf = bytearray()
    while len(buf) < size:
        op = r.random()
        if op < 0.5:
            buf += (0xE5900000 | r.getrandbits(24)).to_bytes(4, "little")
        elif op < 0.75:
            buf += (0xEB000000 | r.getrandbits(24)).to_bytes(4, "little")
        else:
            buf += bytes(r.getrandbits(8) for _ in range(r.randint(2, 8)))
    with open(path, "wb") as f:
        f.write(bytes(buf[:size]))


def gen_data(path: str, size: int = 150_000) -> None:
    r = rng(4)
    with open(path, "wb") as f:
        chunk = bytes(r.getrandbits(8) for _ in range(4096))
        while size > 0:
            n = min(4096, size)
            f.write(chunk[:n])
            size -= n


def gen_pkg(root: str) -> None:
    """Layout de docs/06 §2 (paquete .arca simulado)."""
    os.makedirs(os.path.join(root, "bin/native-aarch64"), exist_ok=True)
    os.makedirs(os.path.join(root, "bin/wasm"), exist_ok=True)
    os.makedirs(os.path.join(root, "assets/fonts"), exist_ok=True)
    os.makedirs(os.path.join(root, "icons"), exist_ok=True)
    os.makedirs(os.path.join(root, "meta"), exist_ok=True)
    with open(os.path.join(root, "manifest.toml"), "w") as f:
        f.write('[package]\nid = "dev.misapps.teclado"\nname = "Mi Teclado Pro"\n'
                'version = "1.2.0"\nmin_host = "1.0.0"\napi_level = 1\n')
    with open(os.path.join(root, "bin/native-aarch64/app"), "wb") as f:
        # pseudo-ELF aarch64
        f.write(b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 8)
        r = rng(10)
        f.write(bytes(r.getrandbits(8) for _ in range(64_000)))
    with open(os.path.join(root, "bin/wasm/app.wasm"), "wb") as f:
        f.write(b"\x00asm\x01\x00\x00\x00")  # magic + version
        r = rng(11)
        f.write(bytes(r.getrandbits(8) for _ in range(32_000)))
    with open(os.path.join(root, "assets/fonts/inter.ttf"), "wb") as f:
        r = rng(12)
        f.write(bytes(r.getrandbits(8) for _ in range(50_000)))
    with open(os.path.join(root, "icons/icon-192.png"), "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + bytes(rng(13).getrandbits(8) for _ in range(12_000)))
    with open(os.path.join(root, "icons/icon-512.png"), "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + bytes(rng(14).getrandbits(8) for _ in range(30_000)))
    with open(os.path.join(root, "meta/graph.mmd"), "w") as f:
        f.write("flowchart LR\n  main --> ui\n  main --> ipc\n  ui --> fonts\n")
    with open(os.path.join(root, "meta/build.json"), "w") as f:
        f.write('{"egui": "0.31", "profile": "release"}\n')


def gen_many(root: str, count: int = 64) -> None:
    os.makedirs(root, exist_ok=True)
    r = rng(20)
    for i in range(count):
        with open(os.path.join(root, f"f{i:02d}.bin"), "wb") as f:
            f.write(bytes(r.getrandbits(8) for _ in range(r.randint(100, 2000))))


def gen_tree(root: str) -> None:
    """Árbol con unicode, dir vacío, archivo vacío y profundidad 16."""
    os.makedirs(os.path.join(root, "ñandú"), exist_ok=True)
    with open(os.path.join(root, "ñandú/水.txt"), "wb") as f:
        f.write("arco sobre el lago".encode("utf-8"))
    os.makedirs(os.path.join(root, "emptydir"), exist_ok=True)
    with open(os.path.join(root, "empty.bin"), "wb") as f:
        f.write(b"")
    deep = os.path.join(root, "/".join(f"d{i}" for i in range(14)))
    os.makedirs(deep, exist_ok=True)
    with open(os.path.join(deep, "fondo.txt"), "w") as f:
        f.write("profundo\n" * 10)
    with open(os.path.join(root, "small.txt"), "wb") as f:
        f.write(b"0123456789" * 10)


# ---------------------------------------------------------------- archivo
class Entry:
    def __init__(self, arcname: str, srcpath: str):
        self.arcname = arcname
        self.srcpath = srcpath
        self.size = os.path.getsize(srcpath)


def write_7z(path: str, entries, filters, solid_note: str = "") -> None:
    with py7zr.SevenZipFile(path, "w", filters=filters) as z:
        for e in entries:
            z.write(e.srcpath, arcname=e.arcname)
    print(f"  + {os.path.basename(path):32s} {filters} {solid_note}")


def main() -> int:
    if os.path.isdir(CORPUS):
        shutil.rmtree(CORPUS)
    os.makedirs(SRC)
    print("Fuentes deterministas...")
    gen_text(os.path.join(SRC, "text.txt"))
    gen_elfish(os.path.join(SRC, "elfish.bin"))
    gen_armish(os.path.join(SRC, "armish.bin"))
    gen_data(os.path.join(SRC, "data.bin"))
    gen_pkg(os.path.join(SRC, "pkg"))
    gen_many(os.path.join(SRC, "many"))
    gen_tree(os.path.join(SRC, "tree"))

    txt = Entry("text.txt", os.path.join(SRC, "text.txt"))
    elf = Entry("elfish.bin", os.path.join(SRC, "elfish.bin"))
    arm = Entry("armish.bin", os.path.join(SRC, "armish.bin"))
    dat = Entry("data.bin", os.path.join(SRC, "data.bin"))

    manifest = []

    def record(archive: str, mode: str, desc: str, entries, dirs=None) -> None:
        manifest.append((archive, mode, desc, entries, dirs or []))

    L = lambda p: FILTER_LZMA2 and {"id": FILTER_LZMA2, "preset": p}  # noqa: E731
    base = [txt, elf, dat]

    print("Corpus (py7zr %s):" % py7zr.__version__)
    write_7z(f"{CORPUS}/lzma2_p0.7z", base, [L(0)])
    record("lzma2_p0.7z", "ok", "LZMA2 preset 0", base)
    write_7z(f"{CORPUS}/lzma2_p3.7z", base, [L(3)])
    record("lzma2_p3.7z", "ok", "LZMA2 preset 3", base)
    write_7z(f"{CORPUS}/lzma2_p6.7z", base, [L(6)])
    record("lzma2_p6.7z", "ok", "LZMA2 preset 6", base)
    write_7z(f"{CORPUS}/lzma2_p9.7z", base, [L(9)])
    record("lzma2_p9.7z", "ok", "LZMA2 preset 9 (dict 64 MiB)", base)
    write_7z(f"{CORPUS}/lzma1_p6.7z", base, [{"id": FILTER_LZMA, "preset": 6}])
    record("lzma1_p6.7z", "ok", "LZMA1 preset 6", base)
    write_7z(f"{CORPUS}/copy.7z", base, [{"id": FILTER_COPY}])
    record("copy.7z", "ok", "COPY (store)", base)
    write_7z(f"{CORPUS}/bzip2.7z", base, [{"id": FILTER_BZIP2}])
    record("bzip2.7z", "ok", "BZIP2", base)

    try:
        write_7z(f"{CORPUS}/deflate.7z", base, [{"id": FILTER_DEFLATE}])
        record("deflate.7z", "ok", "DEFLATE (feature arca-7z/deflate)", base)
    except Exception as e:  # noqa: BLE001
        print("  ! DEFLATE no disponible:", e)

    try:
        write_7z(f"{CORPUS}/ppmd.7z", base, [{"id": FILTER_PPMD}])
        record("ppmd.7z", "ok", "PPMD", base)
    except Exception as e:  # noqa: BLE001
        print("  ! PPMD no disponible:", e)

    try:
        write_7z(f"{CORPUS}/zstd.7z", base, [{"id": FILTER_ZSTD, "level": 3}])
        record("zstd.7z", "ok", "ZSTD (feature arca-7z/zstd)", base)
    except Exception as e:  # noqa: BLE001
        print("  ! ZSTD no disponible:", e)

    try:
        write_7z(f"{CORPUS}/brotli.7z", base, [{"id": FILTER_BROTLI}])
        record("brotli.7z", "ok", "BROTLI (feature arca-7z/brotli)", base)
    except Exception as e:  # noqa: BLE001
        print("  ! BROTLI no disponible:", e)

    # filtros + LZMA2
    write_7z(f"{CORPUS}/delta_lzma2.7z", [dat], [{"id": FILTER_DELTA}, L(6)])
    record("delta_lzma2.7z", "ok", "DELTA+LZMA2", [dat])
    write_7z(f"{CORPUS}/bcj_x86_lzma2.7z", [elf, txt], [{"id": FILTER_X86}, L(6)])
    record("bcj_x86_lzma2.7z", "ok", "BCJ X86+LZMA2", [elf, txt])
    write_7z(f"{CORPUS}/bcj_arm_lzma2.7z", [arm], [{"id": FILTER_ARM}, L(6)])
    record("bcj_arm_lzma2.7z", "ok", "BCJ ARM+LZMA2", [arm])
    write_7z(f"{CORPUS}/bcj_armthumb_lzma2.7z", [arm], [{"id": FILTER_ARMTHUMB}, L(6)])
    record("bcj_armthumb_lzma2.7z", "ok", "BCJ ARMT+LZMA2", [arm])
    write_7z(f"{CORPUS}/bcj_ppc_lzma2.7z", [arm], [{"id": FILTER_POWERPC}, L(6)])
    record("bcj_ppc_lzma2.7z", "ok", "BCJ PPC+LZMA2", [arm])
    write_7z(f"{CORPUS}/bcj_sparc_lzma2.7z", [arm], [{"id": FILTER_SPARC}, L(6)])
    record("bcj_sparc_lzma2.7z", "ok", "BCJ SPARC+LZMA2", [arm])
    write_7z(f"{CORPUS}/bcj_ia64_lzma2.7z", [elf], [{"id": FILTER_IA64}, L(6)])
    record("bcj_ia64_lzma2.7z", "ok", "BCJ IA64+LZMA2", [elf])

    # árbol completo (unicode, vacíos, depth 16, entradas de DIRECTORIO
    # reales: py7zr writeall las escribe)
    tree_root = os.path.join(SRC, "tree")
    with py7zr.SevenZipFile(f"{CORPUS}/tree.7z", "w", filters=[L(6)]) as z:
        z.writeall(tree_root, arcname="tree")
    tree_entries, tree_dirs = [], []
    with py7zr.SevenZipFile(f"{CORPUS}/tree.7z", "r") as z:
        names = [f.filename for f in z.files]
    for n in names:
        if n in ("tree",):
            continue
        full = os.path.join(tree_root, n.removeprefix("tree/"))
        if os.path.isdir(full):
            tree_dirs.append(n)
        else:
            tree_entries.append(Entry(n, full))
    print(f"  + tree.7z ({len(tree_entries)} archivos, {len(tree_dirs)} dirs)")
    record("tree.7z", "ok", "unicode/vacíos/depth16 + dirs", tree_entries,
           dirs=tree_dirs)

    # layout de paquete docs/06 (manifest.toml PRIMERO en el stream)
    pkg_root = os.path.join(SRC, "pkg")
    pkg_files = []
    for dirpath, _dirs, files in os.walk(pkg_root):
        for fn in sorted(files):
            full = os.path.join(dirpath, fn)
            arc = os.path.relpath(full, pkg_root)
            pkg_files.append(Entry(arc, full))
    pkg_files.sort(key=lambda e: (e.arcname != "manifest.toml", e.arcname))
    write_7z(f"{CORPUS}/pkg_layout.7z", pkg_files, [L(6)])
    record("pkg_layout.7z", "ok", "layout docs/06 (manifest primero)", pkg_files)

    # 64 archivos
    many_entries = []
    for dirpath, _dirs, files in os.walk(os.path.join(SRC, "many")):
        for fn in sorted(files):
            full = os.path.join(dirpath, fn)
            arc = os.path.relpath(full, os.path.join(SRC, "many"))
            many_entries.append(Entry(arc, full))
    write_7z(f"{CORPUS}/many_files.7z", many_entries, [L(6)])
    record("many_files.7z", "ok", "64 archivos pequeños", many_entries)

    # archivo VACÍO (cero entradas)
    with py7zr.SevenZipFile(f"{CORPUS}/empty.7z", "w", filters=[L(6)]) as z:
        pass
    print("  + empty.7z (sin entradas)")
    record("empty.7z", "ok", "archivo sin entradas", [])

    # MALICIOSO: path traversal (py7zr no sanea arcnames)
    evil_src = os.path.join(SRC, "data.bin")
    mal_names = ["../evil.txt", "/abs.txt", "a/../../b.txt", "back\\slash.txt"]
    with py7zr.SevenZipFile(f"{CORPUS}/malicious.7z", "w", filters=[L(6)]) as z:
        for i, n in enumerate(mal_names):
            z.write(evil_src, arcname=n)
            # mezclamos entradas válidas entre medias
            if i == 1:
                z.write(os.path.join(SRC, "text.txt"), arcname="innocent.txt")
    print("  + malicious.7z (entradas maliciosas)")
    record("malicious.7z", "fail", "path traversal: ../, /abs, \\, a/../../b",
           [Entry("innocent.txt", os.path.join(SRC, "text.txt"))])

    # ENCRIPTADO (AES): v1 no soporta cifrado → open/extract debe fallar
    # (la capa crypto va ÚLTIMA en la cadena de filtros)
    try:
        with py7zr.SevenZipFile(
            f"{CORPUS}/encrypted.7z", "w", password="secreto",
            filters=[L(6), {"id": FILTER_CRYPTO_AES256_SHA256}],
        ) as z:
            z.write(os.path.join(SRC, "text.txt"), arcname="secreto.txt")
        print("  + encrypted.7z (AES256)")
        record("encrypted.7z", "fail", "AES256 (v1: sin cifrado)",
               [Entry("secreto.txt", os.path.join(SRC, "text.txt"))])
    except Exception as e:  # noqa: BLE001
        print("  ! cifrado no disponible:", e)

    # GRANDE: 500 MiB comprimible (test de memoria streaming)
    print("  generando big500.7z (500 MiB)...")
    big_src = "/tmp/arca7z_big500.bin"
    r = rng(30)
    chunk0 = bytes(r.getrandbits(8) for _ in range(4096))
    with open(big_src, "wb") as f:
        written = 0
        idx = 0
        while written < BIG_BYTES:
            block = bytearray(chunk0)
            block[0:8] = idx.to_bytes(8, "little")  # contador por bloque
            f.write(block)
            written += len(block)
            idx += 1
    with py7zr.SevenZipFile(f"{CORPUS}/big500.7z", "w", filters=[L(1)]) as z:
        z.write(big_src, arcname="gigante.bin")
    os.remove(big_src)
    sz = os.path.getsize(f"{CORPUS}/big500.7z")
    print(f"  + big500.7z (500 MiB → {sz/1e6:.1f} MB comprimido, preset 1)")
    record("big500.7z", "big", "500 MiB para test de memoria", [])

    # ---------------------------------------------------------- manifest
    with open(os.path.join(CORPUS, "manifest.txt"), "w", encoding="utf-8") as f:
        f.write("# Corpus arca-7z (T05) — generado por gen_corpus.py (py7zr)\n")
        f.write(f"# py7zr {py7zr.__version__}\n")
        for archive, mode, desc, entries, dirs in manifest:
            f.write(f"ARCHIVE {archive} {mode} # {desc}\n")
            for d in dirs:
                f.write(f"DIR {d}\n")
            for e in entries:
                rel_src = os.path.relpath(e.srcpath, CORPUS)
                sha = hashlib.sha256(open(e.srcpath, "rb").read()).hexdigest()
                f.write(f"ENTRY {e.arcname} {rel_src} {e.size} {sha}\n")

    n = sum(1 for a in manifest)
    print(f"\nOK: {n} archivos en {CORPUS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
