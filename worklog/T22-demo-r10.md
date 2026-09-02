---
Task ID: T22
Agent: Super Z (agente principal)
Fecha: 2026-09-02

Qué hice: **r10 — la app anfitriona deja de verse como probe.** Pedido
directo del usuario tras ver r9 en su Huawei: "sigue viéndose pixelado,
mejora la app anfitriona; quita los detalles y el botón de detener (pon
una X en la esquina superior dentro de la sub-app); que corra a pantalla
completa sin el bloque del nombre de la app; y añade abrir cualquier
binario desde el almacenamiento".

Cambios (4 pedidos → 4 entregas):

1. **Pixelado**: MAX_LADO 720 → 1440 en DemoActivity. En un 1080×2340 el
   fb pasa de 336×720 a ~664×1440 (4× píxeles) y el escalado del blit
   bilinear baja de ~3.2× a ~1.6×. La sub-app escala TODA su UI por
   `ui_scale()` (diseño 720p × round(h/720), clamp 1..3; 1080→2
   deliberado: mejor grande que diminuto) — mismo layout, doble nitidez.
   Velocidades/radios de la pelota escalan igual (velocidad VISUAL
   constante). El render CPU a 960K px/frame y el bucle RGBA→ARGB del
   blit siguen sobradamente dentro de los 33 ms (qemu sostiene 30 fps
   hasta en 664×1440).

2. **X de cierre DENTRO de la sub-app**: `zona_x()/x_hit()/draw_x()`
   (chip redondeado translúcido + X blanca, hit en Down). Tocarla →
   `exit = Some("x")` → exiting + exit 0 → el host hace finish() y vuelve
   al home. El botón "Salir" del demo se ELIMINÓ (quedan Color/Ping): una
   sola forma de cerrar, y vive donde debe — en la sub-app.

3. **Fullscreen sin chrome del host**: DemoActivity pierde el TextView
   de estado y el botón Detener: root = FrameLayout + SurfaceView a
   pantalla completa. Tema propio `Theme.ArcaView`
   (NoActionBar.Fullscreen, fondo/status/nav negros) + inmersivo sticky
   clásico (systemUiVisibility; sin AndroidX). Los errores ahora son
   Toasts, la telemetría vive solo en logcat. El hijo muere → finish()
   automático (X, watchdog o señal).

4. **Abrir binarios desde el almacenamiento**: MainActivity es ahora el
   HOME de Arca (lanzador): "Ejecutar demo incorporada" + "Abrir binario
   desde el almacenamiento…" (SAF ACTION_OPEN_DOCUMENT */*) + lista de
   instaladas en filesDir/exec (tocar = ejecutar, mantener = borrar con
   confirmación). El URI se copia a filesDir/exec/<nombre sanitizado> +
   chmod 7→ y se pasa a DemoActivity por el extra "bin" — la MISMA
   grieta de targetSdk 28 que usa el probe desde F0 (untrusted_app_27
   permite execve en /data/data). Sirve para cualquier devapp del repo:
   ELF aarch64 estático-PIE + contrato stdout frames / stdin touch / env
   ARCA_FB. El bin externo inválido → IOException al spawn → Toast con
   el errno (típico: no era ELF aarch64).

Extras: versión APK 0.1.0-f3a.r10 (versionCode 2), textos de arca.sh
(run/logs) actualizados, strings nuevas, themes ArcaHome/ArcaView.

Verificación (sandbox, todo con el binario aarch64 REAL):
- Unit tests `cargo test -p devapp-demo`: 4/4 (fps r9 + ui_scale +
  zona_x/hit en 4 geometrías).
- Gate `verifica_elf.py`: demo 2.9 MB y hello estáticos-PIE OK.
- **Harness NUEVO commiteado** `scripts/demo_qemu_check.py` (el de r9
  no llegó al repo): 49/49 bajo qemu 10.0.11 — runs A(160×360)/B(336×720)
  /C(664×1440): hello, geometría, frames (485/303/365 → 30 fps en las
  3), slots 0/1 alternando, AFRM+seq impar en ambos slots, exiting
  shutdown, fps stats=[30,30,30] (regresión r9 cubierta). Runs D/E: la X
  en ui=1 y ui=2 — toque fuera NO mata, píxel del centro de la X blanco
  opaco (235,238,245,255), toque en la X → exiting reason="x" + exit 0,
  sin sigterm.
- **PRIMERA VEZ el APK se compila en el sandbox** (cmdline-tools +
  platform-34 + build-tools 34 + Gradle 8.9, JDK 21): BUILD SUCCESSFUL,
  aapt2 badging OK (label Arca, targetSdk 28, launchable MainActivity).
  El APK del sandbox va en download/ para instalar directo (firma debug
  DISTINTA a la de Deepin: adb install -r fallará por firma → desinstala
  antes o deja que arca.sh install lo haga).

Costo del sdk en el sandbox: 458 MB bajo .arca-tools/ (gitignored).

Próxima tarea sugerida: aplicar r10 en Deepin (git pull o parche),
./arca.sh build && install && run; confirmar: sin pixelado grosero, sin
bloque de nombre, X funciona, abrir un devapp propio desde
almacenamiento. Luego F3b (H.264 real, arca-rt input thread, AIPC).
