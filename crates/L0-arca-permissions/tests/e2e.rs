//! E2E con procesos reales (spec 07 §5): `fork` + seccomp en el hijo.
//!
//! Cada test hornea el perfil, hace `fork`, el hijo aplica el filtro y
//! ejecuta una syscall problema; el padre aserta sobre el estado de
//! `waitpid`:
//!
//! - syscall BLOQUEADA (`socket`/`openat`) → `KILL_PROCESS` → el hijo muere
//!   con `SIGSYS` (nunca vuelve con `EPERM`: el default-deny no se negocia).
//! - syscall PERMITIDA (`write` a un pipe) → el hijo escribe y sale con 0.
//!
//! El helper de fork vive aquí (y no en `src/`) porque `nix` es
//! dev-dependency del crate.

#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::Path;

use arca_permissions::{
    apply_profile, build_profile, current_arch, CapabilitySet, SandboxProfile, TargetArch,
};
use arca_types::Capability;
use nix::sys::signal::Signal;
use nix::sys::socket::{socket, AddressFamily, SockFlag, SockType};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{fork, pipe, write, ForkResult};

fn app() -> &'static Path {
    Path::new("/apps/demo")
}
fn vault() -> &'static Path {
    Path::new("/vault/demo")
}

/// Arch del host que corre los tests (x86_64 en dev/CI): el filtro se
/// aplica EN ESTE proceso-hijo, así que debe estar compilado para SU arch.
fn arch_local() -> TargetArch {
    current_arch().unwrap_or_else(|e| panic!("arch local: {e}"))
}

/// Los 5 perfiles de referencia (mismos sets que `tests/golden.rs`).
fn cinco_perfiles() -> Vec<(&'static str, CapabilitySet)> {
    vec![
        ("empty", CapabilitySet::empty()),
        (
            "net-client",
            CapabilitySet::from_iter([Capability::NetClient]),
        ),
        ("fs-vault", CapabilitySet::from_iter([Capability::FsVault])),
        (
            "net+vault",
            CapabilitySet::from_iter([
                Capability::NetClient,
                Capability::NetServer,
                Capability::FsVault,
            ]),
        ),
        (
            "todas",
            CapabilitySet::from_iter(Capability::all().iter().copied()),
        ),
    ]
}

/// Hace `fork`, el hijo aplica el perfil y corre `probe`. El probe debe
/// terminar SIEMPRE por `_exit` (o porque lo mata el filtro); si volviera,
/// el helper corta con el código 102 (bug de seguridad). Devuelve el estado
/// de waitpid del hijo.
fn fork_probe<F>(profile: &SandboxProfile, probe: F) -> WaitStatus
where
    F: FnOnce(),
{
    // SAFETY — invariante de fork en proceso multihilo: tras fork() el hijo
    // SOLO ejecuta código async-signal-safe antes de _exit: apply_profile
    // (prctl + seccomp(2), syscalls directas), write(2)/socket(2)/open(2) y
    // _exit(2). Nada de esto asigna memoria ni toma locks de Rust/libc, así
    // que no hay deadlock con los hilos del runner de tests en el momento
    // del fork. El padre no se filtra a sí mismo: espera con waitpid.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            if apply_profile(profile).is_err() {
                // SAFETY — invariante: _exit(2) es async-signal-safe y no
                // ejecuta handlers de salida; el código 101 señala "fallo al
                // instalar el filtro" al padre.
                unsafe { nix::libc::_exit(101) };
            }
            probe();
            // El probe VOLVIÓ sin morir ni llamar _exit: el filtro dejó pasar
            // algo que no debía. Código 102 para el padre.
            // SAFETY — invariante: idem _exit(101) (async-signal-safe).
            unsafe { nix::libc::_exit(102) }
        }
        Ok(ForkResult::Parent { child }) => {
            waitpid(child, None).unwrap_or_else(|e| panic!("waitpid: {e}"))
        }
        Err(e) => panic!("fork: {e}"),
    }
}

/// Probe bloqueada: crea un socket AF_INET. Con el filtro activo el proceso
/// muere con SIGSYS ANTES de que socket(2) devuelva.
fn probe_socket() {
    let _ = socket(
        AddressFamily::Inet,
        SockType::Stream,
        SockFlag::empty(),
        None,
    );
    // Solo llegamos aquí si el filtro NO mató: bug de seguridad.
    // SAFETY — invariante: ver fork_probe (exit async-signal-safe, código
    // 110 = "socket(2) sobrevivió al filtro").
    unsafe { nix::libc::_exit(110) }
}

/// Probe bloqueada: abre un archivo por path (openat). Nunca permitido en
/// v1: los archivos van por fds del broker.
fn probe_open() {
    let _ = nix::fcntl::open(
        "/etc/hostname",
        nix::fcntl::OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    );
    // SAFETY — invariante: ver fork_probe (código 111 = "openat sobrevivió").
    unsafe { nix::libc::_exit(111) }
}

/// Aserta que el hijo murió por SIGSYS (KILL_PROCESS del default-deny).
fn assert_sigsys(estado: &WaitStatus, ctx: &str) {
    assert!(
        matches!(estado, WaitStatus::Signaled(_, Signal::SIGSYS, _)),
        "{ctx}: esperaba muerte por SIGSYS, llegó {estado:?}"
    );
}

/// El test canónico de spec 07 §5: sin capabilities, `socket(AF_INET)`
/// muere con SIGSYS.
#[test]
fn seccomp_block_net() {
    let perfil = build_profile(&CapabilitySet::empty(), app(), vault(), arch_local())
        .unwrap_or_else(|e| panic!("{e}"));
    let estado = fork_probe(&perfil, probe_socket);
    assert_sigsys(&estado, "seccomp_block_net");
}

/// Con cada uno de los 5 perfiles de referencia, `socket(AF_INET)` muere
/// con SIGSYS — INCLUSO con net-client/net-server concedidas (patrón
/// "socket pasado": la capability compra fds del broker, no la syscall).
#[test]
fn seccomp_block_net_en_todos_los_perfiles() {
    for (nombre, caps) in cinco_perfiles() {
        let perfil = build_profile(&caps, app(), vault(), arch_local())
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));
        let estado = fork_probe(&perfil, probe_socket);
        assert_sigsys(&estado, &format!("perfil {nombre}"));
    }
}

/// Syscall permitida con cada perfil: `write(2)` a un pipe funciona y el
/// hijo sale limpio (exit 0) — el filtro base no rompe la E/S legítima.
#[test]
fn syscall_permitida_write_ok_en_todos_los_perfiles() {
    for (nombre, caps) in cinco_perfiles() {
        let perfil = build_profile(&caps, app(), vault(), arch_local())
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));

        // Pipe antes del fork: el hijo hereda el extremo de escritura.
        let (r, w) = pipe().unwrap_or_else(|e| panic!("{nombre}: pipe: {e}"));
        let w: OwnedFd = w;

        let estado = fork_probe(&perfil, move || {
            let n = write(&w, b"ok").unwrap_or(usize::MAX);
            // SAFETY — invariante: ver fork_probe (código 0 = write OK;
            // 112 = write falló o escribió menos bytes).
            unsafe {
                nix::libc::_exit(if n == 2 { 0 } else { 112 });
            }
        });

        assert!(
            matches!(estado, WaitStatus::Exited(_, 0)),
            "{nombre}: esperaba exit 0 tras write, llegó {estado:?}"
        );

        // El "ok" cruzó el pipe: write(2) de verdad funcionó.
        let mut buf = [0u8; 2];
        let mut f = std::fs::File::from(r);
        f.read_exact(&mut buf)
            .unwrap_or_else(|e| panic!("{nombre}: leer pipe: {e}"));
        assert_eq!(&buf, b"ok", "{nombre}: payload del pipe");
    }
}

/// Default-deny de verdad: `openat` (archivos por path) muere con SIGSYS
/// aunque la app tenga TODAS las capabilities (fs-vault incluida: la bóveda
/// la media el broker, no openat).
#[test]
fn default_deny_bloquea_openat() {
    for (nombre, caps) in cinco_perfiles() {
        let perfil = build_profile(&caps, app(), vault(), arch_local())
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));
        let estado = fork_probe(&perfil, probe_open);
        assert_sigsys(&estado, &format!("default_deny_openat {nombre}"));
    }
}
