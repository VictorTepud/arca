# Arca — código fuente

Contenedor de sub-apps Rust para Android. Blueprint: `../arca-blueprint/`.

## Estado (F0+F1+F2 implementadas, F3a en el teléfono)

| Fase | Estado | Contenido |
|---|---|---|
| F0 (T01) | ✔ | Workspace 33 crates, justfile, check-graphs, toolchain fijado |
| F0 (T02) | ✔ **GO en hardware** | `host-probe/` (APK targetSdk 28) + `devapp-hello`: exec OK, heartbeats, exit 0 |
| F1 (T03-T09) | ✔ | types, pkg-model, 7z, sign, store, installer, tools-pk — pipeline `.arca` completo |
| F2 (T10-T16) | ✔ | **protocol, shm, ipc, exec-abi, permissions, exec-native, rt** — ejecución nativa headless con sandbox seccomp REAL |
| **F3a (T18)** | ⏳ **pantalla viva** | `devapp-demo` + `DemoActivity`: botones/imagen/animación/touch vía framebuffer seqlock compartido |

- **358 tests verdes** · `clippy -D warnings` verde · grafo sin ciclos (`scripts/check-graphs.py`)
- E2E del backend nativo (PC): spawn → handshake → **ping p99 = 22 µs** (presupuesto 1 ms) → kill-9 → Dead ≤ 50 ms; 100 spawns sin zombis; **seccomp probado** (socket() → SIGSYS); panic de app → exit 101.
- **r4 (fix e2e flaky, worklog T17)**: las dos e2e que fallaban en Deepin
  (`e2e_panic_de_la_app_exit_101`, `e2e_spawn_handshake_ping_kill9_dead`)
  tenían causa raíz: env del hijo contaminado entre tests paralelos +
  latencia de detección de muerte por polling. Arreglado de raíz (LaunchSpec
  v2 hermética + waitpid bloqueante). Verificado 6/6 ×5 corridas, incluso
  con la CPU al 100%.
- **F3a (worklog T18, `graphs/gfx-f3a.mmd`)**: primer render en pantalla.
  `arca-gfx-protocol` define el `FrameHeader` (32 B, golden byte a byte);
  `arca-shm` gana `FrameFile` (región de frames por archivo compartido
  — sin pasar memfds todavía); `arca-sdk-ui` trae canvas software, fuente
  bitmap 12×16, parser de input y botones (100 % safe, sin alloc en el
  path de frame); `devapp-demo` es la app demo interactiva y
  `host-probe/DemoActivity` el display server de juguete (mmap MAP_SHARED
  + seqlock + blit a SurfaceView + touch→stdin). Decisión clave: blake3
  `pure` en targets musl para conservar el cross SIN NDK.

## Uso rápido (PC/Deepin) — TODO EN UNO

```bash
./arca.sh todo      # deps + tests + selftest + APK + install + demo + logs
./arca.sh run hello # el probe F0 original (heartbeat)
./arca.sh logs      # guarda logs/arca-logs-*.txt (el archivo para reportar)
```

(sin NDK: el cross a arm64 usa `rust-lld`, ver `.cargo/config.toml`)

## Uso manual (equivalente)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build                                        # compila TODO (incluido arca-launch)
cargo test --workspace                             # 358 tests (≈40 s: incluye forks reales)
target/debug/devapp-demo --selftest                # pipeline visual del demo en PC

# E2E del backend nativo (requiere la app de prueba ESTÁTICA — el sandbox
# bloquea openat y una app dinámica moriría en el loader):
rustup target add x86_64-unknown-linux-musl
cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl
cargo test -p arca-exec-native --test e2e -- --nocapture   # logs del hijo visibles
```

Pipeline de empaquetado (F1):

```bash
cargo run -p arca-tools-pk -- keygen --out ~/.arca/keys
cargo run -p arca-tools-pk -- pack --src miapp/ --out miapp-1.0.0.arca --key ~/.arca/keys/signing.key --backend native
cargo run -p arca-tools-pk -- verify --file miapp-1.0.0.arca --pubkey ~/.arca/keys/signing.pub
```

## Siguiente paso CRÍTICO (gate F0 → F3a)

F0 = **GO confirmado** (devapp-hello: exit 0, heartbeats 1–60; formalizar
la fila en `host-probe/decision.md`). Ahora: correr el **demo F3a** en el
teléfono (`./arca.sh todo` o `./arca.sh run demo`) y reportar
`logs/arca-logs-*.txt`. Tras eso, F3b: host-core real, AIPC con memfds,
MeshFrame rkyv, wm/input (ver worklog T18). Si el exec fallara → pivot
WASM-first (T25).

## CI local

`just default` (o `scripts/ci.sh`): fmt-check + clippy -D warnings + test + check-graphs.
