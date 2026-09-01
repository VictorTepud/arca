# Arca — paquete F0-F2 (r3): motor corregido + sonda Android + script único

## La buena noticia primero

Tu prueba anterior ya respondió la pregunta **más importante** de todo el
proyecto: ejecutaste la sub-app desde el APK en tu teléfono y salió
`exit code = 0`. Eso significa que **Android te deja lanzar binarios Rust
estáticos desde dentro de una app con targetSdk 28**. El gran riesgo del
proyecto ya pasó: **Arca es viable (F0 = GO)**.

Este paquete reúne todo en uno:

- el **motor corregido (r2)**: los 2 tests que fallaban en tu PC ya pasan;
- la **sonda Android**: un APK (*Arca Probe F0*) que corre las mismas 6
  pruebas, pero **en tu teléfono**;
- **`arca.sh`**: un solo script que hace todo — instala dependencias,
  compila, arma el APK, lo instala, corre la sonda y guarda los logs.

## Requisitos

- Tu Deepin con internet (la primera vez descarga JDK, SDK de Android y
  Gradle; luego queda cacheado en `.arca-tools/`).
- Un Android 10 o mayor, con **Depuración USB** activada, y su cable.
- Unos 2 GB libres en el disco.

## El camino corto (un solo comando)

```bash
cd arca-src-f0-f2-r3
./arca.sh todo
```

Eso hace todo en orden: dependencias → 6 tests del motor en tu PC →
compilación para 3 arquitecturas (sin NDK: usa el enlazador que ya viene con
Rust) → APK → instalación en el teléfono → la sonda corre sola en la
pantalla → espera 45 s → guarda el registro.

Al final verás algo como `logs/arca-logs-20260902-193000.txt`.
**Ese archivo es el que me envías** para revisar cómo estuvo.

En la pantalla del teléfono, la última línea debería decir:
`RESULTADO: 6 OK / 0 FALLAS (de 6)`.

## Comandos por separado

| comando | qué hace |
|---|---|
| `./arca.sh todo` | todo el flujo de arriba |
| `./arca.sh deps` | solo instala dependencias (Rust, JDK, SDK, Gradle) |
| `./arca.sh test` | solo los 6 tests del motor en tu PC |
| `./arca.sh build` | solo compila binarios (3 arquitecturas) + APK |
| `./arca.sh install` | solo instala el APK con adb |
| `./arca.sh run` | solo lanza la sonda en el teléfono |
| `./arca.sh logs` | solo captura los logs a un archivo |
| `./arca.sh limpiar` | borra compilados (conserva dependencias) |
| `./arca.sh --skip-deps todo` | corre «todo» sin revisar dependencias |

## Las 6 pruebas (las mismas en PC y en el teléfono)

| # | prueba | qué demuestra |
|---|--------|---------------|
| 1 | spawn + handshake + ping + apagado | ciclo completo de vida de una sub-app |
| 2 | logs drenados | los logs del «cartucho» llegan etiquetados, con su pid |
| 3 | 25 spawns sin zombis | encender y apagar muchas veces sin ensuciar el sistema |
| 4 | pánico → exit 101 | una sub-app que revienta muere sola y ordenada |
| 5 | kill -9 → enterrado | aunque la maten a lo bestia, no queda zombi |
| 6 | canal cerrado → exit 0 | si el supervisor desaparece, la sub-app se apaga sola |

## Si algo falla

1. **«no hay teléfono conectado»** → activa Depuración USB (Ajustes →
   Acerca del teléfono → toca 7 veces «Número de compilación» → Ajustes →
   Opciones de desarrollador → Depuración USB), conecta y acepta el cuadro
   de «¿Permitir depuración USB?».
2. **Fallo al instalar** → si había un APK viejo con otra firma, el script
   lo desinstala y reintenta solo.
3. Cualquier otra cosa: corre `./arca.sh logs` y **envíame el archivo** que
   genera. Con eso veo exactamente qué pasó.

## Qué hay adentro

```
arca-src-f0-f2-r3/
├── arca.sh                  # EL script (todo en uno)
├── Cargo.toml               # workspace Rust (4 crates en capas)
├── .cargo/config.toml       # enlazado cruzado sin NDK (rust-lld)
├── crates/
│   ├── arca-log/            # L0: mini-logger estilo tracing
│   ├── arca-ipc/            # L0: protocolo AIPC (tramas tag+len)
│   ├── arca-rt/             # L1: runtime de la sub-app
│   │   └── src/bin/arca-ping.rs   # la sub-app de prueba (el "devapp-hello"
│   │                              # de antes, mejorado: canal por stdio)
│   └── arca-exec-native/    # L2: motor del supervisor + 6 tests e2e
├── host-probe/              # el APK Android (Java puro, sin dependencias)
│   ├── app/build.gradle     # targetSdk 28  ← la clave de todo esto
│   └── app/src/main/java/dev/arca/probe/
│       ├── MainActivity.java     # botón + consola en pantalla
│       └── NativeHost.java       # lanza arca-ping, habla AIPC, corre las 6 pruebas
└── graphs/                  # diagramas Mermaid para ubicar errores rápido
    ├── crates-f0-f1-r2.mmd
    ├── motor-nativo-f0-f1-r2.mmd
    └── android-f0-r3.mmd    # flujo completo del script en tu máquina
```

Notas rápidas:

- La app se llama **Arca Probe F0** (paquete `dev.arca.probe`).
- La sub-app de prueba se llama `arca-ping`: es el «devapp-hello» de la otra
  IA, con el protocolo corregido y modo `stdio` para Android.
- En el teléfono los logs viven en el tag `ArcaProbe` de logcat y en un
  archivo interno; `./arca.sh logs` junta todo en un solo `.txt`.
- El targetSdk 28 es **a propósito**: es lo que permite ejecutar binarios
  propios. No lo subas.
- Los primeros minutos de `./arca.sh todo` son descargas; después es rápido
  reintentar: lo que ya se descargó no se baja de nuevo.

## Siguiente paso (cuando la sonda esté en verde)

Con `6 OK / 0 FALLAS` en tu teléfono, F0 y F1 quedan demostrados de punta a
punta (PC y Android). Lo que sigue en el plan es **F2**: el instalador de
paquetes `.arca` (7z + firma ed25519) y el sandbox real por sub-app
(seccomp-BPF). Ahí ya empieza a verse «la consola con cartuchos».
