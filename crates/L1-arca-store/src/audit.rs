//! Auditoría de uso de servicios (net/clipboard/notify) — spec 11 §3.
//!
//! Append-only: lo escribe el broker cuando una app EJERCE una capability;
//! lo lee el panel de diagnóstico por app/tiempo. Sin FK a `apps`: el
//! historial sobrevive al uninstall (evidencia de seguridad).
//!
//! Rendimiento (spec 11 §5 "auditoría lenta"): un `audit()` por evento paga
//! un commit/fsync cada vez; para ráfagas está [`Store::audit_batch`] (UNA
//! transacción, statement preparado 1×) — el broker acumula en cola y hace
//! flush periódico.

use arca_types::{AppId, ArcaError, Capability, Res};
use rusqlite::{params, Connection};

use crate::model::{AuditEvent, UnixMs};
use crate::Store;

/// Mapa sqlite → [`ArcaError`] de ESTE módulo.
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(target: "arca::arca-store::audit", ctx, error = %e, "fallo sqlite");
    ArcaError::Internal(ctx)
}

/// INSERT de un evento (statement compartido por audit/audit_batch).
const INSERT_AUDIT: &str = "INSERT INTO audit_log (app_id, cap, ts, detail) \
     VALUES (?1, ?2, ?3, ?4)";

impl Store {
    /// Añade un evento de auditoría (un INSERT auto-commit).
    ///
    /// Para ráfagas usar [`Store::audit_batch`].
    pub fn audit(&self, e: &AuditEvent) -> Res<()> {
        let conn = self.lock()?;
        conn.execute(
            INSERT_AUDIT,
            params![
                e.app_id.as_str(),
                e.cap.as_str(),
                e.ts.get(),
                e.detail.as_str()
            ],
        )
        .map_err(|e| db("store: append de audit", e))?;
        Ok(())
    }

    /// Append masivo en UNA transacción (statement preparado una sola vez).
    ///
    /// NOTA(agent): extensión del contrato de spec 11 §3 para el patrón
    /// "queue + flush cada 500 ms" de §5 — el api single `audit` no puede
    /// cumplir el presupuesto de 10k eventos en 100 ms pagando commit por
    /// evento.
    pub fn audit_batch(&self, eventos: &[AuditEvent]) -> Res<()> {
        if eventos.is_empty() {
            return Ok(());
        }
        let conn = self.lock()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db("store: BEGIN de audit_batch", e))?;
        match insertar_batch(&conn, eventos) {
            Ok(()) => conn
                .execute_batch("COMMIT")
                .map_err(|e| db("store: COMMIT de audit_batch", e)),
            Err(e) => {
                if let Err(e2) = conn.execute_batch("ROLLBACK") {
                    tracing::warn!(
                        target: "arca::arca-store::audit",
                        error = %e2,
                        "ROLLBACK de audit_batch falló"
                    );
                }
                Err(e)
            }
        }
    }

    /// Eventos de una app desde `since` (inclusive), por tiempo ascendente.
    ///
    /// Empate de `ts` → desempate por `id` de inserción (orden real).
    pub fn query_audit(&self, id: &AppId, since: UnixMs) -> Res<Vec<AuditEvent>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT app_id, cap, ts, detail FROM audit_log \
                 WHERE app_id = ?1 AND ts >= ?2 ORDER BY ts ASC, id ASC",
            )
            .map_err(|e| db("store: preparar query_audit", e))?;
        let filas = stmt
            .query_map(params![id.as_str(), since.get()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| db("store: query_audit", e))?;
        let mut out = Vec::new();
        for f in filas {
            let (app_s, cap_s, ts, detail) = f.map_err(|e| db("store: query_audit (fila)", e))?;
            let app_id = AppId::new(&app_s)?;
            let cap = Capability::from_name(&cap_s).ok_or_else(|| {
                tracing::error!(
                    target: "arca::arca-store::audit",
                    cap = %cap_s,
                    "capability desconocida en audit_log"
                );
                ArcaError::Internal("store: capability corrupta en audit_log")
            })?;
            out.push(AuditEvent {
                app_id,
                cap,
                ts: UnixMs::from_millis(ts),
                detail,
            });
        }
        Ok(out)
    }
}

/// Cuerpo transaccionado de [`Store::audit_batch`].
fn insertar_batch(conn: &Connection, eventos: &[AuditEvent]) -> Res<()> {
    let mut stmt = conn
        .prepare(INSERT_AUDIT)
        .map_err(|e| db("store: preparar audit_batch", e))?;
    for ev in eventos {
        stmt.execute(params![
            ev.app_id.as_str(),
            ev.cap.as_str(),
            ev.ts.get(),
            ev.detail.as_str()
        ])
        .map_err(|e| db("store: insert de audit_batch", e))?;
    }
    Ok(())
}
