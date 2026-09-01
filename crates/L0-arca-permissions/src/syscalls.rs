//! Tabla nombre → número de syscall (privada del crate).
//!
//! # Por qué existe (desviación documentada)
//!
//! El diseño original asumía `seccompiler::SyscallTable`, pero en
//! seccompiler 0.5 ese tipo es `pub(crate)` y solo se compila con la feature
//! `json` (verificada en el fuente de `~/.cargo/registry`: `lib.rs` declara
//! `mod syscall_table;` bajo `#[cfg(feature = "json")]`). Como el
//! `Cargo.toml` del workspace no activa esa feature, este crate mantiene sus
//! **propias tablas mínimas** con la misma forma de API
//! ([`SyscallTable::new`] / [`get_syscall_nr`]), limitadas a las syscalls de
//! [`crate::BASE_SYSCALLS`].
//!
//! # Procedencia de los números
//!
//! Espejo de las tablas del kernel Linux (las mismas que embebe seccompiler,
//! extraídas de su fuente para verificarlas una a una). Los números de
//! syscall son parte de la ABI del kernel: **estables por arquitectura**.
//! En aarch64 no existen `dup2`, `poll`, `epoll_wait` ni `readlink` (la ABI
//! nueva las reemplaza por `dup3`, `ppoll`, `epoll_pwait`, `readlinkat`),
//! por lo que quedan ausentes de esa tabla y [`crate::build_profile`] las
//! omite con un `warn`.

use seccompiler::TargetArch;

/// Tabla x86_64: syscalls del sandbox base (números del kernel, 64-bit ABI).
const X86_64_SYSCALLS: &[(&str, i64)] = &[
    ("read", 0),
    ("write", 1),
    ("close", 3),
    ("fstat", 5),
    ("lseek", 8),
    ("mmap", 9),
    ("mprotect", 10),
    ("munmap", 11),
    ("brk", 12),
    ("rt_sigaction", 13),
    ("rt_sigprocmask", 14),
    ("rt_sigreturn", 15),
    ("ioctl", 16),
    ("readv", 19),
    ("writev", 20),
    ("sched_yield", 24),
    ("mremap", 25),
    ("madvise", 28),
    ("dup", 32),
    ("dup2", 33),
    ("nanosleep", 35),
    ("getpid", 39),
    ("sendto", 44),
    ("recvfrom", 45),
    ("sendmsg", 46),
    ("recvmsg", 47),
    ("shutdown", 48),
    ("getsockname", 51),
    ("getpeername", 52),
    ("setsockopt", 54),
    ("getsockopt", 55),
    ("futex", 202),
    ("exit", 60),
    ("chdir", 80),
    ("execve", 59),
    ("arch_prctl", 158),
    ("set_tid_address", 218),
    ("getuid", 102),
    ("getgid", 104),
    ("geteuid", 107),
    ("getegid", 108),
    ("fcntl", 72),
    ("poll", 7),
    ("sigaltstack", 131),
    ("gettid", 186),
    ("readlink", 89),
    ("prctl", 157),
    ("readlinkat", 267),
    ("pselect6", 270),
    ("ppoll", 271),
    ("set_robust_list", 273),
    ("getdents64", 217),
    ("clock_nanosleep", 230),
    ("exit_group", 231),
    ("epoll_wait", 232),
    ("epoll_ctl", 233),
    ("tgkill", 234),
    ("clock_gettime", 228),
    ("clock_getres", 229),
    ("dup3", 292),
    ("epoll_create1", 291),
    ("epoll_pwait", 281),
    ("getrandom", 318),
    ("rseq", 334),
];

/// Tabla aarch64: syscalls del sandbox base (números del kernel, arm64 ABI).
///
/// Sin `dup2`/`poll`/`epoll_wait`/`readlink`/`arch_prctl`: la ABI aarch64
/// no las define (TLS va por registro tpidr_el0, no syscall).
const AARCH64_SYSCALLS: &[(&str, i64)] = &[
    ("chdir", 49),
    ("execve", 221),
    ("set_tid_address", 96),
    ("epoll_create1", 20),
    ("epoll_ctl", 21),
    ("epoll_pwait", 22),
    ("dup3", 24),
    ("fcntl", 25),
    ("ioctl", 29),
    ("dup", 23),
    ("pselect6", 72),
    ("ppoll", 73),
    ("readlinkat", 78),
    ("close", 57),
    ("fstat", 80),
    ("getdents64", 61),
    ("lseek", 62),
    ("read", 63),
    ("write", 64),
    ("readv", 65),
    ("writev", 66),
    ("futex", 98),
    ("set_robust_list", 99),
    ("nanosleep", 101),
    ("exit", 93),
    ("exit_group", 94),
    ("rt_sigaction", 134),
    ("rt_sigprocmask", 135),
    ("rt_sigreturn", 139),
    ("sigaltstack", 132),
    ("tgkill", 131),
    ("brk", 214),
    ("munmap", 215),
    ("mremap", 216),
    ("mprotect", 226),
    ("mmap", 222),
    ("madvise", 233),
    ("sched_yield", 124),
    ("getsockname", 204),
    ("getpeername", 205),
    ("sendto", 206),
    ("recvfrom", 207),
    ("setsockopt", 208),
    ("getsockopt", 209),
    ("shutdown", 210),
    ("sendmsg", 211),
    ("recvmsg", 212),
    ("getpid", 172),
    ("gettid", 178),
    ("getuid", 174),
    ("geteuid", 175),
    ("getgid", 176),
    ("getegid", 177),
    ("clock_gettime", 113),
    ("clock_getres", 114),
    ("clock_nanosleep", 115),
    ("getrandom", 278),
    ("prctl", 167),
    ("rseq", 293),
];

/// Mapping arch → tabla (misma forma que la `SyscallTable` interna de
/// seccompiler, para poder sustituirla si algún día se publica).
pub(crate) struct SyscallTable {
    /// Entradas (nombre, número) de la arquitectura elegida.
    entries: &'static [(&'static str, i64)],
}

impl SyscallTable {
    /// Tabla de la arquitectura dada. Para `riscv64` devuelve la tabla
    /// vacía: [`crate::build_profile`] la rechaza antes de llegar aquí.
    pub(crate) const fn new(arch: TargetArch) -> Self {
        Self {
            entries: match arch {
                TargetArch::x86_64 => X86_64_SYSCALLS,
                TargetArch::aarch64 => AARCH64_SYSCALLS,
                TargetArch::riscv64 => &[],
            },
        }
    }

    /// Número de syscall para `sys_name` en la arquitectura de la tabla
    /// (`None` si no existe: p. ej. `dup2` en aarch64).
    pub(crate) fn get_syscall_nr(&self, sys_name: &str) -> Option<i64> {
        // Búsqueda lineal: ~60 entradas y se consulta una vez por lanzamiento
        // (no es un path caliente); determinista y sin dependencias.
        self.entries
            .iter()
            .find(|(name, _)| *name == sys_name)
            .map(|(_, nr)| *nr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_64_numeros_canonicos() {
        let t = SyscallTable::new(TargetArch::x86_64);
        // Valores del kernel, verificados contra la tabla de seccompiler:
        assert_eq!(t.get_syscall_nr("read"), Some(0));
        assert_eq!(t.get_syscall_nr("write"), Some(1));
        assert_eq!(t.get_syscall_nr("close"), Some(3));
        assert_eq!(t.get_syscall_nr("epoll_ctl"), Some(233));
        assert_eq!(t.get_syscall_nr("getrandom"), Some(318));
        assert_eq!(t.get_syscall_nr("getdents64"), Some(217));
    }

    #[test]
    fn aarch64_numeros_canonicos_y_legados_ausentes() {
        let t = SyscallTable::new(TargetArch::aarch64);
        assert_eq!(t.get_syscall_nr("close"), Some(57));
        assert_eq!(t.get_syscall_nr("epoll_ctl"), Some(21));
        assert_eq!(t.get_syscall_nr("readlinkat"), Some(78));
        // La ABI arm64 no tiene las variantes legacy:
        assert_eq!(t.get_syscall_nr("dup2"), None);
        assert_eq!(t.get_syscall_nr("poll"), None);
        assert_eq!(t.get_syscall_nr("epoll_wait"), None);
        assert_eq!(t.get_syscall_nr("readlink"), None);
    }

    #[test]
    fn nombres_desconocidos_devuelven_none() {
        let t = SyscallTable::new(TargetArch::x86_64);
        assert_eq!(t.get_syscall_nr("openat"), None); // deliberadamente ausente
        assert_eq!(t.get_syscall_nr("nosuchcall"), None);
        assert_eq!(t.get_syscall_nr(""), None);
    }
}
