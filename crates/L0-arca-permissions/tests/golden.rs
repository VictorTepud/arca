//! Golden del BPF generado (spec 07 §5: "golden test del programa generado").
//!
//! Cinco capability-sets de referencia → hash blake3 del programa BPF
//! (serialización little-endian de `sock_filter` en [`bpf_digest`]).
//!
//! # La invariante que fijan estos hashes
//!
//! En v1 las capabilities **no amplían el filtro seccomp** (patrón "socket
//! pasado": la red va por fds del broker; la bóveda por `allowed_paths`).
//! Por eso los 5 sets comparten hash POR ARQUITECTURA: el golden no solo
//! fija estabilidad entre builds — fija que conceder `net-client` NO abre
//! `socket(2)` en el sandbox. Si algún día una capability debe ampliar el
//! BPF, este test es el que debe cambiarse A PROPÓSITO (con review de
//! seguridad).

use std::path::Path;

use arca_permissions::{
    bpf_digest, build_profile, CapabilitySet, NetPolicy, TargetArch, BASE_SYSCALLS,
};
use arca_types::Capability;

fn app() -> &'static Path {
    Path::new("/apps/demo")
}
fn vault() -> &'static Path {
    Path::new("/vault/demo")
}

/// Los 5 capability-sets de referencia (mismos que usa `tests/e2e.rs`).
fn cap_sets() -> Vec<(&'static str, CapabilitySet)> {
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

/// Golden x86_64 de los 5 sets de referencia (blake3 del BPF serializado).
///
/// TODOS IGUALES por diseño v1: ninguna capability amplía el filtro
/// (ver la invariante en la doc del módulo). Programa: 325 instrucciones
/// (rev2: +chdir/execve/arch_prctl/set_tid_address para arca-launch).
const GOLDEN_X86_64: [&str; 5] = [
    // empty / net-client / fs-vault / net+vault / todas
    "498b8e84258d6b2e20dc3c0d7e416f2cc5e4009bc5b776d0af3f7dd3aed8a4e0",
    "498b8e84258d6b2e20dc3c0d7e416f2cc5e4009bc5b776d0af3f7dd3aed8a4e0",
    "498b8e84258d6b2e20dc3c0d7e416f2cc5e4009bc5b776d0af3f7dd3aed8a4e0",
    "498b8e84258d6b2e20dc3c0d7e416f2cc5e4009bc5b776d0af3f7dd3aed8a4e0",
    "498b8e84258d6b2e20dc3c0d7e416f2cc5e4009bc5b776d0af3f7dd3aed8a4e0",
];

/// Golden aarch64 (mismo programa para los 5 sets; distinto del x86_64).
/// Programa: 300 instrucciones (sin dup2/poll/epoll_wait/readlink/arch_prctl).
const GOLDEN_AARCH64: &str = "29d3bafcaf5b93e5cfaa3313f1f2851ec99d0134fc1587ee9eb1f86bf93b5d76";

#[test]
fn golden_bpf_por_capset_x86_64() {
    for (i, (nombre, caps)) in cap_sets().into_iter().enumerate() {
        let p = build_profile(&caps, app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));
        let hash = bpf_digest(&p.seccomp).to_hex();
        assert_eq!(
            hash, GOLDEN_X86_64[i],
            "golden de {nombre} cambió: ¿se amplió el filtro sin review?"
        );
    }
}

#[test]
fn golden_bpf_aarch64() {
    for (nombre, caps) in cap_sets() {
        let p = build_profile(&caps, app(), vault(), TargetArch::aarch64)
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));
        assert_eq!(
            bpf_digest(&p.seccomp).to_hex(),
            GOLDEN_AARCH64,
            "golden aarch64 de {nombre} cambió"
        );
    }
}

#[test]
fn determinismo_dos_builds_mismo_hash() {
    for (nombre, caps) in cap_sets() {
        for arch in [TargetArch::x86_64, TargetArch::aarch64] {
            let a = build_profile(&caps, app(), vault(), arch).unwrap_or_else(|e| panic!("{e}"));
            let b = build_profile(&caps, app(), vault(), arch).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                bpf_digest(&a.seccomp),
                bpf_digest(&b.seccomp),
                "{nombre}: build no determinista"
            );
            assert_eq!(a, b);
        }
    }
}

#[test]
fn caps_no_amplian_el_bpf_invariante_central() {
    // La invariante de seguridad v1, como test con nombre propio:
    // con TODAS las capabilities concedidas, el BPF es EXACTAMENTE el del
    // sandbox vacío. Ninguna compra syscalls nuevas.
    for arch in [TargetArch::x86_64, TargetArch::aarch64] {
        let vacio = build_profile(&CapabilitySet::empty(), app(), vault(), arch)
            .unwrap_or_else(|e| panic!("{e}"));
        let todas =
            build_profile(&cap_sets()[4].1, app(), vault(), arch).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            bpf_digest(&vacio.seccomp),
            bpf_digest(&todas.seccomp),
            "conceder capabilities amplió el BPF ({arch:?})"
        );
    }
}

#[test]
fn arch_distinta_programa_distinto() {
    // El BPF embebe el AUDIT_ARCH: compilar para otro arch cambia el hash.
    let caps = CapabilitySet::empty();
    let x86 =
        build_profile(&caps, app(), vault(), TargetArch::x86_64).unwrap_or_else(|e| panic!("{e}"));
    let arm =
        build_profile(&caps, app(), vault(), TargetArch::aarch64).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(bpf_digest(&x86.seccomp), bpf_digest(&arm.seccomp));
}

#[test]
fn estructura_y_longitud_del_programa() {
    // Estructura del codegen de seccompiler 0.5 con cadenas vacías:
    // 3 (chequeo de arch) + 1 (LD nr) + N*5 (JEQ+JA+JA+RET allow+RET kill)
    // + 1 (RET default) instrucciones.
    let caps = CapabilitySet::empty();
    let x86 =
        build_profile(&caps, app(), vault(), TargetArch::x86_64).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(BASE_SYSCALLS.len(), 64);
    assert_eq!(x86.seccomp.len(), 3 + 1 + BASE_SYSCALLS.len() * 5 + 1);

    // aarch64 omite dup2/poll/epoll_wait/readlink/arch_prctl (5 cadenas menos).
    let arm =
        build_profile(&caps, app(), vault(), TargetArch::aarch64).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(arm.seccomp.len(), x86.seccomp.len() - 5 * 5);
}

#[test]
fn perfiles_de_los_sets_con_datos_correctos() {
    // Los 5 sets SÍ difieren en las concesiones (lo que de verdad compran):
    for (nombre, caps) in cap_sets() {
        let p = build_profile(&caps, app(), vault(), TargetArch::x86_64)
            .unwrap_or_else(|e| panic!("{nombre}: {e}"));
        let espera_red = caps.has(Capability::NetClient) || caps.has(Capability::NetServer);
        assert_eq!(
            p.net_fds == NetPolicy::BrokerSockets,
            espera_red,
            "{nombre}: NetPolicy"
        );
        let espera_vault = caps.has(Capability::FsVault);
        assert_eq!(p.allowed_paths.len() == 2, espera_vault, "{nombre}: vault");
        assert_eq!(p.landlock, None, "{nombre}: landlock SIEMPRE None");
    }
}
