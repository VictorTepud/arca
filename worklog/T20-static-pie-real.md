---
Task ID: T20
Agent: Super Z (agente principal)
Fecha: 2026-09-02

Qué hice: **fix de raíz del exit 139 (SIGSEGV) de TODAS las devapps
aarch64 en el teléfono** — devapp-demo Y devapp-hello morían al
arrancar (pantalla negra; "hijo ya cerrado" al tocar era consecuencia).
Resultó que NINGÚN binario del cross musl de rustc-lld había corrido
nunca en hardware: el GO de F0 usó el binario biónico (4.28 MB) del
paquete original, construido con cargo-ndk.

- Reproducción: qemu-aarch64-static ejecuta el binario roto y cae con
  `SIGSEGV si_code=1 si_addr=NULL` ANTES de la primera syscall —
  arranque, no lógica. (El usuario confirmó que hello también moría en
  el teléfono → el repro de qemu era fiel, no un falso positivo.)
- Diagnóstico: `.cargo/config.toml` pasaba `-C link-arg=-pie` a
  rust-lld. El target aarch64-unknown-linux-musl de rustc no activa PIE
  (pasa -no-pie y enlaza crt1.o). Resultado: ET_DYN (pasa el gate de
  readelf: DYN + 0 DT_NEEDED) **sin auto-reubicación** — 419
  R_AARCH64_RELATIVE declaradas en .rela.dyn que NADIE procesa (crt1 no
  reubica). Android carga el ET_DYN en base aleatoria → todos los
  punteros absolutos apuntan al vacío → SIGSEGV instantáneo. El
  x86_64-unknown-linux-musl (e2e del motor en PC) SÍ es static-pie por
  defecto (rcrt1 + 616 R_X86_64_RELATIVE) — por eso esto nunca se vio
  en el sandbox.
- Fix: **wrapper de linker `tools/ld.lld`** (python3, ~70 líneas,
  instalado en ~/.cargo/bin por `./arca.sh build` de forma idempotente).
  En links cross-musl con crt1.o self-contained: sustituye crt1.o →
  rcrt1.o (arranque static-PIE con auto-reubicación, incluido en la
  toolchain de rustup — también para armv7) y -no-pie → -pie. Para
  cualquier OTRO link: passthrough puro. Se llama ld.lld porque rustc
  elige el estilo de argumentos por el nombre del linker y lld
  despacha por argv[0].
- `.cargo/config.toml` r8: linker = "ld.lld" + crt-static +
  relocation-model=pic (fuera el "-C link-arg=-pie" manual). armv7 con
  la misma receta (su self-contained sí trae rcrt1.o).
- Bisección que lo cerró (todo bajo qemu): B (pic sin -pie) = ET_EXEC
  funcional (Android lo rechaza) · C (anterior) = ET_DYN roto ·
  W (wrapper) = **ET_DYN + DT_FLAGS_1 PIE + 419 RELATIVE + CORRE**.

Verificación (bajo qemu-aarch64-static con los binarios REALES del
repo): hello = 18 líneas (hello+heartbeats+sigterm) · demo --selftest =
OK exit 0 · demo modo teléfono (fb 336×720 + touch down/move/up + ping
+ shutdown) = **exit 0**, 151 frames, 1 pong, 1 stats, 0 fatal,
framebuffer con magic AFRM y seq impar (303/301) en ambos slots · gate
verifica_elf.py OK en ambos · relocs: hello 419, demo 481 · bash -n
arca.sh OK. Nota: `grep -c R_AARCH64_RELATIVE` da 0 por truncamiento de
readelf (muestra "RELATIV"); contar con "RELATIV".

Próxima tarea sugerida: reconstruir en Deepin (`./arca.sh build` →
reinstala APK → demo en el teléfono) y validar la pantalla viva en
hardware; luego F3b.
