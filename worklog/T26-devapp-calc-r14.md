# T26 — r14: devapp-calc, la calculadora completa del contenedor

## Qué hice

`devapp-calc`: la primera sub-app "de verdad" del contenedor — una
calculadora completa que corre por el pipeline F3a (mismo protocolo del
demo: hello/frame/stats/pong/exiting/sigterm + stdin touch/ping/shutdown)
y aparece en el grid del lanzador con nombre e icono propios (footer
ARCAAPP1: "Calculadora").

Motor (`src/calc.rs`, 100 % testable, sin I/O):
- **Decimal exacto** `Dec { man: i64, esc: u8 }` con intermedios `i128`:
  `0.1+0.2 = 0.3` exacto (la motivación: una calculadora que responde
  `0.30000000000000004` está rota). Suma/resta/multiplicación/porcentaje
  exactos; solo la división trunca (~18 dígitos significativos).
- Rango |x| ∈ (~10⁻³⁸, ~9.2×10¹⁸): subdesborde → 0; desborde →
  `MathErr::Overflow` → "Error" en pantalla (recuperable con C).
- `eval` con **precedencia** (× ÷ antes de + −) en dos pasadas de
  plegado sobre buffers de capacidad fija (sin alloc).
- `Entrada`: modelo de tecleo con eco tal-como-se-tipea, supresión de
  ceros líderes, cota de 18 dígitos (mantisa siempre parseable).

App (`src/main.rs`): grid 4×5 estilo iOS (C % < / · 7 8 9 * · 4 5 6 - ·
1 2 3 + · +/- 0 . =), panel de display con 3 líneas (eco de expresión,
entrada/resultado con escalón tipográfico 3→2→1 alineado a la derecha,
historial "12+3*4=24"), corrección de operador apilado (2+* → 2*),
encadenado desde resultado (2+3= ×4 → 20), X de cierre (r10) y barra
inferior con pista + telemetría. Todo el path de frame sin alloc (buffers
de capacidad fija; el eco clipea con '~' prefijo cuando no cabe).

## Decisiones tomadas

1. **Decimal exacto (i64×escala + i128) en vez de f64**: exactitud
   visible por el usuario; el formateo es trivial (sin notación
   científica). La alternativa f64 ahorraría ~150 líneas pero rompe el
   contrato social de una calculadora.
2. **Congelar números en `nums[n_ops]`** (no `n_ops+1`): el primer número
   de la expresión vive en `nums[0]` — el índice alternativo dejaba un
   cero fantasma inicial y `2*3` daba 0 (cazado en revisión de código
   antes de probar en hardware; los tests de escenarios lo cubren ahora).
3. **Layout anclado al fondo** (grid sobre la barra de 52·ui, con piso
   de 16 px por botón): en fbs degenerados (qemu 160×360) el grid puede
   pisar la barra, que se pinta DESPUÉS y lo tapa — nunca solapa el
   display. El demo anclaba botones y zona de pelota por separado.
4. **Harness por hash del panel** (`scripts/calc_qemu_check.py`): el
   display de la calculadora es una función determinista del estado (nada
   animado dentro del panel) → hash SHA-256 del rect del display:
   `1/0=` cambia el panel (Error), `C` lo restaura EXACTO, `7*6=` produce
   otro distinto (42). Prueba de integración input→estado→render sin
   OCR de píxeles de texto.
5. **Mismo 30 fps del demo** aunque la calculadora es estática: el host
   pacea el blit con los eventos frame; mantener el patrón probado vale
   más que ahorrar CPU en un probe.
6. Icono: silueta pixel-art de calculadora sobre degradado AZUL (color
   de operadores del demo) — distinta del teal del devapp-demo para
   distinguirlas en el grid de un vistazo.
7. Drive-by: `cargo fmt -p devapp-demo` — el main.rs del demo traía
   deriva de formato del r13 (2 import-order/wrap en tests); cambio
   cosmético, sin lógica.

## Verificación

- `cargo test -p devapp-calc`: **27/27** (motor: Dec/eval/Entrada;
  app: escenarios, error/recuperación, cota, geometría en 5 geometrías
  incl. 720×1536 del Huawei; ui_scale/x/fps copiados del demo con sus
  regresiones).
- `--selftest` en PC y bajo qemu-aarch64 8.2.2: OK (5 frames seqlock de
  punta a punta + 10 escenarios + error/historial).
- `cargo fmt --all -- --check` + `clippy -p devapp-calc --all-targets
  -D warnings`: verdes.
- `scripts/check-graphs.py`: OK (34 crates, devapp-calc en capa 3 con
  las mismas deps que el demo).
- Cross a aarch64 musl: **gate estático-PIE verificado por bytes**
  (`verifica_elf.py`), footer ARCAAPP1 re-parseado OK (2.9 MiB).
- **`scripts/calc_qemu_check.py`: 17/17** con el binario REAL
  (720×1536): hello/pong/stats-sane/frames+slots alternando/AFRM por
  slot/display determinista (Error, C exacto, 42)/X → exit 0
  reason=x.

## Qué rompí / Qué falta

- Nada roto conocido. El fix de pantalla negra (r12) y el del grid
  invisible (r13) ya estaban en el remoto — esta sesión recuperó el
  workspace tras el reset del sandbox (re-clonado desde GitHub) antes
  de empezar.
- Pendiente en hardware: probar en el Huawei (`./arca.sh run calc` o
  instalar el binario con el + del lanzador) — bajo qemu todo el
  contrato está verde, pero la geometría real del visor y el tacto real
  solo se ven en el teléfono.
- El número r14 estaba "reservado" para un posible escalón tipográfico
  (T25, pendiente); se usó para la calculadora — si el texto del demo
  queda chico de más en el Huawei, ese ajuste va en r15.

## Próxima tarea sugerida

Probar en el teléfono y reportar `logs/`. Después: F3b (host-core real,
AIPC con memfds, MeshFrame rkyv, wm/input) sobre esta misma base.
