//! Transacción RAII del store (spec 11 §4: toda operación multi-paso en UNA
//! transacción).
//!
//! # Diseño (concurrency)
//!
//! [`Tx`](Tx) retiene el `MutexGuard` de la conexión durante toda su vida:
//! mientras exista una `Tx`, NADIE más toca la conexión (escritor único).
//! El `BEGIN IMMEDIATE` toma el write-lock de SQLite de entrada (evita el
//! upgrade-deadlock de `BEGIN DEFERRED` + primer write). El lector
//! concurrente "por servicio" del launcher va por OTRA conexión en modo WAL
//! (spec 11 §4: WAL + single-writer) y no compite con este mutex.
//!
//! Crash pre-commit: `Drop` → `ROLLBACK` best-effort — el sweep del
//! instalador y el orden "archivos primero, commit DB al final" (spec 11 §4)
//! hacen el resto.
//!
//! Llamar [`Store::begin`](crate::Store::begin) con una `Tx` viva en el
//! MISMO hilo bloquearía el mutex (deadlock): es bug del llamador
//! ("database is locked" = bug de arquitectura, spec 11 §5) y la API lo hace
//! antinatural (los métodos toman `&mut Tx`, no `begin` anidado).

use std::sync::MutexGuard;

use arca_types::{ArcaError, Res};
use rusqlite::Connection;

/// Mapa sqlite → [`ArcaError`] de ESTE módulo.
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(target: "arca::arca-store::tx", ctx, error = %e, "fallo sqlite");
    ArcaError::Internal(ctx)
}

/// Estado de una [`Tx`]: viva o ya cerrada (commit/rollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Estado {
    /// Transacción abierta.
    Activa,
    /// Ya cometida o revertida: `Drop` no hace nada.
    Cerrada,
}

/// Transacción de escritura del store (RAII).
///
/// Se obtiene con [`Store::begin`](crate::Store::begin) y se cierra con
/// [`Tx::commit`] o [`Tx::rollback`]; si se deja caer viva, `Drop` revierte
/// (simula crash pre-commit).
#[derive(Debug)]
pub struct Tx<'a> {
    /// Guard exclusivo de la conexión (single-writer, ver docs del módulo).
    pub(crate) guard: MutexGuard<'a, Connection>,
    estado: Estado,
}

impl Tx<'_> {
    /// Abre la transacción tomando el guard (solo [`Store::begin`]).
    pub(crate) fn begin(guard: MutexGuard<'_, Connection>) -> Res<Tx<'_>> {
        // IMMEDIATE: write-lock YA (single-writer del host; sin upgrade).
        guard
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db("store: BEGIN de transacción", e))?;
        Ok(Tx {
            guard,
            estado: Estado::Activa,
        })
    }

    /// Confirma (COMMIT). Consume la `Tx`: no se puede seguir usando.
    ///
    /// Si el COMMIT falla (disco lleno, etc.), la transacción queda abierta
    /// y `Drop` intentará el rollback — el estado queda el de antes.
    pub fn commit(mut self) -> Res<()> {
        let r = self
            .guard
            .execute_batch("COMMIT")
            .map_err(|e| db("store: COMMIT de transacción", e));
        // Solo marcamos cerrada si el COMMIT entró: en error, Drop revierte.
        if r.is_ok() {
            self.estado = Estado::Cerrada;
        }
        r
    }

    /// Revierte explícitamente (ROLLBACK). Consume la `Tx`.
    pub fn rollback(mut self) -> Res<()> {
        let r = self
            .guard
            .execute_batch("ROLLBACK")
            .map_err(|e| db("store: ROLLBACK de transacción", e));
        self.estado = Estado::Cerrada;
        r
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if self.estado == Estado::Activa {
            // Crash pre-commit / error propagado con `?`: deshacer y loguear
            // (best-effort: la conexión puede estar rota, no pánico aquí).
            if let Err(e) = self.guard.execute_batch("ROLLBACK") {
                tracing::warn!(
                    target: "arca::arca-store::tx",
                    error = %e,
                    "ROLLBACK en Drop falló"
                );
            }
        }
    }
}
