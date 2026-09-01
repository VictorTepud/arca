//! Esquema SQL y migraciones versionadas (spec 11 §4).
//!
//! El versionado usa `PRAGMA user_version` (entero en el header del archivo,
//! transaccional): cada paso v(n)→v(n+1) corre en SU transacción y termina
//! subiendo el número dentro de esa misma transacción — un crash a mitad
//! no puede dejar "esquema nuevo con número viejo".
//!
//! Antes de tocar el primer paso se copia el archivo a `<db>.bak`
//! (`registry.db.bak`): si la migración falla, el rollback transaccional ya
//! protege el original; el `.bak` es la segunda red (esquema pre-migración
//! inspeccionable, docs: "fallo de migración → no se borra nada").
//!
//! Idempotencia: cada paso usa `IF NOT EXISTS`/guardas de columna, y el
//! runner nunca re-ejecuta pasos ya aplicados (el número es la puerta).

use std::path::Path;

use arca_types::{ArcaError, Res};
use rusqlite::Connection;

/// Mapa sqlite → [`ArcaError`] de ESTE módulo (política spec 01 §5:
/// contexto estático en el error, detalle dinámico solo al log).
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(
        target: "arca::arca-store::schema",
        ctx,
        error = %e,
        "fallo sqlite"
    );
    ArcaError::Internal(ctx)
}

/// Última versión del esquema que ESTE código entiende (v2).
pub const SCHEMA_VERSION: u32 = 2;

/// Pasos ordenados: `MIGRATIONS[i]` lleva el esquema a la versión `i+1`.
/// Funciones (no closures) para que el array sea `const`.
const MIGRATIONS: &[fn(&Connection) -> Res<()>] = &[migrate_v1, migrate_v2];

/// Lee `PRAGMA user_version` (0 = recién creada o pre-versionado).
pub(crate) fn user_version(conn: &Connection) -> Res<u32> {
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| db("store: leer user_version", e))?;
    u32::try_from(v).map_err(|_| ArcaError::Internal("store: user_version negativo (corrupción)"))
}

/// Sube `user_version` (transaccional: llamar DENTRO de la tx del paso).
fn set_user_version(conn: &Connection, v: u32) -> Res<()> {
    // `v` es u32 del array de migraciones: sin riesgo de inyección.
    let sql = format!("PRAGMA user_version = {v}");
    conn.execute_batch(&sql)
        .map_err(|e| db("store: escribir user_version", e))
}

/// Lleva la conexión a [`SCHEMA_VERSION`] (no-op si ya está).
///
/// Errores si el archivo es de una versión FUTURA (host viejo ante db nueva:
/// no se inventa downgrade silencioso).
pub(crate) fn migrate(conn: &Connection, path: &Path) -> Res<()> {
    let from = user_version(conn)?;
    if from > SCHEMA_VERSION {
        return Err(ArcaError::Internal(
            "store: user_version del futuro (db más nueva que el host)",
        ));
    }
    if from == SCHEMA_VERSION {
        return Ok(()); // nada que hacer (idempotente)
    }
    if !db_vacio(conn)? {
        // Solo hay algo que perder si ya hay tablas (v0 con datos o v1):
        // snapshot ANTES de migrar.
        backup(path, conn)?;
    }
    for (i, paso) in MIGRATIONS.iter().enumerate().skip(from as usize) {
        let destino = (i + 1) as u32;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db("store: abrir tx de migración", e))?;
        // Cuerpo del paso + número en la MISMA tx (todo o nada).
        let cuerpo = paso(conn).and_then(|()| set_user_version(conn, destino));
        if let Err(e) = cuerpo {
            if let Err(e2) = conn.execute_batch("ROLLBACK") {
                tracing::warn!(
                    target: "arca::arca-store::schema",
                    error = %e2,
                    "rollback de migración falló (¿tx ya cerrada?)"
                );
            }
            return Err(e);
        }
        conn.execute_batch("COMMIT")
            .map_err(|e| db("store: commit de migración", e))?;
        tracing::info!(
            target: "arca::arca-store::schema",
            from,
            to = destino,
            "migración aplicada"
        );
    }
    Ok(())
}

/// v1 — esquema base: apps, permisos concedidos, instancias y auditoría.
///
/// Decisiones de esquema (spec 11 §3 "Estructura de tablas"):
/// - `app_caps`/`instances` referencian `apps(id)` con `ON DELETE CASCADE`
///   → uninstall limpia las filas hijas en el mismo DELETE.
/// - `audit_log` NO tiene FK: es append-only y SOBREVIVE al uninstall
///   (historial de seguridad; borrarlo sería destruir evidencia).
/// - `audit_log.id AUTOINCREMENT`: rowid nunca se reutiliza → el orden de
///   inserción es estable como desempate de timestamps iguales.
fn migrate_v1(conn: &Connection) -> Res<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS apps (
            id             TEXT PRIMARY KEY,
            name           TEXT NOT NULL,
            version        TEXT NOT NULL,
            min_host       TEXT NOT NULL,
            api_level      INTEGER NOT NULL,
            description    TEXT NOT NULL DEFAULT '',
            tags           TEXT NOT NULL DEFAULT '',
            installed_from TEXT NOT NULL,
            installed_at   INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS app_caps (
            app_id     TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
            cap        TEXT NOT NULL,
            granted_at INTEGER NOT NULL,
            PRIMARY KEY (app_id, cap)
        );
        CREATE TABLE IF NOT EXISTS instances (
            instance_id INTEGER PRIMARY KEY,
            app_id      TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
            version     TEXT NOT NULL,
            started_at  INTEGER NOT NULL,
            exited_at   INTEGER,
            outcome     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_instances_app ON instances(app_id, started_at);
        CREATE TABLE IF NOT EXISTS audit_log (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            app_id TEXT NOT NULL,
            cap    TEXT NOT NULL,
            ts     INTEGER NOT NULL,
            detail TEXT NOT NULL DEFAULT ''
        );
        "#,
    )
    .map_err(|e| db("store: migración v1", e))?;
    Ok(())
}

/// v2 — telemetría de update + índice del auditoría.
///
/// - `apps.updated_at`: marca el último update del registro; en v1 no
///   existía, así que se añade con `ALTER TABLE` (idempotente con guardia de
///   columna) y se **backfill-ea** con `installed_at` (no se pierde el dato
///   previo: las apps existentes "se actualizaron" cuando se instalaron).
/// - `idx_audit_app_ts`: índice compuesto de `query_audit(app, since)`
///   (sin él, cada consulta del panel es un full scan del log).
fn migrate_v2(conn: &Connection) -> Res<()> {
    if !columna_existe(conn, "apps", "updated_at")? {
        conn.execute_batch("ALTER TABLE apps ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0")
            .map_err(|e| db("store: migración v2 (updated_at)", e))?;
        conn.execute_batch("UPDATE apps SET updated_at = installed_at WHERE updated_at = 0")
            .map_err(|e| db("store: migración v2 (backfill)", e))?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_audit_app_ts ON audit_log(app_id, ts)")
        .map_err(|e| db("store: migración v2 (índice audit)", e))?;
    Ok(())
}

/// ¿La tabla tiene la columna? (guardia de idempotencia para `ALTER TABLE`).
fn columna_existe(conn: &Connection, tabla: &str, col: &str) -> Res<bool> {
    // `tabla`/`col` son literales internos: sin inyección.
    let sql = format!("PRAGMA table_info({tabla})");
    let mut stmt = conn.prepare(&sql).map_err(|e| db("store: table_info", e))?;
    let mut rows = stmt.query([]).map_err(|e| db("store: table_info", e))?;
    while let Some(row) = rows.next().map_err(|e| db("store: table_info", e))? {
        let nombre: String = row.get(1).map_err(|e| db("store: table_info", e))?;
        if nombre == col {
            return Ok(true);
        }
    }
    Ok(false)
}

/// ¿Hay tablas de usuario? (v0 recién creada = vacía → no hace falta .bak).
fn db_vacio(conn: &Connection) -> Res<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| db("store: sqlite_master", e))?;
    Ok(n == 0)
}

/// Copia `<path>` → `<path>.bak` (best-effort, antes del primer paso).
///
/// `wal_checkpoint(TRUNCATE)` primero: vuelca el WAL al archivo principal
/// para que la copia sea autónoma (sin depender de `-wal`).
fn backup(path: &Path, conn: &Connection) -> Res<()> {
    match conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
        r.get::<_, i64>(0)
    }) {
        Ok(0) => {}
        // Checkpoint ocupado: la copia puede quedarse coja en el margen del
        // WAL, pero la tx de migración protege el original igualmente.
        otro => tracing::warn!(
            target: "arca::arca-store::schema",
            ?otro,
            "checkpoint pre-backup no limpio (se copia igual)"
        ),
    }
    let mut bak = path.as_os_str().to_os_string();
    bak.push(".bak");
    std::fs::copy(path, &bak)
        .map(|_| ())
        .map_err(ArcaError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un paso corrido dos veces sobre la MISMA conexión no rompe
    /// (guardias de idempotencia de cada paso, no del runner).
    #[test]
    fn pasos_idempotentes_sola_conn() {
        let conn = Connection::open_in_memory().unwrap();
        migrate_v1(&conn).unwrap();
        migrate_v1(&conn).unwrap(); // IF NOT EXISTS
        migrate_v2(&conn).unwrap();
        migrate_v2(&conn).unwrap(); // guardia de columna + índice
        assert_eq!(user_version(&conn).unwrap(), 0); // el runner no corrió
    }
}
