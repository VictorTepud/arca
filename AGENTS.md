# AGENTS.md — Reglas para agentes de código (multiagente)

Este archivo es el **contrato operativo** de cualquier agente (humano o IA) que escriba código en el workspace de Arca. Léelo completo antes de tu primera tarea y vuelve a consultarlo al iniciar cada tarea.

## 1. Fuente de verdad (jerarquía)

1. `tasks/TASKS.json` — tu tarea, dependencias y Definition of Done (DoD).
2. `specs/<crate>.md` — el **contrato** de tu crate: API pública, invariantes, errores comunes. Si el código contradice la spec, **gana la spec**; si crees que la spec está mal, detén la tarea y reporta (no la cambies en silencio).
3. `docs/03-decisiones-adr.md` — decisiones de arquitectura; no las contradigas.
4. `graphs/<modulo>.mmd` — mapa de dependencias; mantenlo sincronizado cuando cambies dependencias.

**Nunca** inventes comportamiento no especificado: pregunta (abre un `ISSUE-<task>.md` en la raíz del repo) o elige la opción más conservadora y documéntala con `// NOTA(agent):`.

## 2. Reglas de modularidad (inquebrantables)

- Cada crate vive en `crates/<nombre>/` y **solo** puede depender de los crates listados en su spec (sección "Dependencias permitidas").
- Prohibido el "chatarro transversal": si necesitas un tipo compartido nuevo, va en `arca-types` (o en el crate dueño del dominio) — nunca duplicado.
- Prohibido `unsafe` fuera de los crates marcados `unsafe-heavy` en su spec (shm, exec-native, 7z, exec-wasm, android-glue). Y todo `unsafe` lleva comentario de invariante de seguridad.
- Un cambio en `arca-protocol`, `arca-gfx-protocol` o `arca-pkg-model` exige bump de versión y paso de tests de compatibilidad (ver §6).
- **Grafo por módulo**: cada vez que añadas/elimines una dependencia o módulo interno (archivo `src/*.rs` relevante), actualiza `graphs/<modulo>.mmd` y el diagrama interno de la spec. Un grafo desactualizado = tarea incompleta.

## 3. Convenciones de código

- Rust **stable**, edition 2021, `#![deny(missing_docs)]` en crates públicos.
- Errores: `thiserror` en librerías, `anyhow` solo en binarios/tools/tests.
- Serialización binaria: `rkyv` (con feature `validation`). Nada de `serde_json` en paths calientes.
- Async solo en `arca-host-core` y `arca-tools-*` (tokio). Los crates de protocolo y shm son **síncronos y sin allocaciones en el path de frame**.
- Logging: `tracing` con target = `arca::<crate>::<módulo>` (esto es lo que hace localizable un error en 10 segundos — ver §5).
- Formato: `cargo fmt --all` sin excepciones; lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Commits convencionales: `feat(arca-wm): ...`, `fix(arca-7z): ...`, `spec(exec-native): ...`.

## 4. Protocolo de trabajo por tarea

1. Lee `worklog/` del repo (bitácora de agentes anteriores) — no el de este blueprint.
2. Marca la tarea como `in_progress` en `tasks/TASKS.json` (campo `status`).
3. Lee la spec del crate y los grafos implicados.
4. Implementa → tests → `fmt` + `clippy` → actualiza grafo/spec si cambiaron contratos.
5. Cierra: `status: done` y append en `worklog/` con el formato del §6.
6. **No toques** crates de otras tareas abiertas (mira `owner`/`status` en TASKS.json). Si necesitas un cambio en otro crate, créalo como tarea nueva dependiente.

## 5. Diagnóstico rápido de errores (por qué existen los grafos)

Cada error en runtime llega con su target de tracing. La cadena es:

```
error en arca::arca-gfx-host::pipeline   →  mira graphs/gfx.mmd  →  verás de quién recibe
                                           meshes (arca-gfx-protocol) y a quién culpa
                                           (arca-shm) → specs de esos dos crates → tabla
                                           "errores comunes" → causa probable.
```

La regla: **ningún módulo puede tener más de ~7 colaboradores directos** (in-+out-degree en `MASTER.mmd`). Si supera, hay que partirlo — pídelo, no lo fuerces.

## 6. Formato de bitácora (worklog del repo de código)

```
---
Task ID: T07
Agent: <nombre/modelo>
Fecha: <iso>

Qué hice: ...
Decisiones tomadas: ... (y por qué no la alternativa)
Qué rompí/Qué falta: ...
Próxima tarea sugerida: ...
```

## 7. Anti-patrones (rechazo automático en review)

- `unwrap()`/`expect()` fuera de tests (usa `?` + contexto).
- Bucles de reintento sin backoff en IPC.
- Allocar en el path de frame (60 Hz): cada `Vec`/`String` nueva por frame es un bug de performance.
- Bloquear el hilo del compositor esperando a una sub-app (modo pipelined, §docs/04).
- Ignorar el versionado del protocolo (`AIPC v1` handshake).
- Copiar 7z a memoria antes de descomprimir (streaming).
- Dormir el host para "esperar" a un proceso hijo (usa epoll/eventfd).

## 8. DoD global (para toda tarea)

- [ ] Tests de la spec pasando (`cargo test -p <crate>`).
- [ ] `cargo fmt` + `clippy -D warnings` limpios.
- [ ] Spec y grafo actualizados si cambió el contrato.
- [ ] Entrada en `worklog/`.
- [ ] `TASKS.json.status = done`.
