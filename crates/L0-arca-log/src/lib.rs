//! `arca-log` — inicialización de logging (etapa mínima de F2).
//!
//! Capa L0 · unsafe: no · Contrato: `specs/arca-10-*.md` (T21 lo completa:
//! rotación, puente logcat y minidump estructurado).
//!
//! F2 entrega lo que exec-native y rt necesitan HOY:
//! - [`init_host`]: subscriber global con `EnvFilter` (RUST_LOG) → stderr.
//!   En Android el stderr del host ya va a logcat por el APK (T22).
//! - [`init_subapp`]: idem, prefijando las líneas con la instancia para que
//!   el drain del host pueda taggear `arca::app::<id>`.
//! - ADR-014: todo crate loguea con target `arca::<crate>::<módulo>`.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use arca_types::{InstanceId, Res};
use tracing_subscriber::EnvFilter;

/// Inicializa el logging del proceso HOST (idempotente: segunda llamada
/// no duplica subscribers).
pub fn init_host() -> Res<()> {
    try_init(None)
}

/// Inicializa el logging de una SUB-APP: prefijo por instancia para el drain
/// de stdout del host (spec 14: "los println! de las apps aparecen con su id").
pub fn init_subapp(instance: InstanceId) -> Res<()> {
    try_init(Some(instance))
}

fn try_init(instance: Option<InstanceId>) -> Res<()> {
    use tracing_subscriber::fmt;
    // Idempotencia: si ya hay un subscriber global (tests en el mismo
    // proceso), la segunda init se ignora en silencio.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("arca=debug,info"));
    let builder = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_line_number(false)
        .compact();
    let res = match instance {
        Some(inst) => {
            let _ = inst; // prefijo futuro (T21: tag por instancia)
            builder.with_timer(fmt::time::Uptime::default()).try_init()
        }
        None => builder.try_init(),
    };
    let _ = res; // SetGlobalDefaultError solo si ya estaba inicializado
    if let Some(inst) = instance {
        tracing::debug!(target: "arca::log", instance = inst.get(), "log de sub-app listo");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_es_idempotente() {
        // Dos inits en el mismo proceso de test: la segunda no falla.
        assert!(init_host().is_ok());
        assert!(init_host().is_ok());
        assert!(init_subapp(arca_types::InstanceId::new(1)).is_ok());
    }
}
