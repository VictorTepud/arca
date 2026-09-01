---
Task ID: T18
Agent: Super Z (agente principal)
Fecha: 2026-09-02

Qué hice: **F3a — probe visual "pantalla viva"**. Primeros píxeles de una
sub-app en pantalla del teléfono + interacción táctil, reutilizando la
grieta de targetSdk 28 ya validada (F0 = GO en hardware).

- `arca-gfx-protocol` (stub → real): [`FrameHeader`] del framebuffer
  (32 B: magic "AFRM", versión, formato, w/h, frame_seq, ts_ms) +
  validación fail-closed + golden del layout byte a byte. Decisión
  documentada: cabecera POD fija en vez de rkyv — el lector del probe es
  Kotlin y 32 bytes fijos son más baratos de auditar que un serializador
  completo en el host de prueba. MeshFrame/rkyv sigue para F3b.
- `arca-shm` + `file.rs`: [`FrameFile`] — adjunto SEGURO de la región de
  frames respaldada por un archivo de `filesDir` (host y hijo comparten
  UID/sandbox → no hace falta pasar memfds por AIPC todavía). Valida que
  el archivo mida `region_len(frame_bytes)` exacto (anti-SIGBUS). Todo el
  unsafe se queda dentro del crate unsafe-heavy; los tests de integración
  corren el protocolo con DOS mapeos independientes (como dos procesos).
- `arca-sdk-ui` (stub → real, 28 tests): canvas software RGBA (fill/rect/
  round-rect/disco/degradado/blit con alfa/blit escalado/texto bitmap),
  fuente bitmap 12×16 rasterizada de DejaVu (generador
  `devapp-demo/tools/gen_font.py`, castellano incluido), parser de input
  (touch/ping/shutdown sin alloc), [`Button`] con hit-test y
  [`paint_frame`] (pinta header+bitmap en el payload de un slot). 100 %
  safe Rust, cero alloc en el path de frame.
- `devapp-demo` (stub → real): la app demo interactiva. Título, logo RGBA
  embebido (blit con alfa + escalado), panel de "video" procedural
  (barras cromáticas animadas + barrido + logo rebotando), 3 botones
  táctiles (Color/Ping/Salir), pelota con física que persigue el dedo,
  telemetría (fps/frames/pings). Bucle a 30 fps con reloj drift-corrected;
  SIGTERM → línea final + exit 0 (contrato del watchdog). Modo
  `--selftest`: renderiza 5 frames con marcadores deterministas y los
  relee por un SEGUNDO mapeo (rol host) — valida seqlock de punta a punta
  en PC sin teléfono.
- `host-probe` + `DemoActivity.kt`: el "display server" de juguete.
  Crea `arca-fb.bin` con la geometría exacta, lo mapea MAP_SHARED,
  extrae y lanza `devapp-demo` (env ARCA_FB/ARCA_FB_W/ARCA_FB_H), lee
  stdout (frame→blit, stats→estado), blit = seqlock read_latest →
  Bitmap.setPixels → SurfaceView escalado; touch→stdin con conversión
  pantalla→framebuffer; watchdog 180 s; vertical fija (la geometría del fb
  se acuerda una sola vez). `MainActivity` gana un segundo botón.
- `arca.sh` (r5): build de los DOS binarios (hello+demo) con gate readelf
  ×2, `run [hello|demo]`, selftest en `test`, `todo` corre el demo.
  `check-graphs.py`: devapp-demo → [gfx-protocol, shm, sdk-ui] (extensión
  de la tabla maestra según su propio protocolo de ampliación).

Decisiones tomadas (y por qué no la alternativa):
- **Framebuffer por archivo en filesDir** y no memfd por AIPC: el host del
  probe es Kotlin y pasar fds por Unix socket desde Java es frágil; el
  archivo compartido da exactamente la misma coherencia (MAP_SHARED) con
  cero protocolo nuevo. El pipeline real (memfd+AIPC) llega en F3b con
  host-core; el contrato de la REGIÓN (seqlock) es idéntico.
- **Software rendering y no GPU**: linkear GLES/Vulkan reintroduce el NDK
  en los binarios estáticos (la restricción que hace viable la grieta).
  CPU + fb compartido es el mínimo que demuestra C→H→pantalla completo.
- **blake3 `pure` en targets musl** (Cargo target-conditional): su C exige
  gcc cruzado y rompería el cross con rust-lld "SIN NDK". Ningún binario
  de probe hashea en camino caliente; glibc conserva el camino rápido.
- **"Video" procedural**: sin decodificador (H.264 en static-musl = otro
  proyecto entero). El panel animado demuestra el pipeline; el video real
  es F4+ (códec o decodificación mediada por host).

Qué rompí/Qué falta:
- Devapp-demo pesa 2.9 MB (arrastra arca-types→blake3 por arca-shm):
  inofensivo para el APK; si molesta, feature-gatear digest en arca-types.
- El blit de Kotlin es CPU→CPU (setPixels + drawBitmap escalado): a 720 px
  de lado mayor rinde sobrado para 30 fps; no es el camino final.
- F3b (siguiente): host-core real, AIPC por UDS con memfd, MeshFrame
  rkyv, wm/input como servicios — el probe se desmonta y el contrato L0
  (FrameHeader + FrameSlots) queda igual.

Verificación: fmt ✓ · clippy -D warnings ✓ · **358 tests ✓** (315 en r4) ·
selftest del demo ✓ · check-graphs ✓ (40 aristas, 0 avisos) · bash -n ✓ ·
cross aarch64 musl: devapp-hello y devapp-demo **static-pie (DYN, 0
DT_NEEDED)** ✓.

Próxima tarea sugerida: correr `./arca.sh todo` en Deepin → demo en el
teléfono → capturar `logs/arca-logs-*.txt`; luego F3b.
