# devapp-calc — calculadora completa del contenedor (F3a, r14)

Primera sub-app "de verdad" del contenedor: una calculadora completa que
corre por el pipeline del probe visual (ELF estático-PIE sin NDK,
framebuffer compartido con seqlock, stdio JSON, lanzada por
`DemoActivity` y visible en el grid del lanzador con su icono).

## Qué calcula

- **Aritmética decimal EXACTA** (mantisa `i64` × escala, intermedios en
  `i128`): `0.1 + 0.2 = 0.3`, sin la basura binaria de `f64`.
- **Precedencia de operadores**: `12 + 3 × 4 = 24` (× ÷ antes de + −).
- Porcentaje (`50% → 0.5`), signo (±), borrar carácter, corrección del
  último operador apilado, encadenado desde el resultado (`2+3= ×4 → 20`).
- División por cero y desborde → **Error** (recuperable con C o tecleando).
- Rango: |x| entre ~10⁻³⁸ y ~9.2×10¹⁸; lo diminuto trunca a 0, lo gigante
  da Error. La división trunca a ~18 dígitos significativos (documentado
  en `src/calc.rs`).

## Layout (canvas de diseño 336×720, `ui` por ambas dimensiones — r13)

```text
  0..56    barra de título "Calculadora" + X de cierre (r10)
 68..200   panel display: eco de expresión / entrada-resultado (auto 3×2×1)
           / historial "12+3*4=24"
 208..     grid 4×5: C % < / · 7 8 9 * · 4 5 6 - · 1 2 3 + · +/- 0 . =
           (anclado al fondo, sobre la barra de 52·ui)
 h-52..    barra inferior: pista de uso + telemetría fps/frames
```

## Uso

```bash
cargo test -p devapp-calc                    # 27 tests (motor + geometría)
cargo run -p devapp-calc -- --selftest       # pipeline seqlock + escenarios
./arca.sh run calc                           # subir al teléfono y lanzar
```

En el teléfono también se instala con el botón **+** del lanzador
(SAF): el binario lleva el footer `ARCAAPP1` con nombre "Calculadora" e
icono (ver `tools/gen_icono.py` + `scripts/empaqueta_app.py`).

## Estructura

- `src/calc.rs` — motor: `Dec` (decimal exacto), `eval` con precedencia,
  `Entrada` (modelo de tecleo). 100 % testable, sin I/O.
- `src/main.rs` — protocolo (idéntico al demo: hello/frame/stats/pong/
  exiting/sigterm), estado de la app, render con `arca-sdk-ui`,
  `--selftest` y tests de geometría.
- `scripts/calc_qemu_check.py` (en la raíz del repo) — harness qemu del
  contrato completo: protocolo + display determinista (hash del panel:
  `1/0=` → Error, `C` restaura EXACTO, `7*6=` → 42).
