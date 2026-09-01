# host-probe — APK del gate GO/NO-GO (F0, tarea T02)

APK Kotlin **mínimo** que valida en un teléfono real la grieta de targetSdk 28
(la ruta de Termux, blueprint `docs/01-restricciones-android.md` §2):
extrae un binario Rust de sus assets a `filesDir` (chmod 700) y lo ejecuta
con `fork`+`exec`. Su stdout (JSON de `devapp-hello`) se ve en logcat y en
pantalla. El resultado se registra en `decision.md`.

Estructura:

```
host-probe/                 (excluido del workspace Cargo — no es Rust)
├── settings.gradle.kts     AGP 8.5.2 · Kotlin 1.9.24 · repos google/mavenCentral
├── build.gradle.kts        (raíz)
├── decision.md             plantilla GO/NO-GO (rellenar tras la prueba)
└── app/
    ├── build.gradle.kts    compileSdk 34 · minSdk 26 · targetSdk 28 (LA GRIETA)
    └── src/main/
        ├── AndroidManifest.xml
        ├── res/values/{strings,themes}.xml   (Theme.Material del framework)
        ├── assets/README.md                 (coloca aquí el binario)
        └── java/dev/arca/probe/MainActivity.kt
```

Sin dependencias externas (sin AndroidX): UI programática, un botón, un
log. Desechable tras F0.

## 1. Prerrequisitos (Deepin)

| Herramienta | Versión | Nota |
|---|---|---|
| JDK | 17 | `sudo apt install -y openjdk-17-jdk-headless` |
| Android SDK | cmdline-tools + `platforms;android-34` + `build-tools` | instalación sin Android Studio: blueprint `docs/09-build-deepin.md` §1.2 |
| Android NDK | r26+ (probado r27c) | **solo** para compilar el binario Rust, no para Gradle |
| Gradle | 8.9 (8.7+ vale) | `sudo apt install gradle` o descarga manual del zip |
| Rust + cargo-ndk | stable + `aarch64-linux-android` | blueprint `docs/09` §1.1 |

Variables de entorno (o `app/../local.properties` con `sdk.dir=/home/<user>/Android/Sdk`):

```bash
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018   # ajusta
export PATH=$PATH:$ANDROID_HOME/platform-tools             # adb
```

## 2. Compilar el binario y meterlo en los assets

```bash
# (desde la RAÍZ del repo Arca)
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static-pie" \
  cargo ndk -t arm64-v8a -p 26 -o /tmp/arca-probe-jniLibs \
  build --release -p devapp-hello

cp target/aarch64-linux-android/release/devapp-hello \
   host-probe/app/src/main/assets/devapp-hello
```

Ver `crates/L3-devapps/devapp-hello/README.md` para el quality gate
(`readelf -d` sin `DT_NEEDED`).

## 3. Compilar el APK

```bash
cd host-probe

# OPCIÓN A (recomendada): wrapper de Gradle — genera los archivos del
# wrapper una sola vez (requiere un gradle cualquiera instalado):
gradle wrapper --gradle-version 8.9
./gradlew assembleDebug

# OPCIÓN B: gradle de sistema directo:
gradle assembleDebug
```

Salida: `app/build/outputs/apk/debug/app-debug.apk` (firmado con el keystore
de depuración; listo para instalar).

## 4. Instalar y ejecutar en el teléfono

```bash
adb devices                      # habilita depuración USB en el teléfono
adb install -r app/build/outputs/apk/debug/app-debug.apk

# en otra terminal, ANTES de pulsar el botón:
adb logcat -s ArcaProbe
```

En el teléfono: abre la app **Arca Probe (F0)** → pulsa **Ejecutar
devapp-hello**. Qué verás (en logcat y en pantalla):

1. `instalado: /data/user/0/dev.arca.probe/files/devapp-hello (N B)`
2. `{"event":"hello",…,"uid":10xxx,"cwd":"/data/user/0/dev.arca.probe",…}` → **exec OK**
3. `{"ts":…,"pid":…,"seq":1..60}` cada 500 ms → **heartbeat** (el watchdog corta a los 30 s)
4. `{"event":"sigterm","seq":N}` → el hijo murió limpio por SIGTERM
5. `exit code = 0` → **GO** (rellena `decision.md`)

Si en el paso 2 ves `FAIL: … error=13, Permission denied` → el dispositivo
bloquea el exec: **NO-GO** (rellena `decision.md` y sigue su regla de pivot).

Trucos:
- El binario del asset se empaqueta al **build**: si cambias el binario,
  recompila el APK (`./gradlew assembleDebug`) antes de reinstalar.
- Si `adb install` se queja del targetSdk bajo: usa
  `adb install -r --bypass-low-target-sdk-block` (docs/01 §2).

## 5. Receta sugerida para `just probe` (para el orquestador)

El `justfile` de la raíz del repo NO se toca desde esta tarea. Receta
sugerida (el orquestador T01/T03 la añadirá cuando el SDK esté disponible):

```just
# Gate F0: binario → asset → APK → instalar → instruir al usuario
# (requiere: NDK, cargo-ndk, SDK+adb, gradle; ver host-probe/README.md)
probe:
    RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static-pie" \
        cargo ndk -t arm64-v8a -p 26 -o /tmp/arca-probe-jniLibs \
        build --release -p devapp-hello
    readelf -d target/aarch64-linux-android/release/devapp-hello | grep -q NEEDED && \
        { echo "FAIL: binario no estático"; exit 1; } || true
    cp target/aarch64-linux-android/release/devapp-hello \
        host-probe/app/src/main/assets/devapp-hello
    cd host-probe && gradle assembleDebug && \
        adb install -r app/build/outputs/apk/debug/app-debug.apk
    @echo "Abre 'Arca Probe (F0)' y pulsa el botón; log: adb logcat -s ArcaProbe"
    @echo "Rellena el veredicto en host-probe/decision.md"
```

## 6. Qué NO es este proyecto

- No forma parte del workspace Cargo (el `Cargo.toml` raíz lo excluye).
- No es el host real de Arca (ese vivirá en `host-android/`, F3+): no tiene
  AIPC, ni render, ni paquetes .arca. Es un probador de UNA cosa: ¿permite
  tu ROM ejecutar ELFs extraídos con targetSdk 28?
