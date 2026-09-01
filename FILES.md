# FILES.md — inventario completo del repositorio (r3)

> Con este manifiesto puedes verificar que **no falta nada**: son todos los
> archivos que componen Arca F0-F2 (r3). El motor PC (crates), la sonda
> Android (host-probe) y el automatizador (arca.sh) son *autocontenidos*:
> no requieren ningún archivo externo que no se descargue solo.

Total: **37 archivos** + este manifiesto.

## Raíz

| archivo | qué es |
|---|---|
| `arca.sh` | **el script maestro**: deps / test / build / install / run / logs / todo |
| `README.md` | guía de uso en español simple (empezar por ahí) |
| `Cargo.toml` | workspace Rust: declara los 4 crates en capas L0-L2 |
| `Cargo.lock` | versiones exactas de dependencias (repo reproducible) |
| `rust-toolchain.toml` | fija la toolchain `stable` para todos |
| `.gitignore` | qué NO entra al repo (compilados, SDK local, binarios copiados a assets) |
| `.cargo/config.toml` | enlazador cruzado `rust-lld` → compila para Android **sin NDK** |

## `crates/` — el motor (Rust, probado 6/6 en PC)

| archivo | qué es |
|---|---|
| `crates/arca-log/` | **L0** · mini-logger estilo `tracing` (colores, niveles `ARCA_LOG`) |
| `crates/arca-log/src/lib.rs` | implementación + macros `log_info!` etc. |
| `crates/arca-ipc/` | **L0** · protocolo AIPC: tramas `[u32 len][u8 tag][payload]` |
| `crates/arca-ipc/src/lib.rs` | tags PING/PONG/HELLO/SHUTDOWN + `enviar`/`recibir` |
| `crates/arca-rt/` | **L1** · runtime del lado de la sub-app |
| `crates/arca-rt/src/bin/arca-ping.rs` | la sub-app de prueba: modos `serve/panic/hang`, canal por fd 3 (PC) o stdio (Android) |
| `crates/arca-exec-native/` | **L2** · motor del supervisor |
| `crates/arca-exec-native/src/lib.rs` | `Instancia`: ping / apagar / matar9 / finalizar + Drop sin fugas |
| `crates/arca-exec-native/src/spawn.rs` | lanzamiento: socketpair + `pre_exec` (fd 3, no_new_privs, rlimit) |
| `crates/arca-exec-native/src/watch.rs` | **el vigía**: `waitpid` → exit codes y señales fieles, cero zombis |
| `crates/arca-exec-native/src/drain.rs` | drenaje de stdout/stderr con re-emisión etiquetada |
| `crates/arca-exec-native/tests/e2e.rs` | las 6 pruebas e2e que corre `./arca.sh test` |

## `host-probe/` — la sonda Android (APK, Java puro, targetSdk 28)

| archivo | qué es |
|---|---|
| `host-probe/settings.gradle` | declara el módulo `:app` y los repositorios |
| `host-probe/build.gradle` | plugin de Android (AGP 8.5.2) |
| `host-probe/gradle.properties` | memoria JVM y desactivación de AndroidX |
| `host-probe/app/build.gradle` | **targetSdk 28** (la clave de todo), minSdk 24, Java 17 |
| `host-probe/app/src/main/AndroidManifest.xml` | declara la actividad y el nombre «Arca Probe F0» |
| `host-probe/app/src/main/java/dev/arca/probe/MainActivity.java` | botón + consola en pantalla + log interno rotatorio |
| `host-probe/app/src/main/java/dev/arca/probe/NativeHost.java` | lanza `arca-ping` (ProcessBuilder), habla AIPC por stdio y corre las 6 pruebas **en el teléfono** |
| `host-probe/app/src/main/assets/arca-bin/` | destino de los binarios que copia `arca.sh build` (va vacío a propósito: los binarios se compilan en tu máquina) |

## `gradle/` — wrapper estándar (opcional, para Android Studio)

| archivo | qué es |
|---|---|
| `gradlew` / `gradlew.bat` | Gradle «portátil» del repo (Linux/Mac y Windows) |
| `gradle/wrapper/gradle-wrapper.jar` | motor del wrapper |
| `gradle/wrapper/gradle-wrapper.properties` | versión: Gradle 8.7 (la misma que usa `arca.sh`) |

> `arca.sh` trae su propio Gradle en `.arca-tools/` (sin ensuciar tu home);
> el wrapper está por convención, por si abres el proyecto en un IDE.

## `graphs/` — mapas para cazar errores rápido

| archivo | qué es |
|---|---|
| `graphs/crates-f0-f1-r2.mmd` | capas y dependencias entre crates |
| `graphs/motor-nativo-f0-f1-r2.mmd` | flujo del motor: spawn → vigía → eventos |
| `graphs/android-f0-r3.mmd` | flujo completo: arca.sh → APK → teléfono → logs |

## ¿Y los ~60 archivos que veo en mi zona de descargas?

Son entregas **anteriores** de otras sesiones: el *blueprint* de diseño (58
archivos: docs, specs, tareas) y paquetes viejos (r1 con el error, r2). Este
repo es la **r3, la línea corregida**: el motor r2 + sonda Android + script.
Nada de eso anterior hace falta para compilar o ejecutar; el blueprint es
documentación de diseño (si lo quieres conservar en el repo, descomprime tu
`arca-blueprint.zip` viejo dentro y haz commit).
