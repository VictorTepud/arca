# T23 — r11: lanzador con grid+iconos, fb 1:1 real y barra visible

Fecha: 2026-09-02 · Rama: main · Commit: (r11)

## Contexto

Feedback del usuario tras probar r10 en su Huawei: "funciona bien, puede
correr binarios desde archivos" (SAF OK), pero:

1. Al abrir una sub-app debe MANTENER la barra de notificaciones (r10 la
   escondía con inmersiva sticky).
2. Las dimensiones deben calcularse RESTANDO la barra de notificaciones
   (r10 usaba displayMetrics de pantalla completa) → sigue viéndose
   pixelado.
3. "Las letras pequeñas ahora son miniaturas" (ui base 720 perdía un
   escalón en los fbs altos).
4. Interfaz principal: quitar TODO el texto técnico ("host F3a ·
   targetSdk 28 (grieta de exec)") y la demo incorporada (ya la carga con
   el botón).
5. El botón de abrir → UN botón circular "+".
6. Lista de instaladas en GRID con icono y nombre; el usuario no sabía
   cómo hacer que las apps "vengan con icono desde la compilación".

## Qué se hizo

### DemoActivity (visor)
- SIN inmersiva: `Theme.ArcaView` pasa de `NoActionBar.Fullscreen` a
  `NoActionBar` con statusBarColor teal → la barra queda VISIBLE y el
  contenido se recorta bajo ella (lo que pedía el punto 2).
- El fb se dimensiona en el PRIMER `surfaceChanged` con el tamaño REAL
  de la vista (pantalla − status bar), cap `MAX_LADO` 2160 (antes 1440
  sobre displayMetrics): en un FHD el fb es ~1049×2160 → blit 1.03×
  (antes 1.6× = pixelado). El arranque del hijo se difiere a ese evento
  (onResume ya no dispara nada).
- Demo incorporada ELIMINADA: el binario llega SIEMPRE por el extra
  "bin"; sin extra → toast + finish. Fuera `installBinary`/asset.

### MainActivity (lanzador)
- Grid (GridLayout 3/4 columnas) de apps instaladas: icono 56dp +
  nombre 2 líneas. Toque = ejecutar; mantener = desinstalar (diálogo).
- FAB circular "+" teal (GradientDrawable oval + elevation) → SAF
  `ACTION_OPEN_DOCUMENT` → copia a filesDir/exec + chmod + lanza
  (comportamiento r10 que el usuario ya validó).
- Sin título "Arca", sin subtítulo técnico, sin botón demo: solo
  "Aplicaciones", el grid y el +. Estado vacío con pista de uso.

### Icono y nombre desde la compilación (footer ARCAAPP1)
- `scripts/empaqueta_app.py`: agrega al final del ELF
  `[nombre][u16][PNG][u32][b"ARCAAPP1"]` — el loader de ELF ignora los
  bytes tras el último segmento, así que sigue siendo ejecutable.
  Idempotente (re-empaquetar no acumula), valida ELF/PNG/límites.
- `tools/gen_icono.py` + `assets/icono.png` (192×192, PNG puro sin PIL).
- MainActivity parsea el footer (espejo Kotlin, fail-closed: footer
  corrupto → avatar con la inicial; PNG decodificado con inSampleSize).
- `arca.sh build` empaqueta demo ("Demo Arca" + icono) y hello
  ("Arca Hello" sin icono). Documentado en el README del demo.

### devapp-demo
- `ui_scale`: base 720→540, cap 3→4 (h≈2160→ui=4: el texto pequeño
  gana un escalón completo — era la "miniatura" del feedback).
- `zona_pelota`: tope anclado bajo el panel de video (la fórmula vieja
  dejaba la pelota rebotando DENTRO del panel con base 540).
- Telemetría compacta (sin mini-logo; línea inferior "fps/frames") —
  el ancho de diseño cayó a ~249 columnas y la línea r10 clipeaba.
- **BUG REAL del bucle principal**: si el render tarda >FRAME_MS
  (teléfono lento, qemu), `wait==0` en cada tick y la rama vieja NUNCA
  llamaba `poll_stdin` → shutdown/toques esperando en el pipe (el host
  mataba por señal, con lag de input). Ahora el drenaje es no-bloqueante
  en cada tick. Detectado porque el qemu de este sandbox (7.2) es más
  lento que el de la sesión r10 (10.0) y el harness C colgó al apagar.

### arca.sh
- Sin assets: `build` empaqueta footers tras el gate ELF; `run demo`
  sube el binario por `run-as` (pipe base64 por stdin, inmune a labels
  SELinux de shell_data_file) + `am start --es bin`;
  `run home` abre el lanzador. `run hello` = alias de home.

## Verificación

- Unit tests demo 5/5 (ui_scale 540, zona_pelota bajo panel, X en 6
  geometrías incluida 1049×2160).
- `--selftest` OK (render→publish→seqlock→lectura).
- Gate ELF (bytes): demo 2.9 MB y hello 2.5 MB static-PIE; footer
  re-parseado OK e idempotente (3007792 B estable en 2 pasadas).
- Harness qemu (7.2): **67/67** — A/B/C/F + X en D/E/G, incluida la
  geometría r11 real 1049×2160 (fps≈8 bajo TCG = esperado en emulación;
  protocolo, rotación de slots, X dibujada y exit-x correctos).
- `./arca.sh build` end-to-end OK (con shim JDK: este sandbox solo
  tiene JRE; en Deepin el JDK completo del usuario no lo necesita).
- `./arca.sh run demo` validado con adb fake: quoting del pipe base64,
  chequeo de tamaño y `am start --es bin` correctos.
- APK: versionCode 3, versionName 0.1.0-f3a.r11, targetSdk 28 (la
  grieta intacta), 804 KB, SIN assets (antes ~3 MB).

## Pendiente

- Usuario: probar en el Huawei (instalar APK o pull+build), confirmar
  nitidez 1:1, texto legible, barra visible, grid con iconos, y el
  run-as de `./arca.sh run demo`.
- Si el teléfono no llega a 30 fps con fb 2160: bajar MAX_LADO en
  DemoActivity.kt (una constante) — pero antes medir: el Rust pinta
  2.27 Mpx/frame y el bucle Kotlin RGBA→ARGB es el otro cuello.
- Push a GitHub: hace falta PAT fresco (el de r9/r10 quedó expuesto en
  el chat y se pidió revocarlo).
- Siguiente: F3b (H.264 real, input de arca-rt, AIPC host-core).
