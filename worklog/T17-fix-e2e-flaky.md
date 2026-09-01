---
Task ID: T17
Agent: Super Z (agente principal · sesión sandbox Linux)
Fecha: 2026-09-01

Qué hice:
- **Diagnóstico de las 2 e2e flaky en Deepin** (`e2e_panic_de_la_app_exit_101`,
  `e2e_spawn_handshake_ping_kill9_dead`): en el sandbox pasaban 6/6 → falla
  NO determinista → condición de carrera. Dos causas raíz:
  1. `launch_full` construía el env del hijo filtrando las `ARCA_*` del
     **entorno del proceso de tests**, y los tests e2e corren en PARALELO en
     un mismo proceso: el `ARCA_PING_PANIC=1` del test de pánico se colaba al
     hijo del test de kill-9 (moría de exit 101 antes del kill → assert
     roto) y viceversa. En la Deepin del usuario (máquina más cargada) el
     interleaving lo provocaba; en el sandbox, no.
  2. El watcher hacía `waitpid(WNOHANG)` cada 5 ms: bajo carga, el drift del
     scheduler hacía que `Dead` llegara tarde (el e2e de kill-9 exige
     ≤ 50 ms). Además, nunca comprobaba `is_attached` → si el host soltaba
     el handle con el hijo vivo: fuga de hilo Y de proceso (contrato del
     módulo handle.rs violado).
- **Fix 1 — env hermético (LaunchSpec v2)**: nuevo campo `env_extra:
  Vec<(String,String)>` viaja por el blob del fd 3 (formato v2, versión
  bumpada — v1 rechazada en decode). `arca-launch` construye el env del hijo
  SOLO desde la spec (identidad + pares); eliminado el passthrough de
  `std::env::vars()`. `launch_full` ahora pasa envp VACÍO a posix_spawn.
  Nueva API `launch_full_with_env(spec, env)` para fault-injection por
  instancia. Validación fail-closed (`validar_env_extra`): solo claves
  `ARCA_*` ASCII sin NUL, sin tocar las 4 de identidad, ≤16 pares, ≤256 B.
  +4 tests unitarios (roundtrip v2, versión vieja rechazada, validación,
  bytes sobrantes).
- **Fix 2 — watcher**: reap con `waitpid` BLOQUEANTE en hilo dedicado
  (detección de muerte en µs — el kernel despierta el hilo) + watchdog en
  el watcher principal: cada 250 ms comprueba `driver.is_attached()`; host
  soltado → SIGKILL + reap + reporte (muere la fuga latente). ECHILD/errores
  → `Lost` (fail-closed, como antes).
- **Fix 3 — e2e**: `set_var`/`remove_var` globales ELIMINADOS; pánico y
  socket se inyectan por instancia con `launch_full_with_env`. Tests
  100% paralelos sin contaminación cruzada.
- **Cross-compilación sin NDK**: `.cargo/config.toml` con `rust-lld` +
  `rustflags` (crt-static, relocation-model=pic, link-arg=-pie) para
  aarch64/armv7 musl → `devapp-hello` sale ELF **estático-PIE**
  (verificado: Type=DYN, 0 DT_NEEDED) sin instalar el NDK.
- **`arca.sh`** (todo-en-uno, r4): deps (Rust+targets, JDK 17 vía apt o
  Temurin portátil, cmdline-tools + platform-tools + android-34 +
  build-tools 34, Gradle 8.9), test (6 e2e PC), build (devapp-hello arm64
  static-pie con gate readelf + gradle assembleDebug), install (adb -r con
  retry de firma), run (am start), logs (logcat -s ArcaProbe + errores +
  dumpsys targetSdk → logs/arca-logs-*.txt), todo, limpiar. Salida del
  sdkmanager con `yes |` protegida con set +o pipefail.
- README raíz actualizado (315 tests, r4, arca.sh, F0=GO en hardware).

Decisiones tomadas (y por qué no la alternativa):
1. **`env_extra` en LaunchSpec y no en AppSpec (ABI)**: AppSpec es el ABI
   compartido con exec-wasm/bench/installer; tocarlo obliga a updates en
   cadena por un fix de tests. `launch_full_with_env` es pub y suficiente;
   cuando F3 necesite env por app (manifest), se promueve al ABI con calma.
2. **waitpid bloqueante + hilo dedicado y no pidfd/poll**: pidfd añadiría
   dependencia de kernel 5.3+ y llamada cruda por libc (dep nueva para una
   librería que hoy solo usa nix). El hilo bloqueante da los mismos µs de
   latencia con las deps existentes; el costo es 1 hilo por instancia
   (ya hay 3: watch + 2 drains), todos de vida corta.
3. **Watchdog de detach cada 250 ms y no inmediato**: la muerte del hijo
   sigue detectándose en µs (hilo reap); el 250 ms solo afecta el caso
   "host soltó el handle" que antes NUNCA se detectaba.
4. **VERSION=2 sin compat con v1**: host y arca-launch se buildan del mismo
   repo en la misma pasada; aceptar v1 silenciosamente escondería mezclas.

Qué rompí/Qué falta:
- Nada roto: 315/315 tests verdes (workspace completo), clippy -D warnings
  verde, fmt verde. E2E 6/6 ×5 corridas, incluso con los 2 núcleos al 100%
  (condición que rompía el kill-9 en Deepin). Ping RTT p99 = 22 µs.
- `host-probe/decision.md` sigue siendo plantilla: falta que el usuario
  registre su GO (ya lo demostró de facto: exit 0, heartbeats 1–60).
- El e2e de Android (las 6 pruebas del motor EN el teléfono) no existe
  aún — es trabajo de T22 (host-core) según el plan del blueprint.

Próxima tarea sugerida:
- Usuario: `./arca.sh todo` en Deepin → confirmar 6/6 en PC + APK instalado
  + enviar logs/arca-logs-*.txt. Formalizar GO en decision.md.
- Luego: F3 (T17a en numeración del blueprint: gfx-protocol/input/wm/
  sdk) — la parte visual del contenedor.
