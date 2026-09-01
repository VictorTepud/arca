# arca-store

registry SQLite + migraciones (apps/caps/instancias/audit).

- Capa: L1 (`L1-arca-store/`)
- Spec: `specs/arca-11-*.md` (blueprint) · ADR-011
- Tarea: T07 · Estado: **implementado**
- unsafe: **no** · Errores: `ArcaError` (contexto estático + `tracing`)

## API (resumen)

```rust
let store = Store::open(&path)?;            // WAL + pragmas + migraciones v→2
store.begin()? -> Tx                        // single-writer (MutexGuard + BEGIN IMMEDIATE)
store.upsert_app(&mut tx, &manifest, src)   // install/update: fila + caps del manifest
store.get_app(&id) -> Option<AppRecord>     // registro de una app
store.list_apps(Filter::all().with_cap(..)) // launcher / panel de permisos
store.delete_app(&mut tx, &id)              // uninstall: cascada caps+instancias
store.grant_caps / revoke_cap / caps_of     // permisos (granted al instalar)
store.register_instance / finish_instance   // histórico spawn/exit
store.audit(&ev) / audit_batch(&evs[..])    // append (batch = queue+flush §5)
store.query_audit(&id, since)               // por app/tiempo (índice v2)
```

## Esquema (user_version)

- **v1**: `apps`, `app_caps` (FK CASCADE), `instances` (FK CASCADE),
  `audit_log` (sin FK: append-only, sobrevive al uninstall).
- **v2**: `apps.updated_at` (backfill con `installed_at`) +
  `idx_audit_app_ts`.

Migración: una transacción por paso (número + DDL juntos), `.bak` antes de
migrar dbs con datos, versión futura → error. WAL + `busy_timeout` 5 s.

## Decisiones

- Single-writer: `Mutex<Connection>` (`Connection` es `!Sync`); `Tx` retiene
  el guard → escritor único por diseño. Lecturas del launcher: otra
  conexión en WAL.
- `DateTime` del contrato → `UnixMs` local; `CapabilitySet` → bitset local
  (el de `arca-permissions` no es dependencia permitida, T14).
- Enmienda deps: `+tracing` (ADR-014; detalle dinámico de errores sqlite).
