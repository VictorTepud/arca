//! Regenerador de los hashes golden de `tests/golden.rs` (documentado):
//! `cargo run -p arca-permissions --example probe_hash`.
//! La base de syscalls es rev2 (arranque de arca-launch incluido).

// probe: imprime los nuevos hashes golden tras añadir las 4 syscalls de arranque
use arca_permissions::{bpf_digest, build_profile, CapabilitySet, TargetArch};
fn main() {
    let sets = ["empty", "net-client", "fs-vault", "net+vault", "todas"];
    for (i, (n, caps)) in sets.iter().zip(cap_sets()).enumerate() {
        let p = build_profile(
            &caps,
            std::path::Path::new("/a"),
            std::path::Path::new("/v"),
            TargetArch::x86_64,
        )
        .unwrap();
        let a = build_profile(
            &caps,
            std::path::Path::new("/a"),
            std::path::Path::new("/v"),
            TargetArch::aarch64,
        )
        .unwrap();
        if i == 0 {
            println!(
                "len_x86={} len_arm={} base={}",
                p.seccomp.len(),
                a.seccomp.len(),
                arca_permissions::BASE_SYSCALLS.len()
            );
        }
        println!("x86[{n}] = {}", bpf_digest(&p.seccomp).to_hex());
        if i == 4 {
            println!("arm = {}", bpf_digest(&a.seccomp).to_hex());
        }
    }
}
fn cap_sets() -> Vec<CapabilitySet> {
    use arca_types::Capability::*;
    vec![
        CapabilitySet::empty(),
        CapabilitySet::from_iter([NetClient]),
        CapabilitySet::from_iter([FsVault]),
        CapabilitySet::from_iter([NetClient, FsVault]),
        CapabilitySet::from_iter([
            NetClient,
            NetServer,
            ClipboardRead,
            ClipboardWrite,
            Notify,
            Share,
            OpenUri,
            Vibrate,
            FsVault,
            SystemStoreRead,
            BackgroundAudio,
        ]),
    ]
}
