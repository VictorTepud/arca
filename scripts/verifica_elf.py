#!/usr/bin/env python3
# verifica_elf.py — gate estático-PIE de Arca, a nivel de BYTES.
#
# Por qué existe: el gate anterior parseaba el TEXTO de `readelf -h`
# (`awk '/Type:/{print $2}'` + `grep -c NEEDED`). En la Deepin del
# usuario el parseo devolvía vacío aunque el binario estuviera bien
# (error "no es PIE (Type=)" con Type vacío) y abortaba el build.
# Leer el ELF directamente elimina toda dependencia del entorno:
# sin readelf, sin awk, sin binutils, sin locale, sin versiones.
#
# Comprueba, para cada archivo:
#   1. magic ELF + clase (32/64 bits) + endianismo válidos
#   2. e_type == ET_DYN (3)            → es PIE (Android lo exige)
#   3. sin PT_INTERP                   → no necesita linker dinámico
#   4. 0 entradas DT_NEEDED            → sin bibliotecas dinámicas
#
# Uso: python3 scripts/verifica_elf.py BIN [BIN...]
# Sale 0 si TODOS pasan; 1 si alguno falla (motivo impreso en pantalla).

import struct
import sys

ET_DYN = 3
PT_LOAD = 1
PT_DYNAMIC = 2
PT_INTERP = 3
DT_NEEDED = 1
DT_NULL = 0
PN_XNUM = 0xFFFF

MAQUINAS = {0: "ninguna", 3: "x86", 40: "ARM", 62: "x86-64",
            183: "AArch64", 243: "RISC-V"}


def tam_legible(n):
    if n < 1024:
        return f"{n} B"
    for unidad in ("KiB", "MiB"):
        if n < 1024 * 1024:
            return f"{n / 1024.0:.1f} {unidad}"
        if unidad == "MiB":
            return f"{n / (1024.0 * 1024.0):.1f} MiB"


def verificar(ruta):
    """True si el archivo es un ELF estático-PIE; imprime el veredicto."""
    try:
        with open(ruta, "rb") as f:
            datos = f.read()
    except OSError as e:
        print(f"[FAIL ] {ruta}: no pude leerlo ({e})")
        return False

    problemas = []
    # ── cabecera ELF ──────────────────────────────────────────────
    if len(datos) < 64 or datos[:4] != b"\x7fELF":
        print(f"[FAIL ] {ruta}: no es un ELF (magic ausente o truncado)")
        return False
    ei_class, ei_data = datos[4], datos[5]
    if ei_class not in (1, 2):
        print(f"[FAIL ] {ruta}: EI_CLASS={ei_class} inválido (se espera 1 o 2)")
        return False
    if ei_data not in (1, 2):
        print(f"[FAIL ] {ruta}: EI_DATA={ei_data} inválido (se espera 1 o 2)")
        return False
    end = "<" if ei_data == 1 else ">"
    es64 = ei_class == 2

    if es64:
        e_type, e_machine = struct.unpack_from(end + "HH", datos, 16)
        e_phoff, = struct.unpack_from(end + "Q", datos, 32)
        e_phentsize, e_phnum = struct.unpack_from(end + "HH", datos, 54)
        ph_min, dyn_ent = 56, 16
    else:
        e_type, e_machine = struct.unpack_from(end + "HH", datos, 16)
        e_phoff, = struct.unpack_from(end + "I", datos, 28)
        e_phentsize, e_phnum = struct.unpack_from(end + "HH", datos, 42)
        ph_min, dyn_ent = 32, 8

    if e_type != ET_DYN:
        nombres = {2: "EXEC", 3: "DYN", 4: "CORE"}
        problemas.append(
            f"e_type={nombres.get(e_type, hex(e_type))} — debe ser DYN "
            "(estático-PIE): Android rechaza EXEC")

    if e_phnum == PN_XNUM:
        print(f"[FAIL ] {ruta}: más de 65535 segmentos (PN_XNUM) — no soportado")
        return False
    if e_phnum == 0:
        problemas.append("no tiene program headers (e_phnum=0)")
    if e_phentsize < ph_min:
        problemas.append(f"e_phentsize={e_phentsize} < mínimo {ph_min}")
    if e_phoff == 0:
        problemas.append("e_phoff=0 (no hay tabla de segmentos)")

    # ── program headers ───────────────────────────────────────────
    tiene_interp = False
    dinamica = None
    if not problemas:
        for i in range(e_phnum):
            off = e_phoff + i * e_phentsize
            if off + e_phentsize > len(datos):
                problemas.append(f"phdr[{i}] fuera del archivo (¿truncado?)")
                break
            if es64:
                p_type, = struct.unpack_from(end + "I", datos, off)
                p_offset, = struct.unpack_from(end + "Q", datos, off + 8)
                p_filesz, = struct.unpack_from(end + "Q", datos, off + 32)
            else:
                p_type, p_offset = struct.unpack_from(end + "II", datos, off)
                p_filesz, = struct.unpack_from(end + "I", datos, off + 16)
            if p_type == PT_INTERP:
                tiene_interp = True
            elif p_type == PT_DYNAMIC:
                dinamica = (p_offset, p_filesz)

    if tiene_interp:
        problemas.append("tiene PT_INTERP — no es estático (usa ld.so)")

    # ── entradas dinámicas (DT_NEEDED) ────────────────────────────
    needed = 0
    if dinamica is not None and not problemas:
        p_offset, p_filesz = dinamica
        if p_filesz % dyn_ent != 0:
            problemas.append("PT_DYNAMIC corrupto (tamaño no múltiplo)")
        elif p_offset + p_filesz > len(datos):
            problemas.append("PT_DYNAMIC fuera del archivo (¿truncado?)")
        else:
            for k in range(p_filesz // dyn_ent):
                d_tag = struct.unpack_from(
                    end + ("Q" if es64 else "I"), datos, p_offset + k * dyn_ent)[0]
                if d_tag == DT_NULL:
                    break
                if d_tag == DT_NEEDED:
                    needed += 1
            if needed:
                problemas.append(f"{needed} DT_NEEDED — debe ser 0 (estático)")

    # ── veredicto ─────────────────────────────────────────────────
    clase = "ELF64" if es64 else "ELF32"
    bits = "LE" if ei_data == 1 else "BE"
    maq = MAQUINAS.get(e_machine, f"machine {e_machine}")
    base = f"{ruta}: {clase} {bits} · {maq} · {tam_legible(len(datos))}"
    if problemas:
        print(f"[FAIL ] {base}")
        for p in problemas:
            print(f"        - {p}")
        return False
    print(f"[OK   ] {base} · DYN (PIE) · sin PT_INTERP · 0 DT_NEEDED "
          "→ estático-PIE")
    return True


def main():
    if len(sys.argv) < 2:
        print("uso: verifica_elf.py BIN [BIN...]", file=sys.stderr)
        return 2
    todo_ok = all(verificar(ruta) for ruta in sys.argv[1:])
    return 0 if todo_ok else 1


if __name__ == "__main__":
    sys.exit(main())
