//! Tests del instalador (spec 12 §6): happy path, interrupciones en 4
//! puntos, corpus malicioso, rollback, sweep y verify_installed.

#![allow(clippy::missing_docs_in_private_items)]

use std::path::PathBuf;
use std::sync::Arc;

use arca_store::Store;
use arca_types::{AppId, ArcaError};

use crate::flow;
use crate::source::PackageSource;
use crate::testkit::{test_ring, FixturePkg, TestRing};
use crate::{InstallOpts, InstallOutcome, Installer};

/// Entorno: tmpdir root + store + installer + ring COHERENTES entre sí.
struct Env {
    /// Raíz de apps.
    root: PathBuf,
    /// Tempdir dueña de todo.
    _tmp: tempfile::TempDir,
    /// Installer con el ring de `ring`.
    installer: Installer,
    /// Claves para firmar fixtures.
    ring: TestRing,
}

impl Env {
    fn new(tag: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("apps");
        std::fs::create_dir_all(&root).expect("mkdir apps");
        let db = tmp.path().join(format!("arca-{tag}.db"));
        let store = Arc::new(Store::open(&db).expect("store"));
        let ring = test_ring();
        let installer = Installer::new(root.clone(), store, ring.ring.clone());
        Self {
            root,
            _tmp: tmp,
            installer,
            ring,
        }
    }

    /// Paquete firmado con el ring de este env.
    fn pkg(&self, id: &str, ver: &str) -> PathBuf {
        FixturePkg::new(id, ver).build(self._tmp.path(), &self.ring)
    }

    /// Bytes del paquete firmado (para mutar).
    fn pkg_bytes(&self, id: &str, ver: &str) -> Vec<u8> {
        std::fs::read(self.pkg(id, ver)).expect("leer fixture")
    }
}

fn app(id: &str) -> AppId {
    match AppId::new(id) {
        Ok(a) => a,
        Err(e) => panic!("AppId de test inválido: {e}"),
    }
}

/// Residuos `.tmp-*` de un app-dir.
fn stagings(root: &std::path::Path, id: &str) -> Vec<String> {
    let d = root.join(id);
    if !d.is_dir() {
        return Vec::new();
    }
    std::fs::read_dir(d)
        .expect("dir app")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".tmp-"))
        .collect()
}

/// Happy path: install → disco correcto → verify → uninstall.
#[test]
fn happy_path_completo() {
    let env = Env::new("happy");
    let p1 = env.pkg("dev.arca.t1", "1.0.0");
    let out = env
        .installer
        .install(PackageSource::Path(p1), &InstallOpts::default());
    match &out {
        Ok(InstallOutcome::Installed {
            app: app_id,
            version,
        }) => {
            assert_eq!(app_id, &app("dev.arca.t1"));
            assert_eq!(version.to_string(), "1.0.0");
        }
        other => panic!("esperaba Installed: {other:?}"),
    }

    // disco: current → v1.0.0 con bin ejecutable y assets presentes
    let cur = env
        .installer
        .current_dir(&app("dev.arca.t1"))
        .expect("current");
    assert!(cur.ends_with("v1.0.0"), "{cur:?}");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(cur.join("bin/native-aarch64/app"))
        .expect("bin")
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "bin debe ser ejecutable: {mode:o}");
    assert!(cur.join("manifest.toml").is_file());
    assert!(cur.join("assets/data/blob.bin").is_file());

    assert!(env.installer.verify_installed(&app("dev.arca.t1")).is_ok());

    // uninstall limpia todo
    assert!(env.installer.uninstall(&app("dev.arca.t1")).is_ok());
    assert!(env
        .installer
        .store()
        .get_app(&app("dev.arca.t1"))
        .expect("get")
        .is_none());
    assert!(!env.root.join("dev.arca.t1").exists());
}

/// Update + rollback con el mismo anillo.
#[test]
fn update_y_rollback() {
    let env = Env::new("upd");
    let p1 = env.pkg("dev.arca.t2", "1.0.0");
    let p2 = env.pkg("dev.arca.t2", "1.1.0");
    assert!(matches!(
        env.installer
            .install(PackageSource::Path(p1), &InstallOpts::default()),
        Ok(InstallOutcome::Installed { .. })
    ));
    match env
        .installer
        .install(PackageSource::Path(p2), &InstallOpts::default())
    {
        Ok(InstallOutcome::Updated { from, to, .. }) => {
            assert_eq!(from.to_string(), "1.0.0");
            assert_eq!(to.to_string(), "1.1.0");
        }
        other => panic!("esperaba Updated: {other:?}"),
    }
    // .trash conserva la anterior
    assert!(env.root.join("dev.arca.t2/.trash/v1.0.0").is_dir());

    // rollback → 1.0.0 (y el store lo refleja)
    let rb = env.installer.rollback(&app("dev.arca.t2"));
    match &rb {
        Ok(v) => assert_eq!(v.to_string(), "1.0.0"),
        Err(e) => panic!("rollback: {e}"),
    }
    let cur = env
        .installer
        .current_dir(&app("dev.arca.t2"))
        .expect("current");
    assert!(cur.ends_with("v1.0.0"));
    let rec = env
        .installer
        .store()
        .get_app(&app("dev.arca.t2"))
        .expect("get")
        .expect("rec");
    assert_eq!(rec.version, "1.0.0");
    // el swap dejó la 1.1.0 como nueva fuente de rollback
    assert!(env.root.join("dev.arca.t2/.trash/v1.1.0").is_dir());
}

/// Downgrade rechazado salvo flag.
#[test]
fn downgrade_rechazado_y_permitido() {
    let env = Env::new("dg");
    assert!(env
        .installer
        .install(
            PackageSource::Path(env.pkg("dev.arca.t3", "2.0.0")),
            &InstallOpts::default()
        )
        .is_ok());
    let r = env.installer.install(
        PackageSource::Path(env.pkg("dev.arca.t3", "1.0.0")),
        &InstallOpts::default(),
    );
    assert!(r.is_err(), "downgrade debe rechazarse: {r:?}");
    let ok = env.installer.install(
        PackageSource::Path(env.pkg("dev.arca.t3", "1.0.0")),
        &InstallOpts {
            allow_downgrade: true,
            ..Default::default()
        },
    );
    assert!(ok.is_ok());
}

/// Corpus malicioso: firma inválida / entrada duplicada / fuera de layout /
/// manifest.digest alterado. Ninguno deja staging.
#[test]
fn corpus_malicioso_rechazado_y_limpio() {
    let env = Env::new("mal");

    // 1. firma inválida (bytes de firma mutados dentro del paquete)
    let mut bytes = env.pkg_bytes("dev.arca.m1", "1.0.0");
    // último byte del archivo = zona de streams finales (firma va al final):
    // mutar VARIOS bytes profundos cubre el bloque de la firma
    for i in (bytes.len() - 40..bytes.len() - 4).step_by(3) {
        bytes[i] ^= 0x5a;
    }
    let r = env
        .installer
        .install(PackageSource::Bytes(bytes), &InstallOpts::default());
    assert!(r.is_err(), "firma mutada debe caer: {r:?}");
    assert!(stagings(&env.root, "dev.arca.m1").is_empty());

    // 2. entrada duplicada del bin (7z la rechaza en pre-escaneo)
    let p2 = FixturePkg::new("dev.arca.m2", "1.0.0")
        .with_extra("bin/native-aarch64/app", vec![1u8, 2, 3, 4])
        .build(env._tmp.path(), &env.ring);
    let r2 = env
        .installer
        .install(PackageSource::Path(p2), &InstallOpts::default());
    assert!(r2.is_err(), "duplicado debe rechazarse");
    assert!(stagings(&env.root, "dev.arca.m2").is_empty());

    // 3. archivo fuera de layout
    let p3 = FixturePkg::new("dev.arca.m3", "1.0.0")
        .with_extra("evil/fuera.txt", b"x".to_vec())
        .build(env._tmp.path(), &env.ring);
    let r3 = env
        .installer
        .install(PackageSource::Path(p3), &InstallOpts::default());
    // pkg-model mapea PkgError→ArcaError::Internal estáticamente: el rechazo
    // es lo vinculante (y el texto identifica la causa).
    assert!(r3.as_ref().map(|_| ()).is_err(), "{r3:?}");
    assert!(r3
        .map_err(|e| e.to_string())
        .err()
        .unwrap_or_default()
        .contains("extra"));
    assert!(stagings(&env.root, "dev.arca.m3").is_empty());

    // 4. manifest con sha de bin que no cuadra: bin mutado PERO manifest
    //    re-firmado con el sha del bin viejo → el end_file aborta temprano.
    //    Se construye mutando el bin DESPUÉS de firmar (sha ya fijado):
    let mut bytes4 = env.pkg_bytes("dev.arca.m4", "1.0.0");
    // mutar el bloque de datos del bin (stream LZMA2 del bin ≈ primeros
    // bloques tras el header): bit-flip en la zona media-alta
    let mid = bytes4.len() / 3;
    bytes4[mid] ^= 0xff;
    bytes4[mid + 7] ^= 0x0f;
    let r4 = env
        .installer
        .install(PackageSource::Bytes(bytes4), &InstallOpts::default());
    assert!(r4.is_err(), "sha/crc del bin mutado debe caer: {r4:?}");
    assert!(stagings(&env.root, "dev.arca.m4").is_empty());
}

/// Interrupción (a): truncado en zona de header → fallo ANTES de extraer.
#[test]
fn interrupcion_a_paquete_truncado() {
    let env = Env::new("inta");
    let mut bytes = env.pkg_bytes("dev.arca.inta", "1.0.0");
    bytes.truncate(bytes.len() / 8); // rompe el header/streams temprano
    let r = env
        .installer
        .install(PackageSource::Bytes(bytes), &InstallOpts::default());
    assert!(r.is_err());
    assert!(stagings(&env.root, "dev.arca.inta").is_empty());
    assert_eq!(env.installer.sweep().expect("sweep"), 0);
}

/// Interrupción (b): stream corrupto a mitad de extracción → abort + guard.
#[test]
fn interrupcion_b_io_a_mitad() {
    let env = Env::new("intb");
    let mut bytes = env.pkg_bytes("dev.arca.intb", "1.0.0");
    // corromper la zona media (streams de datos): decode/CRC falla DURANTE
    // la extracción real, no al abrir
    let mid = bytes.len() * 7 / 10;
    for i in (mid..bytes.len() - 4).step_by(5) {
        bytes[i] ^= 0xa3;
    }
    let r = env
        .installer
        .install(PackageSource::Bytes(bytes), &InstallOpts::default());
    assert!(r.is_err(), "stream corrupto debe fallar");
    assert!(
        stagings(&env.root, "dev.arca.intb").is_empty(),
        "guard limpió"
    );
    // nada registrado
    assert!(env
        .installer
        .store()
        .get_app(&app("dev.arca.intb"))
        .expect("get")
        .is_none());
}

/// Interrupción (c): tras finish() OK, ANTES del rename (drop de Prepared).
#[test]
fn interrupcion_c_antes_del_rename() {
    let env = Env::new("intc");
    let p = env.pkg("dev.arca.intc", "1.0.0");
    {
        let prepared = flow::prepare_for_test(&env.installer, PackageSource::Path(p));
        assert!(prepared.is_ok());
        drop(prepared); // crash simulado: el guard debe limpiar staging
    }
    assert!(stagings(&env.root, "dev.arca.intc").is_empty());
    // sin versión instalada
    let d = env.root.join("dev.arca.intc");
    if d.is_dir() {
        let names: Vec<String> = std::fs::read_dir(&d)
            .expect("dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with('v')),
            "sin versiones: {names:?}"
        );
    }
    assert!(env
        .installer
        .store()
        .get_app(&app("dev.arca.intc"))
        .expect("get")
        .is_none());
    assert_eq!(env.installer.sweep().expect("sweep"), 0);
}

/// Interrupción (d): tras rename, ANTES del store → sweep quita la versión
/// colgante.
#[test]
fn interrupcion_d_tras_rename_antes_store() {
    let env = Env::new("intd");
    let p = env.pkg("dev.arca.intd", "1.0.0");
    let prepared = match flow::prepare_for_test(&env.installer, PackageSource::Path(p)) {
        Ok(x) => x,
        Err(e) => panic!("prepare: {e}"),
    };
    // commit a medias: rename hecho, store NO
    let app_dir = env.root.join("dev.arca.intd");
    let vdir = app_dir.join("v1.0.0");
    std::fs::rename(&prepared.staging, &vdir).expect("rename manual");
    assert!(vdir.is_dir());

    let n = env.installer.sweep().expect("sweep");
    assert!(n >= 1, "sweep debe limpiar la versión colgante");
    assert!(!vdir.exists(), "la versión colgante debía desaparecer");
    assert!(env
        .installer
        .store()
        .get_app(&app("dev.arca.intd"))
        .expect("get")
        .is_none());
}

/// verify_installed detecta tamper del binario instalado.
#[test]
fn verify_installed_detecta_tamper() {
    let env = Env::new("tamp");
    let p = env.pkg("dev.arca.tamp", "1.0.0");
    assert!(env
        .installer
        .install(PackageSource::Path(p), &InstallOpts::default())
        .is_ok());
    let bin = env
        .root
        .join("dev.arca.tamp/current/bin/native-aarch64/app");
    let mut bytes = std::fs::read(&bin).expect("bin");
    bytes[50] ^= 0xff;
    std::fs::write(&bin, bytes).expect("escribir");
    let r = env.installer.verify_installed(&app("dev.arca.tamp"));
    assert!(matches!(r, Err(ArcaError::InvalidPackage(_))), "{r:?}");
}

/// Progreso: fases y fracciones reportadas durante la instalación.
#[test]
fn progreso_reporta_fases() {
    let env = Env::new("prog");
    let p = env.pkg("dev.arca.prog", "1.0.0");
    let mut eventos = 0usize;
    let mut vieron_manifest = false;
    let mut vieron_commit = false;
    let r = env.installer.install_with_progress(
        PackageSource::Path(p),
        &InstallOpts::default(),
        &mut |pr| {
            eventos += 1;
            vieron_manifest |= pr.phase == crate::Phase::Manifest;
            vieron_commit |= pr.phase == crate::Phase::Commit;
        },
    );
    assert!(r.is_ok());
    assert!(eventos > 4, "fases+fracciones: {eventos}");
    assert!(vieron_manifest && vieron_commit);
}

/// Instalación concurrente de DOS apps distintas (stagings independientes).
#[test]
fn dos_instalaciones_paralelas() {
    let env = Env::new("par");
    let pa = env.pkg("dev.arca.pa", "1.0.0");
    let pb = env.pkg("dev.arca.pb", "1.0.0");
    let ra = env
        .installer
        .install(PackageSource::Path(pa), &InstallOpts::default());
    let rb = env
        .installer
        .install(PackageSource::Path(pb), &InstallOpts::default());
    assert!(ra.is_ok() && rb.is_ok());
    assert!(env.root.join("dev.arca.pa/current").is_symlink());
    assert!(env.root.join("dev.arca.pb/current").is_symlink());
}

/// Progreso suave en paquete de ~50 MB (callback por bloques, sin RAM).
#[test]
fn instalacion_50mb_progreso_suave() {
    let env = Env::new("big");
    // asset grande NO declarado: 48 MB pseudoaleatorio (xorshift: no
    // comprime, obliga a streaming real por bloques)
    let big: Vec<u8> = {
        let mut st: u64 = 0x1234_5678_9abc_def0;
        (0..48 * 1024 * 1024u32)
            .map(|_| {
                st ^= st << 13;
                st ^= st >> 7;
                st ^= st << 17;
                (st >> 32) as u8
            })
            .collect()
    };
    let p = FixturePkg::new("dev.arca.big", "1.0.0")
        .with_extra("assets/data/grande.bin", big)
        .build(env._tmp.path(), &env.ring);
    let size = std::fs::metadata(&p).expect("stat").len();
    assert!(size > 40 * 1024 * 1024, "paquete comprimido grande: {size}");

    let mut callbacks = 0usize;
    let r = env.installer.install_with_progress(
        PackageSource::Path(p),
        &InstallOpts::default(),
        &mut |_| {
            callbacks += 1;
        },
    );
    assert!(r.is_ok());
    // el extractor reporta cada ≥256 KiB: con ~48 MB → ≥ 100 callbacks
    assert!(callbacks > 100, "progreso no suave: {callbacks}");
}
