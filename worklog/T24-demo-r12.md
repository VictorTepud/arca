# T24 — r12: la pantalla negra del visor (bug de orden en surfaceChanged)

## Síntoma (reportado en hardware por el usuario)

La demo r11 arranca desde el lanzador, todos los logs lucen sanos —
hello con la geometría correcta, 30 fps, slots 0/1 alternando, blits
272/273 — pero la pantalla queda **negra**. El usuario cierra con atrás
a los ~9 s (exit 0 limpio).

    vista 720x1536 → fb 720x1536 (escala de blit 1,000)
    hijo listo: {"event":"hello",...,"w":720,"h":1536}
    frame seq=1 slot=0 … stats: frames=120 fps=30 · blits: 120
    exit code = 0 (blits: 272/273 frames)

Dato nuevo del reporte: el Huawei real entrega una superficie de
**720×1536** (HD+ 720×1600 menos barra) — no el FHD ~1049×2160 que se
asumió al planear r11. El fb sale 1:1 con la vista (blit sin escalado).

## Diagnóstico

El hijo renderiza contenido real (el harness lo prueba por píxeles);
el host lee frames válidos y los pinta. La pantalla negra con blits
sanos solo puede venir del **rect destino del blit**.

Raíz (regresión introducida en r11, `DemoActivity.surfaceChanged`):

```kotlin
override fun surfaceChanged(h, format, w, h2) {
    computeDstRect(w, h2)        // ← fbW/fbH valen 0 todavía
    if (!started && w > 0 && h2 > 0) {
        dimensionarFb(w, h2)     // ← recién AQUÍ se dimensiona el fb
        ...
    }
}
```

r11 movió el dimensionado del fb dentro de `surfaceChanged` (para usar
la superficie real bajo la barra) pero dejó `computeDstRect()` ANTES de
`dimensionarFb()`: con fb 0×0 el guard de `computeDstRect` sale temprano
y **`dstRect` queda `(0,0,0,0)` para siempre**. El blit hace
`drawColor(BLACK)` + `drawBitmap(bmp, null, dstRect, paint)` → un rect
destino vacío no pinta nada → pantalla negra con todos los contadores
subiendo (framesBlit++ está en el `finally`, tras un unlock exitoso de
un canvas que solo recibió el negro del `drawColor`).

En r10 no pasaba porque el fb se dimensionaba en `onCreate`
(displayMetrics) y `computeDstRect` corría ya con valores. El toque
también estaba roto (mapeo pantalla→fb con `dstRect.width()==0`).

## Corrección (3 capas)

1. **Orden**: en el primer `surfaceChanged` se llama
   `computeDstRect(w, h2)` DESPUÉS de `dimensionarFb`; los eventos
   posteriores (rotación/relayout) re-centran como antes.
2. **Autodefensa**: `blit()` detecta `dstRect.isEmpty` y lo reconstruye
   con `holder.surfaceFrame` (si un callback se pierde, el blit se
   auto-repara en vez de pintar a un rect vacío).
3. **Tripa al logcat**: `computeDstRect` ahora loguea el rect —
   `blit dst=Rect(0, 0 - 720, 1536) (fb 720x1536 en vista 720x1536)`.
   Un rect vacío se ve al instante en el próximo reporte de logs.

## Verificación

- **Hijo intacto** (no se tocó): harness qemu 10.0.11 → **86/86 OK**
  (antes 67/67), con 2 casos NUEVOS en la geometría REAL del Huawei:
  * `H(720x1536)`: protocolo completo + **contenido**: la fila central
    del panel de video tiene 35/52 píxeles cromáticos — el fb NO es
    negro en esa geometría (cierra la brecha que dejó pasar el bug:
    720×1536 nunca se había probado, estaba entre C y F).
  * `I(720x1536)`: botón X — pixel blanco opaco + exit 0 + reason=x.
- Unit tests del demo: 5/5 (`cargo test -p devapp-demo`).
- `./arca.sh build` end-to-end OK: gate ELF estático-PIE (2 binarios),
  footers ARCAAPP1 re-empaquetados (idempotente), APK BUILD SUCCESSFUL.
- aapt2 badging: versionCode 4, versionName `0.1.0-f3a.r12`.
- Nota honesta: la lógica de ciclo de vida del host (SurfaceHolder)
  no es ejecutable en el sandbox — el tripwire del log es la cobertura
  de esta clase de bug en hardware.

## Archivos

- `host-probe/.../DemoActivity.kt` — orden + autodefensa + log.
- `scripts/demo_qemu_check.py` — casos H e I (geometría real 720×1536).
- `host-probe/app/build.gradle.kts` — versionCode 4 / r12.

## Pendiente

- Probar en el Huawei: abrir la demo desde el grid → debe verse el
  contenido (fondo degradado, panel cromático, pelota, X) con la barra
  de notificaciones visible. El logcat debe mostrar
  `blit dst=Rect(0, 0 - 720, 1536)`.
- Si aparece otra anomalía: `./arca.sh logs` y enviar el archivo.
