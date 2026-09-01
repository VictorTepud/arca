//! `arca-store` — registro persistente en SQLite (spec 11, ADR-011).
//!
//! Capa L1 · unsafe: **no** · Grafo: `graphs/installer.mmd` (dominio store).
//!
//! Registro de: apps instaladas (versiones), capabilities concedidas,
//! instancias (histórico de ejecución) y auditoría de uso de servicios.
//! **Sin lógica de negocio**: quien decide es el installer (`arca-installer`)
//! o el broker (`arca-svc-broker`); aquí solo vive persistencia.
//!
//! # Concurrency (decisión documentada)
//!
//! `rusqlite::Connection` no es `Sync`, y el modelo del ecosistema es
//! **single-writer** (solo host-core escribe — spec 11 §4/§5). Decisión:
//!
//! - `Store` envuelve la conexión en un `Mutex` → `Store: Send + Sync`;
//!   cada operación toma el guard brevemente y lo suelta (las operaciones
//!   de un solo statement ni siquiera abren transacción explícita).
//! - Las operaciones multi-paso van en [`Tx`], que RETIENE el guard: writer
//!   único garantizado por diseño, no por disciplina del llamador.
//! - WAL activo (ADR-011): el launcher puede abrir su propia conexión de
//!   SOLO lectura en otro hilo/servicio sin bloquear al writer.
//! - `busy_timeout = 5 s`: red de seguridad si algún día aparece un segundo
//!   escritor (bug de arquitectura — mejor error diferido que deadlock).
//!
//! # Migraciones (spec 11 §4)
//!
//! [`Store::open`] migra automáticamente con `PRAGMA user_version`
//! (v1 → v2, cada paso en su transacción; detalle en `src/schema.rs`).
//! Antes de migrar una db con datos se escribe `<db>.bak`. Una db de
//! versión futura se rechaza (no hay downgrade silencioso).
//!
//! # Orden con el filesystem (invariante del installer)
//!
//! El llamador coordina: **archivos primero, commit de db al final**; si el
//! proceso muere pre-commit, el rollback de [`Tx`] deja la db como estaba y
//! el `sweep()` del instalador limpia los restos (docs/06 §7).
//!
//! # Ejemplo
//!
//! ```no_run
//! use std::path::Path;
//! use arca_store::{Filter, Store};
//!
//! let store = Store::open(Path::new("/data/store/registry.db"))?;
//! let apps = store.list_apps(Filter::all())?; // ya migrada a la última versión
//! let _total = apps.len(); // el launcher pinta la lista
//! # Ok::<(), arca_types::ArcaError>(())
//! ```
//!
//! # Enmiendas al contrato de spec 11 (documentadas)
//!
//! - `DateTime` del contrato → [`UnixMs`] (arca-types no expone reloj de
//!   pared y está cerrado; ver `src/model.rs`).
//! - `CapabilitySet` se define localmente (bitset): el canónico vivirá en
//!   `arca-permissions` (T14), no permitido como dependencia aquí.
//! - `Store::begin` se añade para construir la [`Tx`] que el contrato usa
//!   en sus firmas (upsert/delete/grant/revoke la reciben).
//! - `audit_batch` se añade para el patrón queue+flush de spec 11 §5.
//! - Dep extra `tracing` (ADR-014: logging obligatorio; el detalle dinámico
//!   de errores sqlite no cabe en `ArcaError` de contexto estático).

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

mod apps;
mod audit;
mod instances;
mod model;
mod schema;
mod tx;

pub use model::{
    AppRecord, AuditEvent, CapabilitySet, Filter, InstallSource, InstanceRecord, Outcome, UnixMs,
};
pub use schema::SCHEMA_VERSION;
pub use tx::Tx;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use arca_types::{ArcaError, Res};
use rusqlite::Connection;

/// Mapa sqlite → [`ArcaError`] a nivel de Store (open/begin).
///
/// Política del ecosistema (spec 01 §5): contexto ESTÁTICO en el error
/// (nada dinámico hacia el host); el detalle dinámico solo al log con
/// `tracing` (ADR-014).
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(target: "arca::arca-store", ctx, error = %e, "fallo sqlite");
    ArcaError::Internal(ctx)
}

/// Registro persistente de apps/permisos/instancias/auditoría.
///
/// Una instancia de `Store` por proceso (host-core); ver "Concurrency" en el
/// docs del crate. Los métodos de un statement son auto-commit (atómicos por
/// sí mismos); los multi-paso exigen [`Tx`] explícita.
#[derive(Debug)]
pub struct Store {
    /// Conexión única tras mutex (single-writer; ver docs del crate).
    conn: Mutex<Connection>,
}

impl Store {
    /// Abre (crea si no existe) la db y la lleva a [`SCHEMA_VERSION`].
    ///
    /// - `journal_mode=WAL` (persistente en el archivo), `synchronous=NORMAL`
    ///   (presupuesto de fsync móvil), `foreign_keys=ON` (cascadas de
    ///   uninstall), `busy_timeout=5000` (red de seguridad).
    /// - Migraciones automáticas v(n)→[`SCHEMA_VERSION`] con `.bak` previo
    ///   si hay datos (ver `src/schema.rs`).
    /// - Versión futura → error, no downgrade.
    pub fn open(path: &Path) -> Res<Self> {
        let conn = Connection::open(path).map_err(|e| db("store: abrir registry.db", e))?;
        // WAL devuelve una fila con el modo resultante ("wal"; ":memory:"
        // responde "memory" y no es error — solo menos concurrente).
        let modo: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(|e| db("store: activar WAL", e))?;
        if modo != "wal" {
            tracing::warn!(
                target: "arca::arca-store",
                modo,
                "journal_mode distinto de WAL (¿db en memoria?)"
            );
        }
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| db("store: pragmas de conexión", e))?;
        schema::migrate(&conn, path)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Abre una transacción de escritura (single-writer; ver [`Tx`]).
    ///
    /// Mientras la `Tx` viva, ninguna otra operación del store avanza en
    /// este hilo (el guard está retenido).
    pub fn begin(&self) -> Res<Tx<'_>> {
        Tx::begin(self.lock()?)
    }

    /// Guard de la conexión (interno; jamás anidar con otra llamada que
    /// también lo tome — sería deadlock en el mismo hilo).
    pub(crate) fn lock(&self) -> Res<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|poisoned| {
            // Un writer hizo panic con el lock tomado: la invariante
            // single-writer se rompió; preferimos fallar ruidoso a
            // continuar con estado de conexión desconocido.
            tracing::error!(
                target: "arca::arca-store",
                error = %poisoned,
                "mutex de la conexión envenenado (panic de un writer)"
            );
            ArcaError::Internal("store: mutex de conexión envenenado")
        })
    }
}
