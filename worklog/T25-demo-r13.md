# T25 — r13: el grid invisible del lanzador + la UI "muy enorme" de la sub-app

## Síntomas (reportados en hardware por el usuario, tras r12)

1. La sub-app se VE (r12 arregló la pantalla negra) pero "todo se mira
   muy enorme".
2. La app principal "no genera ninguna lista o grid con las apps
   instaladas" — solo título y FAB, aunque la instalación por SAF
   funciona (logcat: "instalada: …/files/exec/devapp-demo").

## Diagnóstico 1 — UI enorme: escala ciega al ancho

r11 calculaba la escala de UI solo con la altura (h/540): en el fb 1:1
del Huawei (720×1536) daba ui=3 — el diseño (cuyo canvas real es
336×720, las coordenadas que `draw` codifica) estirado ×3 sobre una
pantalla que es solo ×2.13 respecto del canvas → elementos ~40% más
grandes que las proporciones r9 que el usuario vio y aceptó.

**Fix**: `ui_scale(w, h) = round(min(w/336, h/720)).clamp(1, 4)`.
Escala por AMBAS dimensiones contra el canvas de diseño: 720×1536→2
(recupera el look r9 a fb 1:1), 664×1440→2, 1049×2160→3, 336×720→1.
El min() además impide que pantallas anchas o alargadas estallen la UI
en un eje.

## Diagnóstico 2 — grid invisible: peso de GridLayout en el eje equivocado

`GridLayout.LayoutParams(rowSpec, columnSpec)` — r11 puso el peso en el
ROW spec y dejó las COLUMNAS sin peso con `width = 0`:

```kotlin
GridLayout.spec(GridLayout.UNDEFINED, 1, 1f),  // ← peso en la FILA
GridLayout.spec(GridLayout.UNDEFINED)          // ← columna SIN peso
lp.width = 0
```

Columna sin peso + width 0 → columnas de 0 px → **todas las celdas (y el
estado vacío "aún no hay apps") invisibles**. El escaneo de
filesDir/exec, el footer ARCAAPP1 y los iconos funcionaban — el grid
entero medía 0 de ancho. El peso de fila, además, era inútil: el grid
vive dentro de un ScrollView (sin exceso vertical).

**Fix** (2 lugares): peso en la COLUMNA — celdas con
`spec(UNDEFINED, 1, 1f)` (1/columnCount del ancho) y estado vacío con
`spec(UNDEFINED, columnCount, 1f)` (ancho completo).

## Verificación

- Unit tests 5/5 (`ui_scale_bordes` reescrito para (w,h): 11 asserts
  incluidos 720×1536→2, ancha 2048×1040→1, angosta 432×4096→1).
- Parche aplicado por script de anclas (scripts/r13_ui_scale.py fuera
  del repo, en my-project/scripts) — notas de sesión: el transporte de
  esta sesión manglea la secuencia `#[` en old_str del Edit tool; se
  parcheó por índice de línea con verificación de anclas.
- Harness qemu 10.0.11: **86/86 OK** con el binario r13. La fila del
  panel H ahora se calcula con el espejo ui_mirror(720,1536)=2
  (54/80 píxeles cromáticos — el panel sigue vivo en la posición
  nueva). La X verificada en D/E/G/I con las zonas desplazadas por los
  ui nuevos (todas blancas opacas + exit 0 + reason=x).
- `./arca.sh build` end-to-end OK: gate ELF estático-PIE, footers
  ARCAAPP1, APK BUILD SUCCESSFUL. aapt2: versionCode 5 /
  versionName 0.1.0-f3a.r13.

## Archivos

- `crates/L3-devapps/devapp-demo/src/main.rs` — ui_scale(w,h) + docs +
  tests + geometría del Huawei en los tests de zona.
- `host-probe/.../MainActivity.kt` — specs de GridLayout corregidos
  (celdas + estado vacío).
- `scripts/demo_qemu_check.py` — ui_mirror(w,h) por ambas dimensiones +
  fila del panel H dinámica.
- `host-probe/app/build.gradle.kts` — versionCode 5 / r13.

## Pendiente

- Probar en el Huawei: (1) el lanzador debe mostrar el grid con "Demo
  Arca" (o el nombre del footer del binario que instales con +) con
  icono; mantener pulsado = desinstalar. (2) La sub-app debe verse con
  proporciones normales (como r9 pero nítida): título, panel, pelota,
  botones y X más pequeños que en r12.
- Si el texto de la sub-app ahora quedara chico de más, NO tocar la
  escala global: subir el escalón tipográfico (bitmap font) en un r14.
