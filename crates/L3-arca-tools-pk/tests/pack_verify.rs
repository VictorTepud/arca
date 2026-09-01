//! Tests de aceptación T09 (spec 25 §6): pack→verify, determinismo, graph
//! sync, mutaciones y el e2e PC completo pack→verify→INSTALL.

#![allow(clippy::missing_docs_in_private_items)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use arca_installer::{InstallOpts, Installer, PackageSource as InstallSource};
use arca_store::Store;
use arca_types::AppId;

/// Binario del CLI (cargo rebuilds con el bin actual).
fn cli() -> Command {
    let exe = env!("CARGO_BIN_EXE_arca-tools-pk");
    Command::new(exe)
}

/// Proyecto de app mínimo válido para pack.
fn proyecto(tmp: &Path, id: &str, version: &str) -> PathBuf {
    let src = tmp.join("app");
    let dirs = ["src", "bin/native-aarch64", "meta", "assets/data"];
    for d in dirs {
        std::fs::create_dir_all(src.join(d)).expect("mkdir");
    }
    // binario "ELF"
    let mut elf = vec![0x7fu8, 69, 76, 70, 2, 1, 0];
    elf.extend((0..2048u32).map(|i| (i % 251) as u8));
    std::fs::write(src.join("bin/native-aarch64/app"), &elf).expect("bin");
    // fuente con 3 módulos y deps cruzadas
    std::fs::write(
        src.join("src/main.rs"),
        "mod ui;\nmod datos;\nuse crate::ui::pinta;\nuse crate::datos::carga;\nfn main() { pinta(carga()); }\n",
    )
    .expect("main");
    std::fs::write(
        src.join("src/ui.rs"),
        "use crate::datos::carga;\npub fn pinta(_: u8) {}\n",
    )
    .expect("ui");
    std::fs::write(src.join("src/datos.rs"), "pub fn carga() -> u8 { 7 }\n").expect("datos");
    std::fs::write(src.join("assets/data/d.bin"), vec![3u8; 128]).expect("asset");
    // manifest (sha256 del bin)
    let sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&elf);
        let x: [u8; 32] = h.finalize().into();
        x.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let manifest = format!(
        "[package]\nid = \"{id}\"\nname = \"App Test\"\nversion = \"{version}\"\nmin_host = \"1.0.0\"\napi_level = 1\n\n[runtime]\nbackend_pref = \"native\"\nentry = \"app\"\nrespawn = \"never\"\n\n[artifacts.native]\npath = \"bin/native-aarch64/app\"\nsha256 = \"{sha}\"\n\n[profile]\nlaunch_budget_ms = 120\nmax_frame_mb = 2\n"
    );
    std::fs::write(src.join("manifest.toml"), manifest).expect("manifest");
    src
}

/// Genera claves con el propio CLI y devuelve (dir, ruta .key, ruta .pub).
fn claves(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let out = tmp.join("keys");
    let st = cli()
        .args(["keygen", "--out"])
        .arg(&out)
        .status()
        .expect("keygen status");
    assert!(st.success(), "keygen");
    let key = out.join("signing.key");
    let pubk = out.join("signing.pub");
    (out, key, pubk)
}

/// pack → verify OK (happy path del CLI).
#[test]
fn pack_verify_roundtrip() {
    let tmp = tempfile::tempdir().expect("tmp");
    let src = proyecto(tmp.path(), "dev.arca.pack1", "1.0.0");
    let (_kd, key, pubk) = claves(tmp.path());
    let out = tmp.path().join("app-1.0.0.arca");

    let st = cli()
        .args(["pack", "--src"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .arg("--key")
        .arg(&key)
        .arg("--backend")
        .arg("native")
        .status()
        .expect("pack status");
    assert!(st.success(), "pack falló");
    assert!(out.is_file());

    let st2 = cli()
        .args(["verify", "--file"])
        .arg(&out)
        .arg("--pubkey")
        .arg(&pubk)
        .status()
        .expect("verify status");
    assert!(st2.success(), "verify falló");
}

/// Doble pack mismo input → MISMO digest y mismos bytes de archivo.
#[test]
fn doble_pack_determinista() {
    let tmp = tempfile::tempdir().expect("tmp");
    let src = proyecto(tmp.path(), "dev.arca.pack2", "1.0.0");
    let (_kd, key, pubk) = claves(tmp.path());
    let o1 = tmp.path().join("a.arca");
    let o2 = tmp.path().join("b.arca");

    for o in [&o1, &o2] {
        let st = cli()
            .args(["pack", "--src"])
            .arg(&src)
            .arg("--out")
            .arg(o)
            .arg("--key")
            .arg(&key)
            .arg("--backend")
            .arg("native")
            .status()
            .expect("pack");
        assert!(st.success());
    }
    // digest idéntico (línea "digest <hex>" del verify)
    let digests = |o: &Path| -> String {
        let outp = cli()
            .args(["verify", "--file"])
            .arg(o)
            .arg("--pubkey")
            .arg(&pubk)
            .output()
            .expect("verify out");
        assert!(outp.status.success());
        String::from_utf8_lossy(&outp.stdout)
            .lines()
            .find(|l| l.contains("digest "))
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(digests(&o1), digests(&o2), "digest debe ser determinista");
    // y los bytes del 7z también (timestamps fijos por defecto)
    let b1 = std::fs::read(&o1).unwrap();
    let b2 = std::fs::read(&o2).unwrap();
    assert_eq!(b1, b2, "byte-determinismo del .arca");
}

/// Graph: se genera, entra en sync, y un mmd editado a mano ROMPE el pack.
#[test]
fn graph_sync_y_fallo() {
    let tmp = tempfile::tempdir().expect("tmp");
    let src = proyecto(tmp.path(), "dev.arca.pack3", "1.0.0");
    let (_kd, key, _pub) = claves(tmp.path());

    // genera
    let st = cli()
        .args(["graph", "--src"])
        .arg(&src)
        .status()
        .expect("graph");
    assert!(st.success());
    let mmd = src.join("meta/graph.mmd");
    assert!(mmd.is_file());
    let contenido = std::fs::read_to_string(&mmd).unwrap();
    assert!(contenido.contains("flowchart TD"));
    assert!(
        contenido.contains("m_[\"(raíz)"),
        "nodo raíz (main.rs): {contenido}"
    );
    assert!(contenido.contains("m_datos"));

    // check-only en sync
    let st2 = cli()
        .args(["graph", "--src"])
        .arg(&src)
        .arg("--check-only")
        .status()
        .expect("check");
    assert!(st2.success());

    // editar a mano → pack falla (spec 25 §4)
    std::fs::write(&mmd, "flowchart TD\n    m_main[\"mentira\"]\n").unwrap();
    let out = tmp.path().join("x.arca");
    let st3 = cli()
        .args(["pack", "--src"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .arg("--key")
        .arg(&key)
        .arg("--backend")
        .arg("native")
        .status()
        .expect("pack roto");
    assert!(!st3.success(), "pack debe fallar con graph desincronizado");

    // regenerar → pack pasa
    let st4 = cli()
        .args(["graph", "--src"])
        .arg(&src)
        .status()
        .expect("regen");
    assert!(st4.success());
    let st5 = cli()
        .args(["pack", "--src"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .arg("--key")
        .arg(&key)
        .arg("--backend")
        .arg("native")
        .status()
        .expect("pack ok");
    assert!(st5.success());
}

/// Corpus de mutaciones post-pack → verify falla (≥18/20).
#[test]
fn mutaciones_post_pack_rechazadas() {
    let tmp = tempfile::tempdir().expect("tmp");
    let src = proyecto(tmp.path(), "dev.arca.pack4", "1.0.0");
    let (_kd, key, pubk) = claves(tmp.path());
    let out = tmp.path().join("m.arca");
    let st = cli()
        .args(["pack", "--src"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .arg("--key")
        .arg(&key)
        .arg("--backend")
        .arg("native")
        .status()
        .expect("pack");
    assert!(st.success());
    let base = std::fs::read(&out).unwrap();
    assert!(base.len() > 512);

    // PRNG determinista (semilla fija)
    let mut state: u64 = 0xC0FF_EE00_CAFE_1234;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut rechazados = 0;
    for caso in 0..20 {
        let mut b = base.clone();
        // zonas: header / media (streams) / cola (firma)
        let lo = match caso % 3 {
            0 => 32..(b.len() / 3),
            1 => (b.len() / 3)..(2 * b.len() / 3),
            _ => (2 * b.len() / 3)..b.len().saturating_sub(2),
        };
        let span = lo.end.saturating_sub(lo.start);
        let pos = lo.start + (next() as usize) % span.max(1);
        b[pos] ^= 1u8 << (next() % 8);
        let tmpf = tmp.path().join(format!("mut-{caso}.arca"));
        std::fs::write(&tmpf, &b).unwrap();
        let st = cli()
            .args(["verify", "--file"])
            .arg(&tmpf)
            .arg("--pubkey")
            .arg(&pubk)
            .status()
            .expect("verify mut");
        if !st.success() {
            rechazados += 1;
        }
        let _ = std::fs::remove_file(&tmpf);
    }
    assert!(
        rechazados >= 18,
        "casi toda mutación debe caer: {rechazados}/20"
    );
}

/// e2e PC COMPLETO: pack (CLI) → verify (CLI) → INSTALL (arca-installer)
/// → verify_installed. La aceptación real de T09.
#[test]
fn e2e_pack_verify_install() {
    let tmp = tempfile::tempdir().expect("tmp");
    let src = proyecto(tmp.path(), "dev.arca.e2e", "1.0.0");
    let (_kd, key, pubk) = claves(tmp.path());
    let out = tmp.path().join("e2e.arca");

    // 1. pack
    let st = cli()
        .args(["pack", "--src"])
        .arg(&src)
        .arg("--out")
        .arg(&out)
        .arg("--key")
        .arg(&key)
        .arg("--backend")
        .arg("native")
        .status()
        .expect("pack");
    assert!(st.success());

    // 2. verify (algoritmo del host)
    let st2 = cli()
        .args(["verify", "--file"])
        .arg(&out)
        .arg("--pubkey")
        .arg(&pubk)
        .status()
        .expect("verify");
    assert!(st2.success());

    // 3. install con el Installer REAL (ring de 1 clave)
    let mut ring = arca_sign::RingOfTrust::empty();
    let pub_bytes = std::fs::read(&pubk).unwrap();
    assert_eq!(pub_bytes.len(), 32);
    let mut b = [0u8; 32];
    b.copy_from_slice(&pub_bytes);
    ring.push_bytes(&b).expect("pubkey");
    let root = tmp.path().join("apps");
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(Store::open(&tmp.path().join("arca.db")).expect("store"));
    let installer = Installer::new(root.clone(), store, ring);
    let r = installer.install(InstallSource::Path(out), &InstallOpts::default());
    match &r {
        Ok(arca_installer::InstallOutcome::Installed { .. }) => {}
        other => panic!("install e2e: {other:?}"),
    }

    // 4. disco + registro + anti-tamper
    let id = match AppId::new("dev.arca.e2e") {
        Ok(i) => i,
        Err(e) => panic!("id: {e}"),
    };
    assert!(root.join("dev.arca.e2e/current").is_symlink());
    assert!(root
        .join("dev.arca.e2e/current/bin/native-aarch64/app")
        .is_file());
    assert!(root.join("dev.arca.e2e/current/meta/graph.mmd").is_file());
    assert!(installer.store().get_app(&id).unwrap().is_some());
    assert!(installer.verify_installed(&id).is_ok());

    // 5. uninstall
    assert!(installer.uninstall(&id).is_ok());
}

/// CLI UX: --help completo y exit codes documentados.
#[test]
fn cli_help_completo() {
    for args in [
        vec!["--help"],
        vec!["keygen", "--help"],
        vec!["pack", "--help"],
        vec!["verify", "--help"],
        vec!["graph", "--help"],
        vec!["trust-ring", "add", "--help"],
    ] {
        let out = cli().args(&args).output().expect("help");
        assert!(out.status.success(), "help {args:?}");
        let texto = String::from_utf8_lossy(&out.stdout);
        assert!(
            texto.contains("Usage") || texto.contains("usage"),
            "help {args:?}: {texto}"
        );
    }
    // error de uso → clap exit code 2
    let out = cli().arg("--no-existe").output().expect("bad flag");
    assert_eq!(out.status.code(), Some(2));
    // fallo operacional → 1 (verify de archivo inexistente)
    let out = cli()
        .args(["verify", "--file"])
        .arg("/no/existe.arca")
        .arg("--pubkey")
        .arg("/no/pub")
        .output()
        .expect("verify fail");
    assert_eq!(out.status.code(), Some(1));
}

/// keygen: 0600, 32 B, no sobreescribe.
#[test]
fn keygen_persiste_0600() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (kd, key, pubk) = claves(tmp.path());
    assert!(key.is_file() && pubk.is_file());
    assert_eq!(std::fs::read(&key).unwrap().len(), 32);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&key).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "permisos de la clave");
    // segunda generación → fallo (exit 1, no sobreescribe)
    let st = cli()
        .args(["keygen", "--out"])
        .arg(kd)
        .status()
        .expect("keygen 2");
    assert_eq!(st.code(), Some(1));
}

/// trust-ring add: acumula, dedup y emite bin parseable.
#[test]
fn trust_ring_add_y_bin() {
    let tmp = tempfile::tempdir().expect("tmp");
    let (_kd, _key, pubk) = claves(tmp.path());
    let ring_txt = tmp.path().join("trusted-pubkeys.txt");
    let bin = tmp.path().join("trusted-pubkeys.bin");

    // añadir DOS veces la misma → dedup
    for _ in 0..2 {
        let st = cli()
            .args(["trust-ring", "add"])
            .arg("--pub-key")
            .arg(&pubk)
            .arg("--ring")
            .arg(&ring_txt)
            .arg("--emit")
            .arg(&bin)
            .status()
            .expect("ring add");
        assert!(st.success());
    }
    let bytes = std::fs::read(&bin).unwrap();
    assert_eq!(bytes.len(), 4 + 32, "1 clave");
    let ring = arca_sign::RingOfTrust::from_bin(&bytes).expect("bin");
    assert_eq!(ring.len(), 1);

    // una clave más
    let kd2 = tmp.path().join("k2");
    let st = cli()
        .args(["keygen", "--out"])
        .arg(&kd2)
        .status()
        .expect("keygen 2");
    assert!(st.success());
    let st = cli()
        .args(["trust-ring", "add"])
        .arg("--pub-key")
        .arg(kd2.join("signing.pub"))
        .arg("--ring")
        .arg(&ring_txt)
        .arg("--emit")
        .arg(&bin)
        .status()
        .expect("ring add 2");
    assert!(st.success());
    let ring = arca_sign::RingOfTrust::from_bin(&std::fs::read(&bin).unwrap()).expect("bin 2");
    assert_eq!(ring.len(), 2);
}
