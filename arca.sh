#!/usr/bin/env bash
# ============================================================================
#  arca.sh — Arca F0-F3a (r5): TODO en un solo comando
#
#  Basado en el proyecto completo (26 crates) con el fix de las e2e flaky (r4)
#  + el probe visual F3a (r5): devapp-demo pinta botones/imagen/animación
#  en pantalla y responde al dedo (framebuffer compartido + stdio).
#
#  Uso:
#    ./arca.sh todo            # deps + test + build + install + run demo + logs
#    ./arca.sh deps            # Rust + targets musl + JDK + SDK + Gradle (1.ª vez)
#    ./arca.sh test            # 6 e2e del motor + selftest del demo EN TU PC
#    ./arca.sh build           # devapp-hello + devapp-demo (arm64, SIN NDK) + APK
#    ./arca.sh install         # instala el APK (adb)
#    ./arca.sh run [hello|demo]  # lanza el probe F0 o el demo F3a (default: demo)
#    ./arca.sh logs            # guarda logs/arca-logs-*.txt (el archivo a enviar)
#    ./arca.sh limpiar         # borra compilados (conserva dependencias)
#
#  Opción global: ./arca.sh --skip-deps todo
# ============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
TOOLS="$REPO_ROOT/.arca-tools"
SDK="$TOOLS/android-sdk"
GRADLE_HOME="$TOOLS/gradle-8.9"
LOGS_DIR="$REPO_ROOT/logs"
HOST_PROBE_DIR="$REPO_ROOT/host-probe"
ASSET_HELLO="$HOST_PROBE_DIR/app/src/main/assets/devapp-hello"
ASSET_DEMO="$HOST_PROBE_DIR/app/src/main/assets/devapp-demo"
APK="$HOST_PROBE_DIR/app/build/outputs/apk/debug/app-debug.apk"
PAQUETE="dev.arca.probe"
BIN_ARM64="$REPO_ROOT/target/aarch64-unknown-linux-musl/release/devapp-hello"
BIN_DEMO_ARM64="$REPO_ROOT/target/aarch64-unknown-linux-musl/release/devapp-demo"

C_R='\033[0;31m'; C_G='\033[0;32m'; C_Y='\033[0;33m'; C_B='\033[0;34m'; C_0='\033[0m'
info()  { printf "${C_B}[arca ]${C_0} %s\n" "$*"; }
ok()    { printf "${C_G}[OK   ]${C_0} %s\n" "$*"; }
warn()  { printf "${C_Y}[aviso]${C_0} %s\n" "$*"; }
error() { printf "${C_R}[ERROR]${C_0} %s\n" "$*" >&2; exit 1; }

export PATH="$HOME/.cargo/bin:$PATH"

ADB="$SDK/platform-tools/adb"
if [ ! -x "$ADB" ]; then
    ADB="$(command -v adb 2>/dev/null || true)"
fi

SKIP_DEPS=0
if [ "${1:-}" = "--skip-deps" ]; then
    SKIP_DEPS=1
    shift
fi
CMD="${1:-help}"

# ─────────────────────────── utilidades ───────────────────────────

# ¿el java del sistema sirve? (versión >= $1 y con javac)
java_lista() {
    command -v java >/dev/null 2>&1 || return 1
    command -v javac >/dev/null 2>&1 || return 1
    local v
    v="$(java -version 2>&1 | head -n1 | sed -E 's/.*version "([0-9]+).*/\1/' || echo 0)"
    [ "${v:-0}" -ge "$1" ] 2>/dev/null
}

resolver_java() {
    if java_lista 17; then
        export JAVA_HOME="$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")"
    elif [ -x "$TOOLS/jdk/bin/java" ]; then
        export JAVA_HOME="$TOOLS/jdk"
        export PATH="$JAVA_HOME/bin:$PATH"
    else
        error "no hay un JDK 17 (con javac); corre primero: ./arca.sh deps"
    fi
}

instalar_jdk() {
    info "hace falta un JDK 17 o mayor..."
    if command -v sudo >/dev/null 2>&1; then
        info "probando con apt (openjdk-17-jdk)..."
        if sudo apt-get update -qq && sudo apt-get install -y openjdk-17-jdk; then
            return 0
        fi
        warn "apt no pudo; bajare un JDK portatil a .arca-tools/"
    fi
    local tgz="$TOOLS/jdk17.tar.gz"
    mkdir -p "$TOOLS"
    if [ ! -f "$tgz" ] || ! tar -tzf "$tgz" >/dev/null 2>&1; then
        info "descargando JDK 17 (Temurin)..."
        curl -fL "https://api.adoptium.net/v3/binary/latest/17/ga/linux/x64/jdk/hotspot/normal/eclipse" \
            -o "$tgz" || error "no pude descargar el JDK"
    fi
    local carpeta
    carpeta="$(tar -tzf "$tgz" | head -n1 | cut -d/ -f1)"
    rm -rf "$TOOLS/jdk" "$TOOLS/$carpeta"
    tar -xzf "$tgz" -C "$TOOLS"
    mv "$TOOLS/$carpeta" "$TOOLS/jdk"
}

instalar_sdk() {
    info "instalando Android SDK (solo las piezas necesarias)..."
    mkdir -p "$SDK" "$TOOLS"
    local zip="$TOOLS/cmdline-tools.zip"
    if [ ! -f "$zip" ] || ! unzip -tq "$zip" >/dev/null 2>&1; then
        info "descargando cmdline-tools..."
        curl -fL "https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip" \
            -o "$zip" || error "no pude descargar cmdline-tools"
    fi
    rm -rf "$TOOLS/ct-tmp"
    mkdir -p "$TOOLS/ct-tmp"
    unzip -q "$zip" -d "$TOOLS/ct-tmp"
    mkdir -p "$SDK/cmdline-tools"
    rm -rf "$SDK/cmdline-tools/latest"
    mv "$TOOLS/ct-tmp/cmdline-tools" "$SDK/cmdline-tools/latest"
    rm -rf "$TOOLS/ct-tmp"

    local sm="$SDK/cmdline-tools/latest/bin/sdkmanager"
    info "aceptando licencias..."
    set +o pipefail
    yes | "$sm" --sdk_root="$SDK" --licenses >/dev/null 2>&1 || true
    info "descargando platform-tools, android-34 y build-tools (un par de minutos)..."
    yes | "$sm" --sdk_root="$SDK" "platform-tools" "platforms;android-34" \
        "build-tools;34.0.0" >"$TOOLS/sdk-install.log" 2>&1
    local rc=$?
    set -o pipefail
    if [ "$rc" -ne 0 ]; then
        tail -n 20 "$TOOLS/sdk-install.log" || true
        error "no pude instalar los paquetes del SDK"
    fi
}

instalar_gradle() {
    info "descargando Gradle 8.9..."
    mkdir -p "$TOOLS"
    local zip="$TOOLS/gradle-8.9-bin.zip"
    if [ ! -f "$zip" ] || ! unzip -tq "$zip" >/dev/null 2>&1; then
        curl -fL "https://services.gradle.org/distributions/gradle-8.9-bin.zip" -o "$zip" \
            || error "no pude descargar Gradle"
    fi
    rm -rf "$GRADLE_HOME"
    unzip -q "$zip" -d "$TOOLS"
}

adb_requerido() {
    [ -n "$ADB" ] && [ -x "$ADB" ] || error "no encuentro adb; corre: ./arca.sh deps"
    local est
    est="$("$ADB" get-state 2>/dev/null || true)"
    case "$est" in
        device) ;;
        unauthorized)
            error "el teléfono no autorizó la depuración USB: desbloquéalo y acepta el cuadro"
            ;;
        *)
            error "no hay teléfono conectado. Activa Depuración USB (Ajustes > Acerca del teléfono > toca 7 veces 'Número de compilación' > Ajustes > Opciones de desarrollador > Depuración USB), conéctalo y acepta el cuadro de confianza."
            ;;
    esac
}

# ─────────────────────────── comandos ───────────────────────────

cmd_deps() {
    local faltan=() c
    for c in curl unzip tar; do
        command -v "$c" >/dev/null 2>&1 || faltan+=("$c")
    done
    if [ "${#faltan[@]}" -gt 0 ]; then
        info "instalando: ${faltan[*]}"
        sudo apt-get install -y "${faltan[@]}" || error "instala a mano: ${faltan[*]}"
    fi

    if ! command -v cargo >/dev/null 2>&1; then
        info "instalando Rust con rustup..."
        curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal \
            || error "no pude instalar Rust"
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    ok "Rust: $(cargo --version)"

    info "targets musl (x86_64 PC, aarch64/armv7 Android)..."
    rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
        armv7-unknown-linux-musleabihf \
        || error "no pude añadir los targets musl (revisa tu conexión)"

    if ! java_lista 17; then
        instalar_jdk
    fi
    resolver_java
    ok "Java: $(java -version 2>&1 | head -n1)"

    if [ ! -x "$SDK/platform-tools/adb" ]; then
        instalar_sdk
    fi
    ok "SDK: $SDK"

    if [ ! -x "$GRADLE_HOME/bin/gradle" ]; then
        instalar_gradle
    fi
    ok "Gradle: $GRADLE_HOME"
    info "(nota: el NDK NO hace falta: cross-compilamos con rust-lld, ver .cargo/config.toml)"
}

cmd_test() {
    command -v cargo >/dev/null 2>&1 || error "no hay cargo; corre: ./arca.sh deps"
    cd "$REPO_ROOT"
    info "compilando arca-ping estático (musl) para el e2e..."
    cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl
    info "corriendo las 6 pruebas e2e del motor nativo (r4, fix flaky aplicado)..."
    if cargo test -p arca-exec-native --test e2e -- --nocapture; then
        ok "motor nativo verificado en tu PC (6/6)"
    else
        error "fallaron tests del motor; cópiame la salida de arriba"
    fi
    info "selftest del demo (render→publish→lectura, sin teléfono)..."
    cargo build -p devapp-demo
    if "$REPO_ROOT/target/debug/devapp-demo" --selftest; then
        ok "probe visual F3a verificado en tu PC"
    else
        error "falló el selftest del demo; cópiame la salida"
    fi
}

cmd_build() {
    command -v cargo >/dev/null 2>&1 || error "no hay cargo; corre: ./arca.sh deps"
    command -v java >/dev/null 2>&1 || error "no hay Java; corre: ./arca.sh deps"
    resolver_java
    [ -x "$GRADLE_HOME/bin/gradle" ] || error "no hay Gradle; corre: ./arca.sh deps"
    [ -d "$SDK/platform-tools" ] || error "no hay Android SDK; corre: ./arca.sh deps"

    cd "$REPO_ROOT"
    info "compilando devapp-hello y devapp-demo para arm64 (estático-PIE, SIN NDK)..."
    cargo build -p devapp-hello --target aarch64-unknown-linux-musl --release \
        || error "falló el cross a aarch64 musl"
    cargo build -p devapp-demo --target aarch64-unknown-linux-musl --release \
        || error "falló el cross del demo a aarch64 musl"

    # gate de calidad: ELF estático-PIE verificado A NIVEL DE BYTES con
    # python3 (scripts/verifica_elf.py). El parseo textual de readelf+awk
    # fallaba en algunas Deepin aunque el binario estuviera bien (Type=
    # vacío) y abortaba el build; leer el ELF directo elimina toda
    # dependencia de versión/locale/binutils del entorno.
    command -v python3 >/dev/null 2>&1 \
        || error "el gate necesita python3: sudo apt install python3"
    for bin in "$BIN_ARM64" "$BIN_DEMO_ARM64"; do
        [ -f "$bin" ] || error "no existe $bin (¿falló la compilación?)"
        python3 "$REPO_ROOT/scripts/verifica_elf.py" "$bin" \
            || error "$(basename "$bin") no pasó el gate estático-PIE (detalle arriba)"
    done
    ok "devapp-hello y devapp-demo: ELF estático-PIE verificados (bytes)"

    info "copiando los binarios a los assets del APK..."
    cp "$BIN_ARM64" "$ASSET_HELLO"
    cp "$BIN_DEMO_ARM64" "$ASSET_DEMO"

    info "construyendo el APK (la primera vez descarga dependencias de Gradle)..."
    export ANDROID_HOME="$SDK"
    export ANDROID_SDK_ROOT="$SDK"
    echo "sdk.dir=$SDK" > "$HOST_PROBE_DIR/local.properties"
    ( cd "$HOST_PROBE_DIR" && exec "$GRADLE_HOME/bin/gradle" --console=plain assembleDebug ) \
        || error "falló el build del APK"
    [ -f "$APK" ] || error "no encontré el APK en $APK"
    ok "APK listo: $APK"
}

cmd_install() {
    adb_requerido
    [ -f "$APK" ] || error "no hay APK; corre primero: ./arca.sh build"
    info "instalando el APK en el teléfono..."
    if ! "$ADB" install -r "$APK"; then
        warn "falló; probablemente hay un APK viejo con otra firma. Lo quito y reintento..."
        "$ADB" uninstall "$PAQUETE" >/dev/null 2>&1 || true
        "$ADB" install "$APK" || error "no se pudo instalar el APK"
    fi
    ok "APK instalado ($PAQUETE)"
}

cmd_run() {
    adb_requerido
    local modo="${1:-demo}"
    if [ "$modo" = "demo" ]; then
        "$ADB" shell am start -n "$PAQUETE/.DemoActivity" >/dev/null \
            || error "no pude lanzar el demo (¿corriste ./arca.sh build?)"
        ok "demo F3a lanzado: toca la pantalla del teléfono (botones y pelota)"
    else
        "$ADB" shell am start -n "$PAQUETE/.MainActivity" >/dev/null \
            || error "no pude lanzar la app"
        ok "sonda F0 lanzada: mira el teléfono y pulsa el botón de ejecutar"
    fi
}

cmd_logs() {
    adb_requerido
    mkdir -p "$LOGS_DIR"
    local stamp out
    stamp="$(date +%Y%m%d-%H%M%S)"
    out="$LOGS_DIR/arca-logs-$stamp.txt"
    {
        echo "=== Arca · registro del probe F0 + demo F3a (r5) ==="
        echo "generado: $(date)"
        echo
        echo "-- teléfono --"
        "$ADB" shell getprop ro.product.model 2>/dev/null || true
        "$ADB" shell getprop ro.build.version.release 2>/dev/null || true
        "$ADB" shell getprop ro.build.version.sdk 2>/dev/null || true
        echo
        echo "-- paquete (fijate en targetSdk=28) --"
        "$ADB" shell dumpsys package "$PAQUETE" 2>/dev/null \
            | grep -E 'targetSdk|versionName' | head -n 4 || true
        echo
        echo "-- logcat: probe + errores --"
        "$ADB" logcat -d -s ArcaProbe:V AndroidRuntime:E libc:F DEBUG:F 2>/dev/null || true
        echo
        echo "-- decisión (patrón GO: hello + heartbeats + pong + exit 0) --"
        echo "(rellena host-probe/decision.md con este registro)"
    } > "$out" 2>&1
    ok "logs guardados en: $out"
    info "envíame ese archivo y lo reviso"
}

cmd_todo() {
    if [ "$SKIP_DEPS" -eq 0 ]; then
        info "revisando dependencias (usa --skip-deps para saltar)..."
        cmd_deps
    fi
    cmd_test
    cmd_build
    cmd_install
    cmd_run demo
    info "esperando 45 s a que juegues con el demo en el teléfono..."
    local i
    for i in $(seq 45 -1 1); do
        printf "\r   quedan %2d s " "$i"
        sleep 1
    done
    echo
    cmd_logs
    ok "LISTO. El registro quedó en logs/ — envíamelo."
}

cmd_limpiar() {
    cd "$REPO_ROOT"
    info "borrando compilados (las dependencias en .arca-tools se conservan)..."
    cargo clean 2>/dev/null || true
    rm -rf "$HOST_PROBE_DIR/app/build" "$HOST_PROBE_DIR/.gradle" "$HOST_PROBE_DIR/build" || true
    ok "limpio"
}

uso() {
    cat <<'EOF'
arca.sh — Arca F0-F3a (r5): construye, instala y corre el probe en tu Android

  ./arca.sh todo             flujo completo: deps + test + build + install + run demo + logs
  ./arca.sh deps             instalar dependencias (Rust, JDK, Android SDK, Gradle)
  ./arca.sh test             6 e2e del motor + selftest del demo en tu PC
  ./arca.sh build            compilar devapp-hello + devapp-demo (arm64, sin NDK) + APK
  ./arca.sh install          instalar el APK en el teléfono (adb)
  ./arca.sh run [hello|demo] lanzar el probe F0 (hello) o el demo F3a (demo, default)
  ./arca.sh logs             guardar los logs en logs/arca-logs-*.txt (para enviar)
  ./arca.sh limpiar          borrar compilados

  opción global: --skip-deps   (ej: ./arca.sh --skip-deps todo)
EOF
}

# ─────────────────────────── despacho ───────────────────────────

case "$CMD" in
    deps)          cmd_deps ;;
    test)          cmd_test ;;
    build)         cmd_build ;;
    install)       cmd_install ;;
    run)           cmd_run "${2:-demo}" ;;
    logs)          cmd_logs ;;
    todo|all)      cmd_todo ;;
    limpiar|clean) cmd_limpiar ;;
    help|-h|--help|"") uso ;;
    *)             error "comando desconocido: '$CMD' (ver: ./arca.sh help)" ;;
esac
