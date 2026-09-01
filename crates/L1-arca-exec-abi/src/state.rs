//! Estados de ciclo de vida, causas de muerte, estado terminal y eventos
//! (spec 13 §3).

use std::path::PathBuf;

/// Fase del ciclo de vida de una instancia, vista por el host.
///
/// La transición la dirige el stream de eventos (ver la máquina descrita
/// en el docs del crate): `Hello` no muta estado, `Ready`/`Paused`/
/// `Resumed` sí, y `Dead` es terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    /// Spawn aceptado; handshake `Hello`→`Welcome`→`Ready` en curso.
    /// `launch` es síncrono hasta aquí — nada más.
    Spawning,
    /// El runtime mapeó la shm y está listo para `Attach`/ticks.
    Ready,
    /// Loop de frames activo (consume ticks, produce frames).
    Running,
    /// Congelado por el host (oclusión/freezer); sin ticks.
    Paused,
    /// Terminal: la instancia murió (ver [`DeathReason`]).
    Dead {
        /// Causa registrada de la muerte.
        reason: DeathReason,
    },
}

/// Por qué murió una instancia (lo reporta el deathWatch del executor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathReason {
    /// Terminó por las suyas con un exit code (`exit_group`).
    Exit {
        /// Código de salida observado (semántica waitpid).
        code: i32,
    },
    /// La mató una señal (SIGKILL del host, OOM, SIGSEGV, …).
    Signaled {
        /// Número de señal (estilo `WIFSIGNALED`).
        signal: i32,
    },
    /// El deathWatch perdió el rastro (pid reaparecido por otro, watcher
    /// caído): fail-closed, se asume muerto y se reporta así.
    Lost,
    /// El host lo mató deliberadamente (escalada de `shutdown`, watchdog,
    /// presión de recursos).
    KilledByHost,
}

/// Estado terminal observado de una instancia (estilo waitpid).
///
/// Convención de campos: si `signal` es `Some`, el proceso murió por señal
/// y `code` no aplica (vale 0); si es `None`, `code` es el exit code real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    /// Exit code (sin uso cuando hubo señal).
    pub code: i32,
    /// Señal que lo mató, si murió por señal.
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// ¿Terminó bien? Exit 0 sin señal. `Lost` nunca es success (code −1).
    #[must_use]
    pub const fn success(&self) -> bool {
        self.signal.is_none() && self.code == 0
    }
}

impl From<DeathReason> for ExitStatus {
    /// Conversión documentada (la usa `shutdown` cuando el Dead ya fue
    /// reportado y debe devolverse el estado terminal asociado):
    ///
    /// - `Exit { code }` → `code` sin señal.
    /// - `Signaled { s }` → señal `s` (code 0, sin uso).
    /// - `Lost` → `code -1` (estado desconocido, nunca success).
    /// - `KilledByHost` → señal 9 (SIGKILL es el mecanismo asumido).
    fn from(reason: DeathReason) -> Self {
        match reason {
            DeathReason::Exit { code } => Self { code, signal: None },
            DeathReason::Signaled { signal } => Self {
                code: 0,
                signal: Some(signal),
            },
            DeathReason::Lost => Self {
                code: -1,
                signal: None,
            },
            DeathReason::KilledByHost => Self {
                code: 0,
                signal: Some(9),
            },
        }
    }
}

/// Evento del stream de ciclo de vida/salud de una instancia
/// ([`AppHandle::on_state_change`](crate::AppHandle::on_state_change)).
///
/// Máquina (garantías del handle, ver docs del crate): `Spawned` es
/// SIEMPRE el primero y no se repite; `Dead` es one-shot y terminal (nada
/// se entrega después); `Unhealthy`/`FrameStalled` son salud, no ciclo —
/// no mutan [`AppState`](crate::AppState) (spec 13 §5, fila 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// Spawn aceptado — siempre el primer evento del stream (lo emite la
    /// propia construcción del handle, no el executor).
    Spawned,
    /// El proceso mandó `Hello` (handshake C→H, docs/04 §3). No muta
    /// estado: la instancia sigue `Spawning`.
    Hello,
    /// Runtime listo (mapeó shm; recibe `Attach` y empieza a correr).
    Ready,
    /// El host congeló el loop de frames (`Pause`).
    Paused,
    /// El host reanudó el loop (`Resume`).
    Resumed,
    /// Terminal: la instancia murió. Llega exactamente UNA vez y es el
    /// último evento (deathWatch, docs/04 §9; invariante spec 13 §4).
    Dead {
        /// Causa de la muerte.
        reason: DeathReason,
        /// Minidump escrito por el runtime antes de morir (si alcanzó).
        minidump: Option<PathBuf>,
    },
    /// Watchdog: pings sin respuesta (docs/04 §9). Salud, no ciclo.
    Unhealthy {
        /// ms desde el primer fallo de ping.
        since_ms: u64,
    },
    /// Frames estancados sin `Busy` (docs/04 §9). Salud, no ciclo.
    FrameStalled {
        /// Frames consecutivos sin actividad.
        frames: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Espejo de la convención documentada de `From<DeathReason>`: si esto
    /// se rompe, se rompió el contrato documentado, no el test.
    #[test]
    fn exit_status_desde_death_reason() {
        assert_eq!(
            ExitStatus::from(DeathReason::Exit { code: 0 }),
            ExitStatus {
                code: 0,
                signal: None
            }
        );
        assert_eq!(
            ExitStatus::from(DeathReason::Signaled { signal: 15 }),
            ExitStatus {
                code: 0,
                signal: Some(15)
            }
        );
        assert_eq!(
            ExitStatus::from(DeathReason::Lost),
            ExitStatus {
                code: -1,
                signal: None
            }
        );
        assert_eq!(
            ExitStatus::from(DeathReason::KilledByHost),
            ExitStatus {
                code: 0,
                signal: Some(9)
            }
        );

        assert!(ExitStatus::from(DeathReason::Exit { code: 0 }).success());
        assert!(!ExitStatus::from(DeathReason::Exit { code: 3 }).success());
        assert!(!ExitStatus::from(DeathReason::Signaled { signal: 9 }).success());
        assert!(!ExitStatus::from(DeathReason::Lost).success());
        assert!(!ExitStatus::from(DeathReason::KilledByHost).success());

        // Estados comparables y copiables (los asserts de test los usan).
        let st = AppState::Dead {
            reason: DeathReason::Lost,
        };
        assert_eq!(st, st);
        assert!(matches!(AppState::Spawning, AppState::Spawning));
    }
}
