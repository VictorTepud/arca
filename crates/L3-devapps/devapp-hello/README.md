# devapp-hello

Binario de **probe de viabilidad F0** (gate GO/NO-GO de Arca, tarea T02).
Demuestra la *grieta de Termux*: un APK con `targetSdk 28` (dominio SELinux
`untrusted_app_27`) extrae este ELF a `/data/data/<pkg>/files` y lo ejecuta
con `fork`+`exec`. Ver `docs/01-restricciones-android.md` §2 y
`docs/12-roadmap-fases.md` §F0 del blueprint.

- Capa: L3 (`crates/L3-devapps/devapp-hello/`)
- Es un **binario** (`[[bin]] devapp-hello`), sin dependencias Arca — es
  deliberadamente autocontenido (probe previo a toda la infraestructura).
- Deps: `libc` **local** (versión explícita; ver nota en `Cargo.toml`).
- Tarea: T02 · Estado: **implementado** (verificado en Linux PC)

## Protocolo de stdout (una línea JSON compacta por evento)

| Línea | Cuándo | Significado |
|---|---|---|
| `{"event":"hello","ts":…,"pid":…,"uid":…,"gid":…,"cwd":"…","argv0":"…"}` | arranque | identidad del proceso en el dispositivo (¿qué ve el hijo tras exec?) |
| `{"ts":…,"pid":…,"seq":N}` | cada 500 ms | heartbeat: el proceso vive y su stdout llega al host |
| `{"event":"pong","seq":N}` | stdin recibe línea `ping` | eco AIPC mínimo (el pipe de control funciona en ambos sentidos) |
| `{"event":"sigterm","seq":N}` | SIGTERM/SIGINT | línea final antes de `_exit(0)` (debe aparecer en ≤ 100 ms) |
| `{"event":"fatal","error":"…"}` | (stderr) | error irrecuperable; exit code 1 |

`ts` = ms de `CLOCK_MONOTONIC` (desde el arranque del sistema). `seq` =
número de heartbeat. Detalles de ingeniería (async-signal-safety del handler,
tolerancia a EOF, atomicidad de líneas) en los comentarios de `src/main.rs`.

## Probarlo en PC (Linux)

```bash
cargo build -p devapp-hello
B=./target/debug/devapp-hello

# 1) heartbeats + SIGTERM a los 3 s (GNU timeout manda SIGTERM; el binario
#    responde la línea final y muere limpio; timeout reporta 124):
timeout 3 $B

# 2) ping/pong + EOF tolerado (tras el EOF sigue latiendo hasta el SIGTERM):
printf 'ping\n' | timeout 3 $B

# 3) exit code real (0) tras SIGTERM manual:
$B & PID=$!; sleep 1; kill -TERM $PID; wait $PID; echo "exit=$?"   # → 0
```

Salida real observada (PC, Debian/Deepin x86_64):

```json
{"event":"hello","ts":7015959,"pid":9420,"uid":1001,"gid":1001,"cwd":"/home/z/my-project/arca","argv0":"./target/debug/devapp-hello"}
{"ts":7015960,"pid":9420,"seq":1}
{"event":"pong","seq":1}
{"ts":7016460,"pid":9420,"seq":2}
{"ts":7016960,"pid":9420,"seq":3}
{"ts":7017465,"pid":9420,"seq":4}
{"event":"sigterm","seq":4}
```

Tests del protocolo: `cargo test -p devapp-hello` (formato JSON, escape,
partidor de líneas, formateo del handler).

## Compilación para Android (aarch64, estático-PIE)

Se hace desde **Deepin** con `cargo-ndk` + NDK (ver `docs/09-build-deepin.md`
del blueprint). Prerrequisitos (una vez):

```bash
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=~/Android/Sdk/ndk/<versión>   # NDK r26+
```

Comando exacto (PIE estático — ADR-008 del blueprint):

```bash
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static-pie" \
  cargo ndk -t arm64-v8a -p 26 -o /tmp/arca-probe-jniLibs \
  build --release -p devapp-hello
```

- `-t arm64-v8a`: triple de Android de 64 bits.
- `-p 26`: nivel de API mínimo (= `minSdk` del host-probe).
- `-o` solo copia artefactos tipo librería; el binario queda en
  `target/aarch64-linux-android/release/devapp-hello`.
- `+crt-static`: enlaza contra la `libc.a` de bionic del NDK → **cero
  dependencias dinámicas** (sin `DT_NEEDED`), sin problemas de
  linker-namespaces ni de rutas de `LD_LIBRARY_PATH` en el hijo. La
  alternativa dinámica (`default-`, enlazando `libc.so` de bionic) también
  funciona en Android, pero exige que el runtime del hijo resuelva bionic —
  innecesario para sub-apps autocontenidas; se descarta (decisión ADR-008).
- `-static-pie`: ELF estático **y** con PIC (Android 10+ exige PIE para
  ejecutar; ojo: Android 5+ ya lo exige en binarios dinámicos, y el loader de
  bionic moderno también lo pide para estáticos).

### Verificación de estático-PIE (quality gate F0)

```bash
readelf -d target/aarch64-linux-android/release/devapp-hello
# → no debe haber NINGUNA entrada "NEEDED" (Dynamic section … vacía o sin
#   DT_NEEDED). Si aparece libc.so o libdl.so → NO es estático.

readelf -h target/aarch64-linux-android/release/devapp-hello | grep Type
# → "DYN" (PIE). "EXEC" sería no-PIE (Android moderno lo rechaza).

file target/aarch64-linux-android/release/devapp-hello
# → "ELF 64-bit LSB pie executable, ARM aarch64, statically linked"
```

### Entregarlo al APK del probe

```bash
cp target/aarch64-linux-android/release/devapp-hello \
   host-probe/app/src/main/assets/devapp-hello      # nombre EXACTO
```

Luego compilar/instalar el APK según `host-probe/README.md`.

## Nota sobre libc (desviación documentada)

La tarea pedía `libc = "0.3"`, pero **libc 0.3 no existe en crates.io** (la
serie estable es `0.2.x`; `1.0` está en alpha). Se fija `libc = "0.2.189"`
como dependencia local explícita del crate (sin tocar el `[workspace]`),
misma versión que hay en la cache local de cargo.
