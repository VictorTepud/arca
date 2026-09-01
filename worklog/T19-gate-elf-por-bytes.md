---
Task ID: T19
Agent: Super Z (agente principal)
Fecha: 2026-09-02

Qué hice: **fix del gate estático-PIE** — el build de `arca.sh` se
abortaba en la Deepin del usuario con `[ERROR] devapp-hello no es PIE
(Type=) — Android lo rechazaría` aunque los binarios estaban BIEN
(compilaron y en otra máquina el mismo gate daba DYN). El `Type=` vacío
delataba que el problema no era el binario sino el **parseo textual** de
readelf+awk en ese entorno: `tipo="$(readelf -h "$bin" | awk
'/Type:/{print $2}')"` devolvía cadena vacía.

- Reproducción: en el sandbox de desarrollo el mismo gate SÍ daba
  `tipo=[DYN]` (binutils 2.44, mawk, locale es probado) → diferencia de
  entorno, no lógica. En lugar de perseguir la versión exacta de
  binutils de esa Deepin, se eliminó la dependencia de raíz.
- `scripts/verifica_elf.py` (nuevo): verificación **a nivel de bytes**
  del ELF con python3 (el proyecto ya depende de python3: `ci.sh` corre
  `check-graphs.py`). Comprueba magic, EI_CLASS/EI_DATA, **e_type ==
  ET_DYN**, **sin PT_INTERP** (nuevo: el gate viejo ni lo miraba) y **0
  DT_NEEDED** recorriendo PT_DYNAMIC. Soporta ELF64/ELF32, LE/BE, con
  guardas de truncado y fail-closed en todo borde. Mensajes de error
  accionables en español (qué campo, qué valor, qué se esperaba).
- `arca.sh` (r5→r6): el gate ahora llama al verificador por bytes;
  agrega chequeo previo de python3 y de existencia del binario (antes
  un path malo se reportaba como "no es PIE", engañoso). El gate corre
  igual con o sin binutils/awk/locale: probado con un PATH que solo
  contiene python3 y basename.
- Bug propio encontrado por la batería de tests: había leído `p_vaddr`
  (offset 16 del phdr) en vez de `p_filesz` (offset 32) → falso
  "PT_DYNAMIC corrupto". Corregido y contrastado contra `readelf -l`
  (DYNAMIC: offset 0x61288, size 0x140 = 20 entradas exactas).

Verificación: batería de 11 pruebas sobre los binarios reales r5 +
mutaciones byte a byte (e_type=EXEC, PT_INTERP inyectado, DT_NEEDED
inyectado, truncado, no-ELF, inexistente, mixto) — 11/11 ✓ · gate
completo sin readelf/awk en PATH ✓ · `bash -n arca.sh` ✓ · contraste
manual con readelf -h/-l ✓.

Próxima tarea sugerida: reintentar `./arca.sh build` (y `todo`) en la
Deepin; el gate ya no depende de sus herramientas locales.
