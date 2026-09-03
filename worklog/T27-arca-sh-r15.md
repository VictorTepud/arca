# T27 — r15: arca.sh deps deja de morir en silencio

## Qué hice

Reporte del usuario en su PC (Ubuntu/Deepin, ruta `~/Desktop/arca-main
(1)/arca-main`, ZIP descargado de GitHub):

```
$ ./arca.sh build → [ERROR] no hay Gradle; corre: ./arca.sh deps
$ ./arca.sh deps  → Rust OK, Java OK, "descargando platform-tools..." y
                    vuelve al prompt SIN [OK] ni [ERROR]
$ ./arca.sh build → [ERROR] no hay Gradle  (de nuevo)
```

Diagnóstico: **dos bugs compuestos**.

1. **Bug de arca.sh (r14)**: en `instalar_sdk`, el pipeline `yes |
   sdkmanager … >sdk-install.log 2>&1` corría DESNUDO bajo `set -e`.
   Cuando sdkmanager fallaba, `set -e` mataba el script EN ESA LÍNEA —
   antes de `local rc=$?`, del `tail` del log y del `error "no pude
   instalar los paquetes del SDK"`. Todo el manejador era **código
   muerto**: deps terminaba en silencio con el exit code del pipeline, sin
   [OK] SDK, sin descargar Gradle. El `build` siguiente reclamaba "no hay
   Gradle" — síntoma correcto de un estado incompleto, pero sin pista de
   la causa raíz (que quedó tragada en `.arca-tools/sdk-install.log`).
2. **Disparador probable**: la ruta del ZIP re-descargado trae espacios y
   paréntesis ("arca-main (1)"), un clásico rompe-herramientas de
   Android (sdkmanager/aapt2/Gradle). La causa exacta del sdkmanager
   nunca se vio por el bug 1; con r15 ya sería visible.

Fixes en `arca.sh` (r15):

- `instalar_sdk`: el pipeline va ahora dentro de `if ! …` — con `set -e`,
  la condición no dispara errexit, el manejador se alcanza, imprime el
  `tail -n 20` de `sdk-install.log` y sale con `[ERROR]` explicando dónde
  está el log completo. `pipefail` sigue OFF solo en ese bloque (razón
  original conservada: `yes` muere por SIGPIPE=141 cuando sdkmanager
  cierra stdin y, con pipefail, una instalación exitosa parecería
  fallida).
- **Guard de ruta**: `REPO_ROOT` con espacios o paréntesis → `[ERROR]`
  inmediato con el `mv` exacto para arreglarlo (antes de gastar minutos
  en downloads que fallarían de formas extrañas más adentro).
- **deps**: un SDK a medias ya no se da por bueno — se checa
  `platform-tools/adb` **y** `platforms/android-34` **y**
  `build-tools/34.0.0`; si falta cualquiera, `instalar_sdk` repara
  (sdkmanager es idempotente con lo ya instalado). Antes, un deps
  interrumpido tras instalar solo platform-tools dejaba pasar el check
  y el fallo explotaba mucho después dentro de Gradle.
- Descompresiones (cmdline-tools/JDK/Gradle) con `|| error …` visible
  (mismo patrón de muerte silenciosa, mismo fix).
- Hardening de `deps` documentado en la cabecera del script y en el
  README raíz (fila r15).

## Verificación

`bash -n` + harness `scripts` externo al repo
(`/home/z/my-project/scripts/test_r15.sh`, 13/13 PASS):

- guard dispara en la ruta EXACTA del usuario ("arca-main (1)") y
  sugiere el `mv`; `help` sigue funcionando en ruta limpia;
- patrón `if !`: sdkmanager fallando (return 3) → exit 1 CON `[ERROR]` y
  el `tail` muestra la causa simulada (no silencio);
- sdkmanager OK + SIGPIPE de `yes` → exit 0 (sin falso positivo);
- con pipefail ON el mismo run exitoso daría 141 → confirma por qué va
  OFF en ese bloque;
- reproducción del bug r14: pipeline desnudo + set -e → exit 3
  silencioso, el manejador nunca imprime (confirma el diagnóstico).

Nota: el harness vive fuera del repo a propósito — prueba un script de
infraestructura local del sandbox, no código del contenedor.

## Qué rompí / Qué falta

- Nada roto conocido. r14 (devapp-calc) sigue intacto; r15 solo toca
  `arca.sh` y documentación.
- La causa raíz del sdkmanager EN LA MÁQUINA DEL USUARIO sigue
  pendiente de confirmar: el log completo está en
  `.arca-tools/sdk-install.log` (pre-mover la carpeta) o será visible
  con r15 si reaparece tras el `mv`.

## Próxima tarea sugerida

El usuario mueve la carpeta a `~/arca`, re-corre `deps && build`,
instala y prueba la calculadora en el Huawei (`./arca.sh run calc`),
y reporta `logs/` (pendiente de hardware del r14). Después: F3b.
