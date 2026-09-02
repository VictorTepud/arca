---
Task ID: T21
Agent: Super Z (agente principal)
Fecha: 2026-09-02

Qué hice: **r9 — pulido de la demo F3a tras el GO en hardware del
usuario**. La demo corría 30 fps estables (r8 OK) pero el logcat mostraba
tres síntomas raros; dos eran bugs míos y uno era diseño confundiéndose
con bug.

Síntomas reportados en el teléfono (logcat del usuario):
1. `stats: frames=2160 fps=4608229846042376` — fps basura (~4.6e15).
2. La app "moría" (parecía normal) a los ~5401 frames.
3. `slot=0` siempre en las líneas `frame seq=… slot=…`.

Diagnósticos:
1. **fps basura = underflow de u64 en el stats** (bug real). La fórmula
   era `(STATS_CADA - stats_f0) * 1000 / dt`: restaba la CONSTANTE 120
   de un contador ACUMULADO. 1.er intervalo: 120-0=120 (bien, casualidad
   porque f0=0). 2.º: 120-120=0 → fps=0. Desde el 3.º: 120-240 → wrap a
   ~2^64 → `(2^64-x)*1000/dt` ≈ 4.6e15 — calca el patrón del logcat del
   usuario ([30, 0, 4.6e15, …]; los valores variaban porque `dt` tiembla
   unos ms). El selftest nunca lo vio: solo publica 5 frames, no llega
   al 3.er intervalo de stats. Fix: delta REAL de frames
   (`demo.frames - stats_f0`, saturando) + helper `fps_medida` con
   `saturating_mul` y `dt.max(1)`.
2. **muerte a los 5401 frames = el watchdog de 180 s** (diseño, no
   bug): 30 fps × 180 s = 5400 + 1 en vuelo = 5401; SIGTERM → exit 0
   limpio → "parecía normal" (el contrato de apagado limpio funcionó
   tan bien que confundió). WATCHDOG_S 180 → 900 (15 min) +
   FLAG_KEEP_SCREEN_ON (sin él, el timeout de pantalla congela el
   SurfaceView y EMUI puede matar el proceso en segundo plano).
3. **slot=0 fijo = aliasing de muestreo** (no bug): el host logueaba 1
   de cada 60 frames; 60 es múltiplo de 2 (slots) → siempre caía el
   mismo slot. La rotación SÍ funcionaba. Fix cosmético: pacing 60 → 61
   (coprimo con 2) → el slot logueado alterna 0/1/0/1 y el logcat
   DEMUESTRA la rotación.

Extras en el mismo pase:
- Telemetría en pantalla honesta: la línea de abajo ahora muestra el
  fps MEDIDO del último stats (antes mostraba el teórico fijo).
- Rama muerta de resincronización de cadencia: la condición vieja
  comparaba `now` contra el `next` recién reescrito (nunca verdadero).
  Ahora: `next += FRAME_MS` anclado al calendario + resync real si el
  atraso supera 200 ms (sin ráfagas de catch-up tras un congelón).
- Pixelado ("como video juego antiguo"): EXPLICADO, no "arreglado" —
  es el presupuesto de F3a: fb 336×720 (MAX_LADO=720) escalado ~3.2× a
  la pantalla física por el blit de Kotlin con bilinear ya activo.
  Render CPU: subir la resolución es ~cuadrático en píxeles (4× área =
  4× costo de render + 4× blit Java). El look "retro" es inherente al
  software rendering de F3a; F4/F5 (GPU/códec) lo resuelven de raíz.

Verificación (sandbox, binario aarch64 REAL bajo qemu 10.0.11):
- selftest aarch64: OK, exit 0.
- Nuevo harness de modo teléfono (scripts/demo_qemu_check.py, 12
  comprobaciones): Run A 160×360×16 s → exit 0, hello, 473 frames,
  **fps=[30, 30, 30]**, slots 0/1 alternando frame a frame, pong,
  exiting, fb con seq impar + AFRM en ambos slots. Run B 336×720
  (geometría del teléfono) × 10 s → exit 0, hello, 291 frames,
  stats fps=[30, 30].
- **Prueba de mutación**: reintroducida la fórmula vieja → el harness
  FALLA con `fps=[30, 0, 4658268705482210]` (patrón exacto del
  teléfono) → la regresión queda cubierta de verdad.
- Unit tests (`cargo test -p devapp-demo`): 2/2
  (stats_fps_no_hace_underflow, fps_medida_bordes).
- Gate verifica_elf.py: estático-PIE OK (2.8 MiB).

Limitación conocida (para F3b): el bucle solo hace poll de stdin
mientras espera el tick (wait > 0); si el render tarda más que el
período, el input se demora. En el teléfono el render (~ms) va sobrado,
pero un hilo de input dedicado es el diseño correcto (arca-rt).

Próxima tarea sugerida: aplicar el parche r9 en Deepin, rebuild
(recompila devapp-demo y re-empaqueta el APK), correr la demo > 3 min
sin muerte del watchdog, mirar stats con fps≈30 en logcat y slot
alternando; luego F3b.
