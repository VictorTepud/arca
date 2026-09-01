# devapp-demo

App demo **interactiva** del probe visual F3a: pinta una UI completa en
pantalla (botones, imagen embebida, "video" procedural) y responde al dedo.

- Capa: L3 (`L3-devapps/devapp-demo/`)
- Spec: `specs/devapps-*.md` (blueprint) · Tarea: T23 adelantada a F3a
- Estado: **funcional en PC (selftest) y lista para el teléfono**

## Qué muestra en pantalla

| Elemento | Qué demuestra |
|---|---|
| Título + barras | texto bitmap 12×16 (castellano incluido), degradados |
| Logo + mini logo | imagen RGBA embebida (`assets/logo.rgba`): blit con alfa y blit escalado |
| Panel "video procedural" | animación a 30 fps (barras cromáticas + barrido + logo rebotando) — placeholder honesto: video real = F4+ (códec) |
| Botones Color / Ping / Salir | hit-test táctil, estados pressed, acciones |
| Pelota | física simple; **persigue tu dedo** cuando arrastras |
| Barra inferior | telemetría: fps, frames, pings, coords del toque |

## Cómo funciona (F3a)

```
host Kotlin (DemoActivity)              devapp-demo (este binario)
──────────────────────────              ─────────────────────────
crea filesDir/arca-fb.bin ─── env ────▶ ARCA_FB / ARCA_FB_W / ARCA_FB_H
mmap RW (MAP_SHARED) ────────────────── mmap RW (FrameFile::open)
touch → stdin JSON ────────────────────▶ parse (sdk-ui) → estado
◀── stdout {"event":"frame"} ────────── render CPU → publish (seqlock)
blit → SurfaceView ◀──────────────────── (double-buffer de arca-shm)
```

- Frame = [`FrameHeader`] (32 B, `arca-gfx-protocol`) + bitmap RGBA8888.
- Protocolo stdout: `hello`/`frame`/`stats`/`pong`/`exiting`/`sigterm`/`fatal`.
- SIGTERM → línea final + `exit 0` (contrato del watchdog del host).

## Compilar y probar (PC, sin teléfono)

```bash
cargo build -p devapp-demo
./target/debug/devapp-demo --selftest
# selftest: OK (5 frames, seqlock de punta a punta)
```

El `--selftest` crea un framebuffer temporal, renderiza 5 frames con
marcadores de color conocidos y los RELEE por un segundo mapeo (el rol del
host): valida render → publish → seqlock → lectura sin depender del
teléfono. `./arca.sh test` lo corre como parte de la suite.

## En el teléfono

`./arca.sh build` lo compila a aarch64 musl **static-pie SIN NDK** (ver
`.cargo/config.toml`), lo copia a los assets del APK y `./arca.sh run demo`
abre `DemoActivity` (vertical fija: la geometría del framebuffer se acuerda
una sola vez al arrancar).

Quality gate (igual que devapp-hello): `readelf` debe dar `Type: DYN` y
`0` DT_NEEDED — `arca.sh build` lo verifica y se niega a seguir si no.

## Regenerar los assets

```bash
python3 tools/gen_logo.py   # assets/logo.rgba (96×96 RGBA + cabecera)
python3 tools/gen_font.py   # crates/L2-arca-sdk-ui/src/font_data.rs
```

(ambos usan PIL + DejaVu del sistema; el generador de fuente FALLA si algún
glifo queda vacío o no cabe — una letra invisible es un bug, no un aviso)
