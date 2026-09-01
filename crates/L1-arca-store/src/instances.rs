//! Histórico de instancias (runtime, spec 11 §3).
//!
//! El host-core registra cada spawn ([`Store::register_instance`]) y su fin
//! ([`Store::finish_instance`]): diagnósticos "app se cerró" + respawn
//! (docs/10 §9). Ambas son operaciones de UN statement → auto-commit
//! (atómicas por sí mismas; no necesitan `Tx` del llamador).

use arca_types::{ArcaError, InstanceId, Res};
use rusqlite::params;

use crate::model::{InstanceRecord, Outcome, UnixMs};
use crate::Store;

/// Mapa sqlite → [`ArcaError`] de ESTE módulo.
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(
        target: "arca::arca-store::instances",
        ctx,
        error = %e,
        "fallo sqlite"
    );
    ArcaError::Internal(ctx)
}

/// `InstanceId` (u64 del host) → i64 de SQL. El host es contador monotónico:
/// > i64::MAX solo sería un bug de asignación (se enmienda, no se trunca).
fn iid(i: InstanceId) -> Res<i64> {
    i64::try_from(i.get()).map_err(|_| {
        tracing::error!(
            target: "arca::arca-store::instances",
            id = i.get(),
            "InstanceId fuera de rango SQL"
        );
        ArcaError::Internal("store: InstanceId fuera de rango SQL")
    })
}

impl Store {
    /// Registra el spawn de una instancia (histórico de ejecución).
    ///
    /// La FK a `apps` exige que la app esté instalada (orden del flujo:
    /// install → spawn). `instance_id` duplicado → error de constraint
    /// (bug del asignador: el host nunca reusa ids vivos).
    pub fn register_instance(&self, i: &InstanceRecord) -> Res<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO instances (instance_id, app_id, version, started_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                iid(i.instance_id)?,
                i.app_id.as_str(),
                i.version.as_str(),
                i.started_at.get()
            ],
        )
        .map_err(|e| db("store: registro de instancia", e))?;
        Ok(())
    }

    /// Cierra una instancia: exit/crash/killed (UPDATE atómico).
    ///
    /// `exited_at IS NULL` en el WHERE: si la instancia no existe o ya fue
    /// finalizada (doble shutdown — docs/14 §5), el UPDATE no toca nada y
    /// se devuelve error interno con detalle al log (bug del llamador, no
    /// del registro).
    pub fn finish_instance(&self, id: InstanceId, outcome: Outcome) -> Res<()> {
        let conn = self.lock()?;
        let n = conn
            .execute(
                "UPDATE instances SET exited_at = ?1, outcome = ?2 \
                 WHERE instance_id = ?3 AND exited_at IS NULL",
                params![UnixMs::now().get(), outcome.as_sql(), iid(id)?],
            )
            .map_err(|e| db("store: fin de instancia", e))?;
        if n == 0 {
            tracing::warn!(
                target: "arca::arca-store::instances",
                id = id.get(),
                "finish de instancia inexistente o ya finalizada"
            );
            return Err(ArcaError::Internal(
                "store: instancia desconocida o ya finalizada",
            ));
        }
        Ok(())
    }
}
