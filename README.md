# Arca — código fuente

Contenedor de sub-apps Rust para Android. Blueprint: `../arca-blueprint/`.

## Estado (F0+F1+F2 implementadas, PC-verificables)

| Fase | Estado | Contenido |
|---|---|---|
| F0 (T01) | ✔ | Workspace 33 crates, justfile, check-graphs, toolchain fijado |
| F0 (T02) | ⏳ código listo | `host-probe/` (APK targetSdk 28) + `devapp-hello` — **ejecutar en tu teléfono = gate GO/NO-GO** |
| F1 (T03-T09) | ✔ | types, pkg-model, 7z, sign, store, installer, tools-pk — pipeline `.arca` completo |
| F2 (T10-T16) | ✔ | **protocol, shm, ipc, exec-abi, permissions, exec-native, rt** — ejecución nativa headless con sandbox seccomp REAL |

- **315 tests verdes** · `clippy -D warnings` verde · grafo sin ciclos (`scripts/check-graphs.py`)
- E2E del backend nativo (PC): spawn → handshake → **ping p99 = 22 µs** (presupuesto 1 ms) → kill-9 → Dead ≤ 50 ms; 100 spawns sin zombis; **seccomp probado** (socket() → SIGSYS); panic de app → exit 101.
- **r4 (fix e2e flaky, worklog T17)**: las dos e2e que fallaban en Deepin
  (`e2e_panic_de_la_app_exit_101`, `e2e_spawn_handshake_ping_kill9_dead`)
  tenían causa raíz: env del hijo contaminado entre tests paralelos +
  latencia de detección de muerte por polling. Arreglado de raíz (LaunchSpec
  v2 hermética + waitpid bloqueante). Verificado 6/6 ×5 corridas, incluso
  con la CPU al 100%.

## Uso rápido (PC/Deepin) — TODO EN UNO

```bash
./arca.sh todo      # deps + 6 e2e en PC + APK + install + run + logs
./arca.sh logs      # guarda logs/arca-logs-*.txt (el archivo para reportar)
```

(sin NDK: el cross a arm64 usa `rust-lld`, ver `.cargo/config.toml`)

## Uso manual (equivalente)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build                                        # compila TODO (incluido arca-launch)
cargo test --workspace                             # 315 tests (≈35 s: incluye forks reales)

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

## Siguiente paso CRÍTICO (gate F0)

El usuario YA ejecutó devapp-hello en su teléfono (exit 0, heartbeats
1–60) — **gate F0 = GO confirmado en hardware real**. Queda formalizarlo
en `host-probe/decision.md` y seguir con **F3** (gfx-protocol/input/wm/
compositor/sdk + host-core). Si FAIL → pivot WASM-first (T25).

## CI local

`just default` (o `scripts/ci.sh`): fmt-check + clippy -D warnings + test + check-graphs.
