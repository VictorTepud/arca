//! `arca-launch` — el lanzador con seccomp (binario del APK, spec 14 §3).
//!
//! Ejecución: `arca-launch` es spawnado por el executor con los fds fijos:
//! ```text
//!  0/1/2  stdin=/dev/null · stdout/stderr → pipes del host (drain a arca-log)
//!  3      LaunchSpec serializada ([len u32][blob], pipe del host)
//!  4      socket ctl (par del host — AIPC)
//!  5      eventfd signal-in  (host → app: ticks)
//!  6      eventfd signal-out (app → host: FrameReady)
//! ```
//! Pasos (INVARIANTE spec 14 §4: el binario de la app JAMÁS corre sin seccomp):
//! 1. leer LaunchSpec del fd 3 (magic + crc validados);
//! 2. enumerar los fds a cerrar vía `/proc/self/fd` (ANTES del filtro);
//! 3. `build_profile` + `apply_profile` (NO_NEW_PRIVS + SECCOMP_MODE_FILTER);
//! 4. cerrar todo fd fuera de {0,1,2,4,5,6};
//! 5. `chdir(app_dir)`;
//! 6. `execve(app, argv, env mínimo)` — nunca retorna en éxito.
//!
//! El env mínimo: identidad de handshake (`ARCA_APP_ID`, `ARCA_INSTANCE`,
//! `ARCA_ARTIFACT`, `ARCA_VAULT`) + los pares `env_extra` de la spec.
//!
//! HERMÉTICO (v2): el env del hijo nace SOLO de la LaunchSpec — ya NO se
//! filtra el entorno del proceso host. Motivo: con tests e2e corriendo en
//! paralelo dentro de un mismo proceso, un `ARCA_PING_PANIC=1` global se
//! colaba a hijos de OTROS tests y rompía dos e2e según el interleaving
//! de la máquina. Las flags de test/config viajan por `LaunchSpec.env_extra`
//! (validadas: solo `ARCA_*`, sin tocar la identidad).
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use std::ffi::CString;
use std::io::Read as _;
use std::os::fd::{FromRawFd as _, RawFd};

use arca_exec_native::LaunchSpec;
use arca_permissions::{apply_profile, build_profile, CapabilitySet};
use arca_types::ArcaError;
use nix::unistd::{chdir, execve};

/// Fds fijos del protocolo de lanzamiento (spec 14 §3).
const FD_SPEC: RawFd = 3;
const FD_CTL: RawFd = 4;
const FD_SIG_IN: RawFd = 5;
const FD_SIG_OUT: RawFd = 6;

fn main() {
    if let Err(e) = run() {
        // Solo stderr queda disponible: el host lo drena al log.
        eprintln!("arca-launch: FATAL: {e}");
        std::process::exit(64); // el host lo distingue de un crash de la app
    }
}

fn run() -> Result<(), ArcaError> {
    // 1) spec por fd 3: [len u32][blob] — lectura EXACTA (sin depender de
    // EOF: este proceso hereda una copia del extremo de escritura del pipe).
    let mut f3 = unsafe { std::fs::File::from(std::os::fd::OwnedFd::from_raw_fd(FD_SPEC)) };
    let mut len4 = [0u8; 4];
    f3.read_exact(&mut len4)
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("fd3 len: {e}"))))?;
    let len = u32::from_le_bytes(len4) as usize;
    if len > (1 << 20) {
        return Err(ArcaError::InvalidFrame("launch spec: >1 MiB"));
    }
    let mut raw = vec![0u8; len];
    f3.read_exact(&mut raw)
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("fd3 blob: {e}"))))?;
    drop(f3); // el fd 3 ya no se necesita
    let spec = LaunchSpec::decode(&raw)?;

    // 2) enumerar fds a cerrar ANTES de seccomp (openat solo es legal ahora).
    let keep: [RawFd; 6] = [0, 1, 2, FD_CTL, FD_SIG_IN, FD_SIG_OUT];
    let to_close = list_fds()?;

    // 3) perfil + filtro (default-deny; invariantes de arca-permissions).
    let caps = CapabilitySet::from_bits(spec.caps_bits);
    let profile = build_profile(
        &caps,
        std::path::Path::new(&spec.app_dir),
        std::path::Path::new(&spec.vault_dir),
        arca_permissions::current_arch()?,
    )?;
    apply_profile(&profile)?;

    // 4) cierre de fds heredados del host (memfd/sockets/pipes del spawn).
    for fd in to_close {
        if !keep.contains(&fd) {
            // Invariante: ignorar EBADF (carreras benignas de dup/close).
            let _ = nix::unistd::close(fd);
        }
    }

    // 5) cwd = app dir (las apps ven rutas relativas a su sandbox).
    chdir(std::path::Path::new(&spec.app_dir)).map_err(|e| {
        ArcaError::Io(std::io::Error::other(format!(
            "chdir {}: {e}",
            spec.app_dir
        )))
    })?;

    // 6) execve con env mínimo.
    let app = CString::new(spec.app_path.as_bytes())
        .map_err(|_| ArcaError::Internal("launch: app_path con NUL"))?;
    let argv = [app.clone()];
    let env = build_env(&spec);
    // Invariante: execve reemplaza la imagen; con seccomp YA aplicado, el
    // binario de la app corre SIEMPRE filtrado (spec 07 §4).
    execve(&app, &argv, &env).map_err(|e| {
        ArcaError::Io(std::io::Error::other(format!(
            "execve {}: {e}",
            spec.app_path
        )))
    })?;
    unreachable!("execve no retorna en éxito")
}

/// Enumera /proc/self/fd (std usa openat+getdents64: legales ANTES del
/// filtro; seccomp los bloquea DESPUÉS — por eso enumeramos aquí).
fn list_fds() -> Result<Vec<RawFd>, ArcaError> {
    let dir = std::fs::read_dir("/proc/self/fd")
        .map_err(|e| ArcaError::Io(std::io::Error::other(format!("proc fd: {e}"))))?;
    let mut out = Vec::new();
    let self_dir = std::fs::read_link("/proc/self/fd")
        .ok()
        .and_then(|p| p.to_str().and_then(|s| s.parse::<RawFd>().ok()));
    for entry in dir {
        let entry =
            entry.map_err(|e| ArcaError::Io(std::io::Error::other(format!("getdents: {e}"))))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(fd) = name.parse::<RawFd>() {
            // El propio dirfd se auto-referencia: se cierra solo al salir.
            if Some(fd) != self_dir {
                out.push(fd);
            }
        }
    }
    Ok(out)
}

/// Env mínimo: identidad de handshake + `env_extra` de la spec.
/// Hermético: NADA del entorno de este proceso pasa al hijo (ver docs del
/// módulo — fix de las e2e flaky por contaminación de env entre tests).
fn build_env(spec: &LaunchSpec) -> Vec<CString> {
    let mut env = Vec::new();
    let fijas = [
        format!("ARCA_APP_ID={}", spec.app_id),
        format!("ARCA_INSTANCE={}", spec.instance),
        format!("ARCA_ARTIFACT={}", spec.artifact_hex()),
        format!("ARCA_VAULT={}", spec.vault_dir),
    ];
    for s in fijas {
        if let Ok(c) = CString::new(s) {
            env.push(c);
        }
    }
    for (k, v) in &spec.env_extra {
        if let Ok(c) = CString::new(format!("{k}={v}")) {
            env.push(c);
        }
    }
    env
}
