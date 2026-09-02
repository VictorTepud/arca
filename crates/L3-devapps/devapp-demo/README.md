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
| Botones Color / Ping | hit-test táctil, estados pressed, acciones (r10 quitó "Salir": el cierre es la X de la esquina) |
| Botón X (esquina sup. der.) | cierre limpio desde DENTRO de la sub-app: `exit(0)` + `exiting reason=x` |
| Pelota | física simple; **persigue tu dedo** cuando arrastras |
| Barra inferior | telemetría: fps, frames, coords del toque |

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
`.cargo/config.toml`), le agrega el footer ARCAAPP1 (nombre+icono, ver
abajo) y `./arca.sh run demo` lo sube por `run-as` y abre `DemoActivity`
(vertical fija: la geometría del framebuffer se acuerda una sola vez al
arrancar). Desde r11 NO viaja como asset del APK: se instala desde el
lanzador (botón +) o lo subes con `./arca.sh run demo`.

Quality gate (igual que devapp-hello): `readelf` debe dar `Type: DYN` y
`0` DT_NEEDED — `arca.sh build` lo verifica y se niega a seguir si no.

## Nombre e icono desde la compilación (r11)

El lanzador (MainActivity) muestra las apps instaladas en un grid con
**icono y nombre**. Esos metadatos viajan DENTRO del binario en un footer
al final del archivo (el loader de ELF ignora los bytes tras el último
segmento, así que sigue siendo ejecutable tal cual):

```text
[nombre UTF-8][u16 len][icono PNG][u32 len][b"ARCAAPP1"]   ← final del archivo
```

`./arca.sh build` lo hace solo para las devapps de este repo:

```bash
python3 scripts/empaqueta_app.py target/aarch64-unknown-linux-musl/release/devapp-demo \
    --name "Demo Arca" --icono crates/L3-devapps/devapp-demo/assets/icono.png
```

Para TU propia sub-app es lo mismo: compila tu binario estático-PIE y
pásale `--name` (obligatorio) e `--icono` (opcional: PNG de 192×192 va
de sobra; sin icono el lanzador dibuja un avatar con la inicial). El
empaquetador es idempotente: re-empaquetar tras un rebuild no acumula
footers. El icono de la demo se regenera con `tools/gen_icono.py`.

## Regenerar los assets

```bash
python3 tools/gen_logo.py    # assets/logo.rgba (96×96 RGBA + cabecera)
python3 tools/gen_font.py    # crates/L2-arca-sdk-ui/src/font_data.rs
python3 tools/gen_icono.py   # assets/icono.png (192×192, PNG puro sin PIL)
```

(logo y fuente usan PIL + DejaVu del sistema; el generador de fuente
FALLA si algún glifo queda vacío o no cabe — una letra invisible es un
bug, no un aviso)
