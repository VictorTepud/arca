#!/usr/bin/env bash
# ============================================================================
#  arca.sh — Arca F0-F2 (r3)
#  Construye todo (sub-app Rust + APK Android) e instala la sonda en tu
#  telefono. Tambien captura los logs para que puedas enviarlos.
#
#  Uso:
#    ./arca.sh todo      # flujo completo: deps + test + build + install
#                        #   + run + esperar + guardar logs
#    ./arca.sh deps      # solo instalar dependencias (Rust, JDK, SDK, Gradle)
#    ./arca.sh test      # solo los 6 tests del motor en tu PC
#    ./arca.sh build     # solo compilar binarios (3 arq.) + APK
#    ./arca.sh install   # solo instalar el APK en el telefono
#    ./arca.sh run       # solo lanzar la sonda en el telefono
#    ./arca.sh logs      # solo capturar logs a logs/arca-logs-*.txt
#    ./arca.sh limpiar   # borrar compilados (conserva dependencias)
#
#  Opcion global: ./arca.sh --skip-deps todo
# ============================================================================
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
TOOLS="$REPO_ROOT/.arca-tools"
SDK="$TOOLS/android-sdk"
GRADLE_HOME="$TOOLS/gradle-8.7"
LOGS_DIR="$REPO_ROOT/logs"
HOST_PROBE_DIR="$REPO_ROOT/host-probe"
ASSETS_DIR="$HOST_PROBE_DIR/app/src/main/assets/arca-bin"
APK="$HOST_PROBE_DIR/app/build/outputs/apk/debug/app-debug.apk"
PAQUETE="dev.arca.probe"

C_R='\033[0;31m'; C_G='\033[0;32m'; C_Y='\033[0;33m'; C_B='\033[0;34m'; C_0='\033[0m'
info()  { printf "${C_B}[arca ]${C_0} %s\n" "$*"; }
ok()    { printf "${C_G}[OK   ]${C_0} %s\n" "$*"; }
warn()  { printf "${C_Y}[aviso]${C_0} %s\n" "$*"; }
error() { printf "${C_R}[ERROR]${C_0} %s\n" "$*" >&2; exit 1; }

export PATH="$HOME/.cargo/bin:$PATH"

# que adb usar: el del SDK que instala este script, o el del sistema
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

# ¿el java del sistema sirve? (version >= $1 y con javac)
java_lista() {
    command -v java >/dev/null 2>&1 || return 1
    command -v javac >/dev/null 2>&1 || return 1
    local v
    v="$(java -version 2>&1 | head -n1 | sed -E 's/.*version "([0-9]+).*/\1/' || echo 0)"
    [ "${v:-0}" -ge "$1" ] 2>/dev/null
}

# decide y exporta JAVA_HOME (sistema o el JDK portatil de .arca-tools)
resolver_java() {
    if java_lista 17; then
        export JAVA_HOME="$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")"
    elif [ -x "$TOOLS/jdk/bin/java" ]; then
        export JAVA_HOME="$TOOLS/jdk"
        export PATH="$JAVA_HOME/bin:$PATH"
    else
        error "no hay un JDK 17 (con javac) disponible; corre: ./arca.sh deps"
    fi
}

instalar_jdk() {
    info "hace falta un JDK 17 o mayor..."
    if command -v sudo >/dev/null 2>&1; then
        info "probando con apt (openjdk-17-jdk)..."
        if sudo apt-get update -qq && sudo apt-get install -y openjdk-17-jdk; then
            return 0
        fi
        warn "apt no pudo; bajare un JDK portatil"
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
    info "descargando Gradle 8.7..."
    mkdir -p "$TOOLS"
    local zip="$TOOLS/gradle-8.7-bin.zip"
    if [ ! -f "$zip" ] || ! unzip -tq "$zip" >/dev/null 2>&1; then
        curl -fL "https://services.gradle.org/distributions/gradle-8.7-bin.zip" -o "$zip" \
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
            error "el telefono no autorizo la depuracion USB: desbloquealo y acepta el cuadro"
            ;;
        *)
            error "no hay telefono conectado. Activa Depuracion USB (Ajustes > Acerca del telefono > toca 7 veces 'Numero de compilacion' > Ajustes > Opciones de desarrollador > Depuracion USB), conectalo y acepta el cuadro de confianza. Si tienes varios conectados: ANDROID_SERIAL=<serial> ./arca.sh ..."
            ;;
    esac
}

# ─────────────────────────── comandos ───────────────────────────

cmd_deps() {
    # herramientas basicas del sistema
    local faltan=()
    local c
    for c in curl unzip tar; do
        command -v "$c" >/dev/null 2>&1 || faltan+=("$c")
    done
    if [ "${#faltan[@]}" -gt 0 ]; then
        info "instalando: ${faltan[*]}"
        sudo apt-get install -y "${faltan[@]}" || error "instala a mano: ${faltan[*]}"
    fi

    # Rust (si ya esta, no hace nada)
    if ! command -v cargo >/dev/null 2>&1; then
        info "instalando Rust con rustup..."
        curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal \
            || error "no pude instalar Rust"
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
    ok "Rust: $(cargo --version)"

    info "targets musl (x86_64, aarch64, armv7)..."
    rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf \
        || error "no pude anadir los targets musl (revisa tu conexion)"

    # JDK 17+
    if ! java_lista 17; then
        instalar_jdk
    fi
    resolver_java
    ok "Java: $(java -version 2>&1 | head -n1)"
    ok "JAVA_HOME=$JAVA_HOME"

    # Android SDK
    if [ ! -x "$SDK/platform-tools/adb" ]; then
        instalar_sdk
    fi
    ok "SDK: $SDK"

    # Gradle
    if [ ! -x "$GRADLE_HOME/bin/gradle" ]; then
        instalar_gradle
    fi
    ok "Gradle: $GRADLE_HOME"
}

cmd_test() {
    command -v cargo >/dev/null 2>&1 || error "no hay cargo; corre: ./arca.sh deps"
    cd "$REPO_ROOT"
    info "compilando la sub-app para tu PC (musl estatico)..."
    cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl
    info "corriendo las 6 pruebas del motor (revision r2)..."
    if cargo test -p arca-exec-native --test e2e; then
        ok "motor nativo verificado en tu PC (6/6)"
    else
        error "fallaron tests del motor; copiame la salida de arriba"
    fi
}

cmd_build() {
    command -v cargo >/dev/null 2>&1 || error "no hay cargo; corre: ./arca.sh deps"
    command -v java >/dev/null 2>&1 || error "no hay Java; corre: ./arca.sh deps"
    resolver_java
    [ -x "$GRADLE_HOME/bin/gradle" ] || error "no hay Gradle; corre: ./arca.sh deps"
    [ -d "$SDK/platform-tools" ] || error "no hay Android SDK; corre: ./arca.sh deps"

    cd "$REPO_ROOT"
    info "compilando arca-ping para 3 arquitecturas (estatico, sin NDK)..."
    local t
    for t in aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf x86_64-unknown-linux-musl; do
        cargo build -p arca-rt --bin arca-ping --release --target "$t" \
            || error "fallo compilar $t"
    done
    info "copiando binarios al APK (assets)..."
    mkdir -p "$ASSETS_DIR/aarch64" "$ASSETS_DIR/armv7" "$ASSETS_DIR/x86_64"
    cp "target/aarch64-unknown-linux-musl/release/arca-ping" "$ASSETS_DIR/aarch64/"
    cp "target/armv7-unknown-linux-musleabihf/release/arca-ping" "$ASSETS_DIR/armv7/"
    cp "target/x86_64-unknown-linux-musl/release/arca-ping" "$ASSETS_DIR/x86_64/"
    ok "3 binarios dentro del APK"

    info "construyendo el APK (la primera vez descarga dependencias de Gradle)..."
    export ANDROID_HOME="$SDK"
    export ANDROID_SDK_ROOT="$SDK"
    echo "sdk.dir=$SDK" > "$HOST_PROBE_DIR/local.properties"
    ( cd "$HOST_PROBE_DIR" && exec "$GRADLE_HOME/bin/gradle" --console=plain assembleDebug ) \
        || error "fallo el build del APK"
    [ -f "$APK" ] || error "no encontre el APK en $APK"
    ok "APK listo: $APK"
}

cmd_install() {
    adb_requerido
    [ -f "$APK" ] || error "no hay APK; corre primero: ./arca.sh build"
    info "instalando el APK en el telefono..."
    if ! "$ADB" install -r "$APK"; then
        warn "fallo; probablemente hay un APK viejo con otra firma. Lo quito y reintento..."
        "$ADB" uninstall "$PAQUETE" >/dev/null 2>&1 || true
        "$ADB" install "$APK" || error "no se pudo instalar el APK"
    fi
    ok "APK instalado ($PAQUETE)"
}

cmd_run() {
    adb_requerido
    "$ADB" shell am start -n "$PAQUETE/.MainActivity" -e auto 1 >/dev/null \
        || error "no pude lanzar la app"
    ok "sonda lanzada: mira la pantalla del telefono"
}

cmd_logs() {
    adb_requerido
    mkdir -p "$LOGS_DIR"
    local stamp out
    stamp="$(date +%Y%m%d-%H%M%S)"
    out="$LOGS_DIR/arca-logs-$stamp.txt"
    {
        echo "=== Arca · registro de la sonda F0 (r3) ==="
        echo "generado: $(date)"
        echo
        echo "-- telefono --"
        "$ADB" shell getprop ro.product.model 2>/dev/null || true
        "$ADB" shell getprop ro.build.version.release 2>/dev/null || true
        "$ADB" shell getprop ro.build.version.sdk 2>/dev/null || true
        echo
        echo "-- paquete (fijate en targetSdk=28) --"
        "$ADB" shell dumpsys package "$PAQUETE" 2>/dev/null \
            | grep -E 'targetSdk|versionName' | head -n 4 || true
        echo
        echo "-- logcat: sonda + errores --"
        "$ADB" logcat -d -s ArcaProbe:V AndroidRuntime:E libc:F DEBUG:F 2>/dev/null || true
        echo
        echo "-- archivo interno de la app --"
        "$ADB" shell run-as "$PAQUETE" cat files/arca-probe.log 2>/dev/null \
            || echo "(sin archivo interno: corrio la sonda?)"
    } > "$out" 2>&1
    ok "logs guardados en: $out"
    info "enviame ese archivo y lo reviso"
}

cmd_todo() {
    if [ "$SKIP_DEPS" -eq 0 ]; then
        info "revisando dependencias (usa --skip-deps para saltar)..."
        cmd_deps
    fi
    cmd_test
    cmd_build
    cmd_install
    cmd_run
    info "esperando 45 s a que las 6 pruebas terminen en el telefono..."
    local i
    for i in $(seq 45 -1 1); do
        printf "\r   quedan %2d s " "$i"
        sleep 1
    done
    echo
    cmd_logs
    ok "LISTO. La sonda corrio en tu telefono y el registro quedo en logs/"
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
arca.sh — Arca F0-F2 (r3): construye, instala y mide la sonda en tu Android

  ./arca.sh todo      flujo completo: deps + test + build + install + run + logs
  ./arca.sh deps      instalar dependencias (Rust, JDK, Android SDK, Gradle)
  ./arca.sh test      6 pruebas del motor nativo en tu PC
  ./arca.sh build     compilar sub-app (3 arquitecturas) + APK
  ./arca.sh install   instalar el APK en el telefono (adb)
  ./arca.sh run       lanzar la sonda en el telefono
  ./arca.sh logs      guardar los logs en logs/arca-logs-*.txt (para enviar)
  ./arca.sh limpiar   borrar compilados

  opcion global: --skip-deps   (ej: ./arca.sh --skip-deps todo)
EOF
}

# ─────────────────────────── despacho ───────────────────────────

case "$CMD" in
    deps)          cmd_deps ;;
    test)          cmd_test ;;
    build)         cmd_build ;;
    install)       cmd_install ;;
    run)           cmd_run ;;
    logs)          cmd_logs ;;
    todo|all)      cmd_todo ;;
    limpiar|clean) cmd_limpiar ;;
    help|-h|--help|"") uso ;;
    *)             error "comando desconocido: '$CMD' (ver: ./arca.sh help)" ;;
esac
