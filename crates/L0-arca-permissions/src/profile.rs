//! [`SandboxProfile`] y [`build_profile`]: capabilities → filtro seccomp-BPF.
//!
//! # Modelo de seguridad (spec 07 §4)
//!
//! - **Default-deny**: [`BASE_SYSCALLS`] es la ÚNICA lista permitida; toda
//!   syscall fuera de ella dispara `SECCOMP_RET_KILL_PROCESS` (muerte del
//!   proceso, sin `errno` jugable). `openat`/`open` NUNCA están en la lista:
//!   en v1 los archivos van por broker/vault sobre fds heredados.
//! - **Patrón "socket pasado"**: `net-client`/`net-server` NO añaden ninguna
//!   syscall de red al filtro. El broker entrega fds YA conectados y la app
//!   solo hace `send`/`recv`/`getsockopt` sobre ellos (ya en la base).
//! - `fs-vault` añade `vault_dir` a [`SandboxProfile::allowed_paths`]
//!   (informativo para el broker en v1; sin Landlock).
//! - **Landlock (v1)**: stub honesto — [`SandboxProfile::landlock`] es
//!   SIEMPRE `None` (ver [`LandlockRules`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use arca_types::{ArcaError, Capability, Digest, Res};
use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

use crate::capset::CapabilitySet;
use crate::syscalls::SyscallTable;

/// Lista blanca base (default-deny): las ÚNICAS syscalls que el filtro
/// permite, todas incondicionalmente.
///
/// Justificación por grupo:
/// - E/S sobre fds heredados: `read`/`write`/`readv`/`writev`/`close`/
///   `fstat`/`fcntl`/`lseek`/`dup`/`dup2`/`dup3`/`getdents64`/
///   `readlink(at)`/`ioctl`.
/// - Red SOLO sobre fds ya conectados (patrón "socket pasado"):
///   `sendto`/`recvfrom`/`sendmsg`/`recvmsg`/`shutdown`/`get(set)sockopt`/
///   `getsockname`/`getpeername`. Sin `socket`/`connect`/`bind`/`listen`.
/// - Memoria y runtime: `brk`/`mmap`/`munmap`/`mprotect`/`madvise`/
///   `mremap`/`futex`/`rseq`/`set_robust_list`.
/// - Reloj/swap de contexto: `nanosleep`/`clock_*`/`sched_yield`/`poll`-family/
///   `epoll`-family (sin `epoll_create` legacy: va `epoll_create1`).
/// - Señales y ciclo de vida: `rt_sigaction`/`rt_sigprocmask`/
///   `rt_sigreturn`/`sigaltstack`/`tgkill`/`exit`/`exit_group`/`getpid`/
///   `gettid`/`get{e}{uid,gid}`.
/// - Misc mínima: `getrandom` (allocators), `prctl` (incondicional en v1:
///   `NO_NEW_PRIVS` ya está fijado y solo permite restringir más).
///
/// **Nada de**: `openat`/`open` (archivos vía broker), `socket`/`connect`/
/// `bind`/`listen`/`accept` (fds pasados), `execve` (arranque en v1 lo
/// resuelve el launcher antes de aplicar), `statx`/`newfstatat` (el broker
/// resuelve paths), `fork`/`clone` (una sub-app no cría procesos).
pub const BASE_SYSCALLS: &[&str] = &[
    // E/S sobre fds heredados
    "read",
    "write",
    "readv",
    "writev",
    "close",
    "fstat",
    "fcntl",
    "lseek",
    "dup",
    "dup2",
    "dup3",
    "getdents64",
    "readlink",
    "readlinkat",
    "ioctl",
    // Red solo sobre fds YA conectados (socket pasado)
    "recvfrom",
    "sendto",
    "sendmsg",
    "recvmsg",
    "getsockopt",
    "setsockopt",
    "shutdown",
    "getsockname",
    "getpeername",
    // Memoria y runtime
    "brk",
    "mmap",
    "munmap",
    "mprotect",
    "madvise",
    "mremap",
    "futex",
    "rseq",
    "set_robust_list",
    // Reloj y espera
    "nanosleep",
    "clock_nanosleep",
    "clock_gettime",
    "clock_getres",
    "sched_yield",
    "epoll_create1",
    "epoll_ctl",
    "epoll_wait",
    "epoll_pwait",
    "poll",
    "ppoll",
    "pselect6",
    // Arranque (arca-launch aplica seccomp ANTES de execve: spec 07 §4)
    "chdir",
    "execve",
    // x86_64: TLS del runtime estático (musl/glibc llaman arch_prctl en init)
    "arch_prctl",
    "set_tid_address",
    // Señales y ciclo de vida
    "exit",
    "exit_group",
    "getpid",
    "gettid",
    "getuid",
    "geteuid",
    "getgid",
    "getegid",
    "rt_sigprocmask",
    "rt_sigaction",
    "rt_sigreturn",
    "sigaltstack",
    "tgkill",
    "prctl",
    // Entropía para allocators
    "getrandom",
];

/// Política de fds de red que el broker puede conceder a la sub-app.
///
/// En NINGÚN caso la app crea sockets propios: el filtro base bloquea
/// `socket(2)` con `KILL_PROCESS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetPolicy {
    /// Sin red: el broker no entrega fds de red (capacidad `net-*` no
    /// concedida).
    NoNet,
    /// El broker puede entregar sockets YA conectados/en escucha como fds
    /// heredados (requiere `net-client` o `net-server`).
    BrokerSockets,
}

/// Placeholder de reglas Landlock (v1: SIN Landlock).
///
/// Landlock (kernel ≥ 5.28) reforzará `allowed_paths` con el LSM del kernel
/// en v2. El tipo existe para que la firma de [`SandboxProfile`] no cambie
/// entonces; el campo privado impide construirlo desde fuera.
///
/// Invariante v1: [`SandboxProfile::landlock`] es **siempre `None`** — un
/// stub honesto, no una promesa vacía.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockRules {
    /// V2: reglas de acceso por path (read/write/execute por jerarquía).
    _v2: (),
}

/// Sandbox completo de una sub-app: filtro seccomp + concesiones del host.
///
/// Construido SIEMPRE con [`build_profile`] (determinista: misma entrada →
/// mismo BPF; golden test del programa generado en `tests/golden.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProfile {
    /// Programa seccomp-BPF compilado para la `arch` dada (default-deny:
    /// `SECCOMP_RET_KILL_PROCESS` para todo lo fuera de [`BASE_SYSCALLS`]).
    pub seccomp: BpfProgram,
    /// Paths que el broker puede mediar para la app: `app_dir` (sus propios
    /// assets, siempre) + `vault_dir` si concede `fs-vault`.
    pub allowed_paths: Vec<PathBuf>,
    /// Política de fds de red concedidos al broker (patrón "socket pasado").
    pub net_fds: NetPolicy,
    /// Landlock: **siempre `None` en v1** (ver [`LandlockRules`]).
    pub landlock: Option<LandlockRules>,
}

/// Traduce capabilities a un [`SandboxProfile`] concreto.
///
/// **Puro y determinista**: no toca el sistema, misma entrada → mismo BPF
/// (el golden test de `tests/golden.rs` fija el hash). Las capabilities
/// `net-*`/`fs-vault`/**no** amplían la lista de syscalls: solo amplían
/// concesiones de fds/servicios que el host entrega (invariante spec 07 §4).
///
/// # Parámetros
///
/// - `caps` — capabilities concedidas (decisión del instalador).
/// - `app_dir` — directorio de la app instalada (va a `allowed_paths`:
///   el broker sirve sus assets; la app no puede abrirlos por sí misma).
/// - `vault_dir` — bóveda de la app; solo se añade con `fs-vault`.
/// - `arch` — **arquitectura del DISPOSITIVO** (desviación documentada del
///   contrato de spec 07, que no la incluía): el BPF de aarch64 se GENERA en
///   PC y se APLICA en el teléfono. `riscv64` se rechaza (v1 no trae tabla).
///
/// # Errores
///
/// - [`ArcaError::Internal`] si `arch` no tiene tabla de syscalls en v1 o
///   si seccompiler rechaza el filtro (ambos casos "imposibles" salvo bug).
pub fn build_profile(
    caps: &CapabilitySet,
    app_dir: &Path,
    vault_dir: &Path,
    arch: TargetArch,
) -> Res<SandboxProfile> {
    if !matches!(arch, TargetArch::x86_64 | TargetArch::aarch64) {
        tracing::warn!(target: "arca::permissions::profile", ?arch, "arch sin tabla de syscalls en v1");
        return Err(ArcaError::Internal(
            "permissions: arch sin tabla de syscalls en v1",
        ));
    }

    // Default-deny: solo BASE_SYSCALLS en el mapa, con cadena VACÍA
    // (= permitir la syscall incondicionalmente). Las ausentes en la tabla
    // del arch (p. ej. `dup2` en aarch64) se omiten con warn (portabilidad).
    let table = SyscallTable::new(arch);
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    for name in BASE_SYSCALLS {
        match table.get_syscall_nr(name) {
            Some(nr) => {
                rules.insert(nr, Vec::new());
            }
            None => {
                tracing::warn!(
                    target: "arca::permissions::profile",
                    syscall = name,
                    ?arch,
                    "syscall ausente en la tabla del arch; se omite"
                );
            }
        }
    }

    // mismatch_action (default-deny) = KillProcess; match_action = Allow.
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        arch,
    )
    .map_err(|e| secc("permissions: compilar filtro", e))?;
    let seccomp: BpfProgram = filter
        .try_into()
        .map_err(|e| secc("permissions: generar BPF", e))?;

    // Concesiones de fds/servicios (NUNCA syscalls nuevas).
    let net_fds = if caps.has(Capability::NetClient) || caps.has(Capability::NetServer) {
        NetPolicy::BrokerSockets
    } else {
        NetPolicy::NoNet
    };
    let mut allowed_paths = vec![app_dir.to_path_buf()];
    if caps.has(Capability::FsVault) {
        allowed_paths.push(vault_dir.to_path_buf());
    }

    Ok(SandboxProfile {
        seccomp,
        allowed_paths,
        net_fds,
        // Invariante v1: Landlock es stub; SIEMPRE None.
        landlock: None,
    })
}

/// Arquitectura del proceso que corre ESTE código (para `apply_profile`
/// en dev/CI x86_64 y en el teléfono aarch64).
///
/// Úsala cuando el perfil se va a aplicar EN ESTE host; para generar el BPF
/// de otro dispositivo usa la `arch` de ese dispositivo.
pub fn current_arch() -> Res<TargetArch> {
    std::env::consts::ARCH.try_into().map_err(|e| {
        tracing::warn!(target: "arca::permissions::profile", error = %e, "arch local sin soporte seccomp");
        ArcaError::Internal("permissions: arch local sin soporte seccomp")
    })
}

/// Huella blake3 estable del programa BPF (8 bytes por instrucción,
/// little-endian: `code:u16, jt:u8, jf:u8, k:u32`).
///
/// Para golden tests, logs de diagnóstico y detectar cambios del filtro
/// entre versiones del host. `sock_filter` es `#[repr(C)]` con campos
/// públicos: la serialización es pura y no requiere leer memoria cruda.
#[must_use]
pub fn bpf_digest(p: &BpfProgram) -> Digest {
    let mut bytes = Vec::with_capacity(p.len() * 8);
    for f in p {
        bytes.extend_from_slice(&f.code.to_le_bytes());
        bytes.push(f.jt);
        bytes.push(f.jf);
        bytes.extend_from_slice(&f.k.to_le_bytes());
    }
    Digest::of(&bytes)
}

/// Mapa seccompiler → [`ArcaError`]: contexto estático en el error,
/// detalle dinámico al log (política spec 01 §5 / ADR-014).
fn secc(ctx: &'static str, e: impl std::fmt::Display) -> ArcaError {
    tracing::warn!(target: "arca::permissions::profile", error = %e, "fallo de seccompiler");
    ArcaError::Internal(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> &'static Path {
        Path::new("/apps/demo")
    }
    fn vault() -> &'static Path {
        Path::new("/vault/demo")
    }

    #[test]
    fn base_syscalls_todos_conocidos_en_x86_64() {
        // En x86_64 NINGUNA syscall de la lista falta (documenta la portabilidad:
        // las omisiones solo ocurren en aarch64: dup2/poll/epoll_wait/readlink).
        let t = SyscallTable::new(TargetArch::x86_64);
        for name in BASE_SYSCALLS {
            assert!(t.get_syscall_nr(name).is_some(), "falta {name} en x86_64");
        }
    }

    #[test]
    fn bpf_digest_serializacion_a_mano() {
        // 3 instrucciones construidas a mano vs la serialización del helper.
        let prog: BpfProgram = vec![
            seccompiler::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 4,
            },
            seccompiler::sock_filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: 0xC000_003E,
            },
            seccompiler::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x8000_0000,
            },
        ];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x20u16.to_le_bytes());
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&0x15u16.to_le_bytes());
        bytes.push(1);
        bytes.push(0);
        bytes.extend_from_slice(&0xC000_003Eu32.to_le_bytes());
        bytes.extend_from_slice(&0x06u16.to_le_bytes());
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&0x8000_0000u32.to_le_bytes());
        assert_eq!(bpf_digest(&prog), Digest::of(&bytes));
    }

    #[test]
    fn build_profile_riscv64_rechazada() {
        let r = build_profile(&CapabilitySet::empty(), app(), vault(), TargetArch::riscv64);
        assert!(r.is_err());
    }

    #[test]
    fn build_profile_net_policy_por_caps() {
        for caps in [
            CapabilitySet::empty(),
            CapabilitySet::from_iter([Capability::FsVault]),
            CapabilitySet::from_iter([Capability::Notify]),
        ] {
            let p = build_profile(&caps, app(), vault(), TargetArch::x86_64)
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(p.net_fds, NetPolicy::NoNet, "sin net-*");
        }
        for caps in [
            CapabilitySet::from_iter([Capability::NetClient]),
            CapabilitySet::from_iter([Capability::NetServer]),
            CapabilitySet::from_iter([Capability::NetClient, Capability::NetServer]),
        ] {
            let p = build_profile(&caps, app(), vault(), TargetArch::x86_64)
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(p.net_fds, NetPolicy::BrokerSockets, "con net-*");
        }
    }

    #[test]
    fn build_profile_allowed_paths() {
        let sin_vault = build_profile(&CapabilitySet::empty(), app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sin_vault.allowed_paths, vec![app()]);
        assert_eq!(sin_vault.landlock, None);

        let con_vault = build_profile(
            &CapabilitySet::from_iter([Capability::FsVault]),
            app(),
            vault(),
            TargetArch::x86_64,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(con_vault.allowed_paths, vec![app(), vault()]);
        assert_eq!(con_vault.landlock, None);
    }

    #[test]
    fn build_profile_determinista_y_aarch64_mas_corto() {
        let caps = CapabilitySet::from_iter(Capability::all().iter().copied());
        let a = build_profile(&caps, app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{e}"));
        let b = build_profile(&caps, app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a, b, "misma entrada → mismo perfil");
        assert_eq!(bpf_digest(&a.seccomp), bpf_digest(&b.seccomp));

        let arm = build_profile(&caps, app(), vault(), TargetArch::aarch64)
            .unwrap_or_else(|e| panic!("{e}"));
        // aarch64 omite 5 syscalls (dup2/poll/epoll_wait/readlink/arch_prctl) → 5 cadenas menos.
        assert_eq!(a.seccomp.len() - arm.seccomp.len(), 5 * 5);
        // Estructura: 3 (chequeo de arch) + 1 (LD nr) + N*5 + 1 (RET default).
        assert_eq!(a.seccomp.len(), 3 + 1 + BASE_SYSCALLS.len() * 5 + 1);
    }

    #[test]
    fn las_capabilities_no_amplian_el_bpf() {
        // Invariante spec 07 §4: el filtro de TODOS los cap-sets es el mismo.
        let vacio = build_profile(&CapabilitySet::empty(), app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{e}"));
        let todas = build_profile(
            &CapabilitySet::from_iter(Capability::all().iter().copied()),
            app(),
            vault(),
            TargetArch::x86_64,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(vacio.seccomp, todas.seccomp);
    }

    #[test]
    fn current_arch_coincide_con_el_host() {
        #[cfg(target_arch = "x86_64")]
        assert_eq!(current_arch().ok(), Some(TargetArch::x86_64));
        #[cfg(target_arch = "aarch64")]
        assert_eq!(current_arch().ok(), Some(TargetArch::aarch64));
    }
}
