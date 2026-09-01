//! [`apply_profile`] — instalación del filtro en el hilo actual.
//!
//! # Contrato (spec 07 §4)
//!
//! `NO_NEW_PRIVS` + `SECCOMP_MODE_FILTER`: [`seccompiler::apply_filter`]
//! fija `PR_SET_NO_NEW_PRIVS` y carga el filtro con la syscall `seccomp(2)`,
//! **en el hilo que llama**. El invariante de Arca: `arca-launch` la invoca
//! en el hijo tras `fork`, **antes de `exec`** del binario de la sub-app,
//! para que el proceso no pueda escapar por arranque.
//!
//! # Frontera unsafe
//!
//! Este crate NO añade bloques `unsafe` propios: todo el FFI
//! (`prctl`/`seccomp`) vive en seccompiler (upstream auditado). La spec
//! marca el crate "unsafe: sí" por el dominio (kernel BPF), satisfecho vía
//! dependencia.
//!
//! # Fallo-cerrado ante arch distinta
//!
//! El programa BPF embebe un chequeo de arquitectura: si el perfil se
//! compiló para aarch64 y se aplica en x86_64 (o viceversa), la PRIMERA
//! syscall muere con `KILL_PROCESS`. Es la garantía de que un perfil
//! cruzado nunca ejecuta nada sin filtro: falla cerrado, no abierto.

use crate::profile::SandboxProfile;
use arca_types::{ArcaError, Res};

/// Instala el filtro seccomp del perfil en el **hilo actual**.
///
/// - Idempotente a nivel kernel (los filtros se apilan; siempre añade
///   restricción, nunca quita).
/// - Aplica al HIJO en el flujo de `arca-launch` (fork → apply → exec);
///   el host nunca se filtra a sí mismo.
/// - Tras una llamada exitosa, cualquier syscall fuera de
///   [`crate::BASE_SYSCALLS`] mata el proceso (`SIGSYS`).
///
/// # Errores
///
/// [`ArcaError::Internal`] si el kernel rechaza el filtro (programa vacío,
/// `seccomp(2)` fallida…); el detalle dinámico va al log con target
/// `arca::permissions::apply`.
pub fn apply_profile(p: &SandboxProfile) -> Res<()> {
    seccompiler::apply_filter(&p.seccomp).map_err(|e| {
        tracing::warn!(target: "arca::permissions::apply", error = %e, "fallo al instalar el filtro seccomp");
        ArcaError::Internal("permissions: instalar filtro seccomp")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capset::CapabilitySet;
    use crate::profile::build_profile;
    use seccompiler::TargetArch;
    use std::path::Path;

    /// Un programa vacío se rechaza ANTES de tocar prctl (seccompiler
    /// valida is_empty): se puede comprobar in-process sin peligro.
    #[test]
    fn apply_profile_rechaza_programa_vacio() {
        let p = SandboxProfile {
            seccomp: Vec::new(),
            allowed_paths: Vec::new(),
            net_fds: crate::profile::NetPolicy::NoNet,
            landlock: None,
        };
        assert!(matches!(apply_profile(&p), Err(ArcaError::Internal(_))));
    }

    /// Un perfil válido construido para otra arch se aplica SIN error aquí
    /// (el chequeo de arch viaja DENTRO del BPF: muerte en la 1ª syscall,
    /// no error de apply). Esto documenta la frontera de responsabilidad.
    #[test]
    fn apply_profile_no_valida_la_arch_aqui() {
        let arm = build_profile(
            &CapabilitySet::empty(),
            Path::new("/apps/demo"),
            Path::new("/vault/demo"),
            TargetArch::aarch64,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // La signatura es infalible en este aspecto; solo afirmamos que el
        // tipo compone y el programa no está vacío.
        assert!(!arm.seccomp.is_empty());
    }
}
