# CLAUDE.md

Alias corto: lee y cumple **[AGENTS.md](AGENTS.md)** íntegro antes de escribir código.

Recordatorios mínimos:
- Fuente de verdad: `tasks/TASKS.json` → `specs/<crate>.md` → `docs/03-decisiones-adr.md` → `graphs/*.mmd`.
- Contratos primero: si el código contradice la spec, gana la spec; duda = `ISSUE-<task>.md`, no silencio.
- Al terminar: fmt, clippy `-D warnings`, tests, grafo actualizado, worklog, `status: done`.
