//! Tests de integración de `arca-store` (spec 11 §6 + TASKS.json T07).
//!
//! Cubre: migraciones v0→v2 con datos preservados e idempotencia, `.bak`
//! pre-migración, versión futura rechazada, tx-rollback con error inyectado
//! a mitad (estado consistente), uninstall limpia filas hijas, instancias,
//! auditoría por app/tiempo y el bench de 10k inserts batched.
//!
//! NOTA sobre `update_crash_midway` (docs/14 §5): el escenario CANÓNICO
//! (crash entre rename/symlink y commit SQLite, con sweep del instalador)
//! es tarea de T08 (`arca-installer`), que coordina filesystem+store. Aquí
//! se prueba la mitad de store: transacción abortada a mitad → rollback →
//! db consistente (precondición del sweep).

use std::path::{Path, PathBuf};
use std::time::Instant;

use arca_pkg_model::Manifest;
use arca_store::{
    AppRecord, AuditEvent, Filter, InstallSource, InstanceRecord, Outcome, Store, UnixMs,
};
use arca_types::{AppId, ArcaError, Capability, InstanceId};
use rusqlite::{params, Connection};
use tempfile::TempDir;

// ---------------------------------------------------------------- helpers

/// Manifiesto válido con caps pedidas (granted al instalar).
fn manifest(id: &str, version: &str, caps: &[&str]) -> Manifest {
    let perms = if caps.is_empty() {
        String::new()
    } else {
        let lista = caps
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("perms = [{lista}]\n")
    };
    let toml = format!(
        "[package]\nid = \"{id}\"\nname = \"App {id}\"\nversion = \"{version}\"\n\
         min_host = \"1.0.0\"\napi_level = 1\ndescription = \"app de prueba\"\n\
         tags = [\"alpha\", \"tool\"]\n\n[runtime]\nbackend_pref = \"any\"\nentry = \"app\"\n\
         respawn = \"never\"\n{perms}\
         \n[artifacts.wasm]\npath = \"bin/wasm/app.wasm\"\n\
         sha256 = \"0202020202020202020202020202020202020202020202020202020202020202\"\n\n\
         [profile]\nlaunch_budget_ms = 60\nmax_frame_mb = 1\n"
    );
    Manifest::parse(toml.as_bytes()).unwrap()
}

/// Instala (install/update de registro) en una tx cometida.
fn install(store: &Store, m: &Manifest, src: InstallSource) {
    let mut tx = store.begin().unwrap();
    store.upsert_app(&mut tx, m, src).unwrap();
    tx.commit().unwrap();
}

fn app_id(s: &str) -> AppId {
    AppId::new(s).unwrap()
}

/// db nueva en tmpdir, abierta (y migrada) — la mayoría de tests empieza así.
fn db_fresh() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let store = Store::open(&db_path(&dir)).unwrap();
    (dir, store)
}

fn db_path(dir: &TempDir) -> PathBuf {
    dir.path().join("registry.db")
}

/// Conexión cruda de INSPECCIÓN (solo tests: ver filas directamente).
fn raw(path: &Path) -> Connection {
    Connection::open(path).unwrap()
}

fn contar(path: &Path, sql: &str, arg: &str) -> i64 {
    raw(path)
        .query_row(sql, params![arg], |r| r.get(0))
        .unwrap()
}

fn user_version_raw(path: &Path) -> u32 {
    raw(path)
        .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
        .unwrap() as u32
}

/// DDL de v1 exacto (espejo de `schema.rs`) para simular una db vieja.
const DDL_V1: &str = "
CREATE TABLE IF NOT EXISTS apps (
    id TEXT PRIMARY KEY, name TEXT NOT NULL, version TEXT NOT NULL,
    min_host TEXT NOT NULL, api_level INTEGER NOT NULL,
    description TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT '',
    installed_from TEXT NOT NULL, installed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_caps (
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    cap TEXT NOT NULL, granted_at INTEGER NOT NULL,
    PRIMARY KEY (app_id, cap)
);
CREATE TABLE IF NOT EXISTS instances (
    instance_id INTEGER PRIMARY KEY,
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    version TEXT NOT NULL, started_at INTEGER NOT NULL,
    exited_at INTEGER, outcome TEXT
);
CREATE INDEX IF NOT EXISTS idx_instances_app ON instances(app_id, started_at);
CREATE TABLE IF NOT EXISTS audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT, app_id TEXT NOT NULL,
    cap TEXT NOT NULL, ts INTEGER NOT NULL, detail TEXT NOT NULL DEFAULT ''
);
";

/// db en v1 con UNA app + caps + instancia + auditoría (datos a preservar).
fn db_v1_con_datos(dir: &TempDir, version: u32) -> PathBuf {
    let path = db_path(dir);
    let conn = raw(&path);
    conn.execute_batch(DDL_V1).unwrap();
    conn.execute(
        "INSERT INTO apps (id, name, version, min_host, api_level, description, tags, \
         installed_from, installed_at) \
         VALUES ('dev.arca.vieja', 'Vieja', '0.9.0', '1.0.0', 1, 'desc', 'a,b', 'user', 1111)",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO app_caps VALUES ('dev.arca.vieja', 'net-client', 1111);
         INSERT INTO app_caps VALUES ('dev.arca.vieja', 'notify', 1111);
         INSERT INTO instances (instance_id, app_id, version, started_at) \
           VALUES (7, 'dev.arca.vieja', '0.9.0', 2222);
         INSERT INTO audit_log (app_id, cap, ts, detail) \
           VALUES ('dev.arca.vieja', 'net-client', 3333, 'connect tcp:443');
         INSERT INTO audit_log (app_id, cap, ts, detail) \
           VALUES ('dev.arca.vieja', 'notify', 4444, 'notify');",
    )
    .unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))
        .unwrap();
    path
}

fn instancia(app: &AppId, iid: u64, version: &str) -> InstanceRecord {
    InstanceRecord {
        instance_id: InstanceId::new(iid),
        app_id: app.clone(),
        version: version.to_string(),
        started_at: UnixMs::from_millis(5000),
    }
}

fn evento(app: &AppId, cap: Capability, ts: i64) -> AuditEvent {
    AuditEvent {
        app_id: app.clone(),
        cap,
        ts: UnixMs::from_millis(ts),
        detail: "connect tcp:443".to_string(),
    }
}

// ---------------------------------------------------- migraciones + .bak

/// v0 (archivo nuevo) → v2, y re-abrir no re-migra ni crea .bak.
#[test]
fn open_v0_migra_a_v2_e_idempotente() {
    let (dir, store) = db_fresh();
    let path = db_path(&dir);
    drop(store); // cerrar para inspeccionar en frío

    assert_eq!(user_version_raw(&path), 2);
    // v0 es archivo nuevo: no había nada que respaldar.
    assert!(!dir.path().join("registry.db.bak").exists());

    // Re-abrir: idempotente (mismo número, sin migración, sin .bak).
    let store = Store::open(&path).unwrap();
    let m = manifest("dev.arca.x", "1.0.0", &["net.client"]);
    install(&store, &m, InstallSource::User);
    drop(store);
    assert_eq!(user_version_raw(&path), 2);
    assert!(!dir.path().join("registry.db.bak").exists());
    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM apps WHERE id = ?1",
            "dev.arca.x"
        ),
        1
    );
}

/// v1 → v2 con datos: las filas preexistentes quedan intactas y
/// `updated_at` se backfill-ea con `installed_at`.
#[test]
fn migracion_v1_a_v2_preserva_datos() {
    let dir = TempDir::new().unwrap();
    let path = db_v1_con_datos(&dir, 1);

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version_raw(&path), 2);

    // Datos preservados + legibles por la API.
    let rec = store.get_app(&app_id("dev.arca.vieja")).unwrap().unwrap();
    assert_eq!(rec.version, "0.9.0");
    assert_eq!(rec.name, "Vieja");
    assert_eq!(rec.installed_from, InstallSource::User);
    assert_eq!(rec.installed_at, UnixMs::from_millis(1111));
    assert_eq!(rec.updated_at, UnixMs::from_millis(1111)); // backfill de v2
    assert_eq!(rec.tags, vec!["a".to_string(), "b".to_string()]);
    drop(store);
}

/// v0 CON tablas y datos (db pre-versionado) → v2 igual, sin perder nada.
#[test]
fn migracion_v0_con_datos_a_v2() {
    let dir = TempDir::new().unwrap();
    let path = db_v1_con_datos(&dir, 0);

    let store = Store::open(&path).unwrap();
    assert_eq!(user_version_raw(&path), 2);
    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM app_caps WHERE app_id = ?1",
            "dev.arca.vieja"
        ),
        2
    );
    let n = store
        .query_audit(&app_id("dev.arca.vieja"), UnixMs::from_millis(0))
        .unwrap()
        .len();
    assert_eq!(n, 2);
}

/// Antes de migrar una db con datos se escribe `<db>.bak` (esquema viejo).
#[test]
fn bak_se_crea_antes_de_migrar() {
    let dir = TempDir::new().unwrap();
    let path = db_v1_con_datos(&dir, 1);

    let store = Store::open(&path).unwrap();
    let bak = dir.path().join("registry.db.bak");
    assert!(bak.exists(), "falta el .bak pre-migración");
    drop(store);

    // El .bak es el estado PRE-migración (v1 con su fila).
    assert_eq!(user_version_raw(&bak), 1);
    assert_eq!(
        contar(
            &bak,
            "SELECT COUNT(*) FROM apps WHERE id = ?1",
            "dev.arca.vieja"
        ),
        1
    );
    // El original quedó en v2 (migrado).
    assert_eq!(user_version_raw(&path), 2);
}

/// db de versión futura → error ruidoso, sin downgrade ni .bak.
#[test]
fn version_futura_rechazada() {
    let (dir, store) = db_fresh();
    let path = db_path(&dir);
    drop(store);
    let conn = raw(&path);
    conn.execute_batch("PRAGMA user_version = 99").unwrap();
    drop(conn);

    match Store::open(&path) {
        Err(ArcaError::Internal(ctx)) => assert!(ctx.contains("futuro"), "{ctx}"),
        otro => panic!("debía fallar con Internal, vino {otro:?}"),
    }
    assert_eq!(user_version_raw(&path), 99); // intacto
    assert!(!dir.path().join("registry.db.bak").exists());
}

// ------------------------------------------------ flujo install/update

/// Install → update → uninstall del registro, con caps sincronizadas.
#[test]
fn flujo_install_update_uninstall() {
    let (_dir, store) = db_fresh();
    let id = app_id("dev.arca.flujo");

    // Install 1.0.0 con 2 caps pedidas.
    install(
        &store,
        &manifest("dev.arca.flujo", "1.0.0", &["net.client", "notify"]),
        InstallSource::User,
    );
    let rec = store.get_app(&id).unwrap().unwrap();
    assert_eq!(rec.version, "1.0.0");
    assert_eq!(rec.installed_from, InstallSource::User);
    assert_eq!(rec.name, "App dev.arca.flujo");
    assert_eq!(rec.min_host, "1.0.0");
    assert_eq!(rec.api_level, 1);
    assert_eq!(rec.description, "app de prueba");
    assert_eq!(rec.tags, vec!["alpha".to_string(), "tool".to_string()]);
    assert_eq!(store.caps_of(&id).unwrap().len(), 2);

    // Update 1.1.0: el manifest ya no pide notify.
    install(
        &store,
        &manifest("dev.arca.flujo", "1.1.0", &["net.client"]),
        InstallSource::User,
    );
    let rec: AppRecord = store.get_app(&id).unwrap().unwrap();
    assert_eq!(rec.version, "1.1.0");
    assert!(rec.updated_at >= rec.installed_at); // v2
    let caps = store.caps_of(&id).unwrap();
    assert!(caps.contains(Capability::NetClient));
    assert!(!caps.contains(Capability::Notify)); // retirada del manifest
    assert_eq!(caps.len(), 1);

    // Uninstall (delete del registro).
    let mut tx = store.begin().unwrap();
    store.delete_app(&mut tx, &id).unwrap();
    tx.commit().unwrap();
    assert!(store.get_app(&id).unwrap().is_none());
    // Segundo delete → NotFound (no idempotente a ciegas: decide el caller).
    let mut tx = store.begin().unwrap();
    match store.delete_app(&mut tx, &id) {
        Err(ArcaError::NotFound(x)) => assert_eq!(x, id),
        otro => panic!("debía ser NotFound, vino {otro:?}"),
    }
    tx.rollback().unwrap();
}

/// Filtros de listado: por capability y por origen.
#[test]
fn list_apps_con_filtro() {
    let (_dir, store) = db_fresh();
    install(
        &store,
        &manifest("dev.arca.a", "1.0.0", &["net.client"]),
        InstallSource::User,
    );
    install(
        &store,
        &manifest("dev.arca.b", "1.0.0", &["notify"]),
        InstallSource::Dev,
    );
    install(
        &store,
        &manifest("dev.arca.c", "1.0.0", &[]),
        InstallSource::User,
    );

    let todos = store.list_apps(Filter::all()).unwrap();
    assert_eq!(todos.len(), 3);
    // Orden por nombre (determinista para el launcher).
    let nombres: Vec<&str> = todos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        nombres,
        vec!["App dev.arca.a", "App dev.arca.b", "App dev.arca.c"]
    );

    let con_net = store
        .list_apps(Filter::all().with_cap(Capability::NetClient))
        .unwrap();
    assert_eq!(con_net.len(), 1);
    assert_eq!(con_net[0].id, app_id("dev.arca.a"));

    let dev = store
        .list_apps(Filter::all().from(InstallSource::Dev))
        .unwrap();
    assert_eq!(dev.len(), 1);
    assert_eq!(dev[0].id, app_id("dev.arca.b"));

    let ambos = store
        .list_apps(
            Filter::all()
                .from(InstallSource::User)
                .with_cap(Capability::Notify),
        )
        .unwrap();
    assert_eq!(ambos.len(), 0);
}

// -------------------------------------------------------- caps grant/revoke

/// Grant/revoke + idempotencia de revoke + NotFound en app inexistente.
#[test]
fn caps_grant_revoke() {
    let (_dir, store) = db_fresh();
    let id = app_id("dev.arca.caps");
    install(
        &store,
        &manifest("dev.arca.caps", "1.0.0", &["notify"]),
        InstallSource::User,
    );

    // Grant extra (usuario concede luego de instalar).
    let mut tx = store.begin().unwrap();
    store
        .grant_caps(
            &mut tx,
            &id,
            &[Capability::ClipboardRead, Capability::Share],
        )
        .unwrap();
    tx.commit().unwrap();
    let caps = store.caps_of(&id).unwrap();
    assert_eq!(caps.len(), 3);
    assert!(caps.contains(Capability::ClipboardRead));

    // Grant repetido: idempotente.
    let mut tx = store.begin().unwrap();
    store
        .grant_caps(&mut tx, &id, &[Capability::ClipboardRead])
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(store.caps_of(&id).unwrap().len(), 3);

    // Revoke.
    let mut tx = store.begin().unwrap();
    store
        .revoke_cap(&mut tx, &id, Capability::ClipboardRead)
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(store.caps_of(&id).unwrap().len(), 2);

    // Revoke de algo ya revocado: Ok (idempotente).
    let mut tx = store.begin().unwrap();
    store
        .revoke_cap(&mut tx, &id, Capability::ClipboardRead)
        .unwrap();
    tx.commit().unwrap();

    // caps_of / grant de app inexistente → NotFound.
    match store.caps_of(&app_id("dev.arca.nope")) {
        Err(ArcaError::NotFound(_)) => {}
        otro => panic!("debía ser NotFound, vino {otro:?}"),
    }
    let mut tx = store.begin().unwrap();
    match store.grant_caps(&mut tx, &app_id("dev.arca.nope"), &[Capability::Notify]) {
        Err(ArcaError::NotFound(_)) => {}
        otro => panic!("debía ser NotFound, vino {otro:?}"),
    }
    tx.rollback().unwrap();
}

// ------------------------------------------------------------- tx rollback

/// Error inyectado a MITAD de la tx → rollback → estado pre-tx consistente.
///
/// Invariante spec 11 §4: la install/update va en UNA tx; si algo falla
/// después de pasos exitosos, NADA de esa tx queda a medias.
#[test]
fn tx_rollback_con_error_inyectado() {
    let (_dir, store) = db_fresh();
    let a = app_id("dev.arca.a");
    let b = app_id("dev.arca.b");
    install(
        &store,
        &manifest("dev.arca.a", "1.0.0", &["net.client"]),
        InstallSource::User,
    );

    // Tx: upsert de B OK → grant a app inexistente (error inyectado) →
    // la tx se cae con `?` y se dropea → rollback.
    let mut tx = store.begin().unwrap();
    store
        .upsert_app(
            &mut tx,
            &manifest("dev.arca.b", "1.0.0", &[]),
            InstallSource::User,
        )
        .unwrap();
    let fallo = store.grant_caps(&mut tx, &app_id("dev.arca.fantasma"), &[Capability::Notify]);
    assert!(matches!(fallo, Err(ArcaError::NotFound(_))));
    drop(tx); // Drop → ROLLBACK (simula crash pre-commit del proceso)

    // NADA de la tx sobrevive: B ausente, A intacta con sus caps.
    assert!(store.get_app(&b).unwrap().is_none());
    assert!(store.get_app(&a).unwrap().is_some());
    assert_eq!(store.caps_of(&a).unwrap().len(), 1);

    // Y la db queda USABLE (nueva tx funciona).
    install(
        &store,
        &manifest("dev.arca.b", "1.0.0", &[]),
        InstallSource::Dev,
    );
    assert!(store.get_app(&b).unwrap().is_some());
}

/// Rollback tras pasos destructivos: delete dentro de tx abortada NO aplica.
#[test]
fn tx_rollback_rescata_delete() {
    let (_dir, store) = db_fresh();
    let a = app_id("dev.arca.a");
    install(
        &store,
        &manifest("dev.arca.a", "1.0.0", &["net.client", "notify"]),
        InstallSource::User,
    );

    // Tx: delete A OK → grant a A (ya borrada DENTRO de la tx) → NotFound.
    let mut tx = store.begin().unwrap();
    store.delete_app(&mut tx, &a).unwrap();
    let fallo = store.grant_caps(&mut tx, &a, &[Capability::Share]);
    assert!(matches!(fallo, Err(ArcaError::NotFound(_))));
    tx.rollback().unwrap();

    // A sigue instalada con sus 2 caps (el delete era parte de la tx muerta).
    let rec = store.get_app(&a).unwrap().unwrap();
    assert_eq!(rec.version, "1.0.0");
    assert_eq!(store.caps_of(&a).unwrap().len(), 2);
}

/// Commit explícito OK: lo de dentro de la tx SÍ queda.
#[test]
fn tx_commit_persiste() {
    let (_dir, store) = db_fresh();
    let mut tx = store.begin().unwrap();
    store
        .upsert_app(
            &mut tx,
            &manifest("dev.arca.c", "2.0.0", &["notify"]),
            InstallSource::Bundled,
        )
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        store
            .get_app(&app_id("dev.arca.c"))
            .unwrap()
            .unwrap()
            .version,
        "2.0.0"
    );
}

// ------------------------------------------------------------- instancias

/// Spawn + fin de instancias; doble-fin e id desconocido → error.
#[test]
fn instancias_spawn_y_fin() {
    let (dir, store) = db_fresh();
    let id = app_id("dev.arca.inst");
    install(
        &store,
        &manifest("dev.arca.inst", "1.0.0", &[]),
        InstallSource::User,
    );

    store
        .register_instance(&instancia(&id, 1, "1.0.0"))
        .unwrap();
    store
        .register_instance(&instancia(&id, 2, "1.0.0"))
        .unwrap();
    store
        .finish_instance(InstanceId::new(1), Outcome::Exited { code: 0 })
        .unwrap();

    let path = db_path(&dir);
    // Instancia 1: cerrada con outcome canónico.
    let (salida, outcome): (Option<i64>, Option<String>) = raw(&path)
        .query_row(
            "SELECT exited_at, outcome FROM instances WHERE instance_id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(salida.is_some());
    assert_eq!(outcome.as_deref(), Some("exit:0"));

    // Instancia 2: sigue "corriendo" (histórico).
    let corre: i64 = raw(&path)
        .query_row(
            "SELECT COUNT(*) FROM instances WHERE instance_id = 2 AND exited_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(corre, 1);

    // Doble fin → error (docs/14 §5 double_shutdown: nunca silencioso).
    assert!(store
        .finish_instance(InstanceId::new(1), Outcome::Killed)
        .is_err());
    // Id desconocido → error.
    assert!(store
        .finish_instance(InstanceId::new(999), Outcome::Crashed)
        .is_err());

    // Crash persiste su forma canónica.
    store
        .finish_instance(InstanceId::new(2), Outcome::Crashed)
        .unwrap();
    let outcome: String = raw(&path)
        .query_row(
            "SELECT outcome FROM instances WHERE instance_id = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "crash");
}

// ---------------------------------------------------------------- auditoría

/// Append único + query por app y tiempo (desde inclusive, orden ascendente).
#[test]
fn audit_append_y_query_por_app_y_tiempo() {
    let (_dir, store) = db_fresh();
    let a = app_id("dev.arca.audit");
    let otra = app_id("dev.arca.otra");
    install(
        &store,
        &manifest("dev.arca.audit", "1.0.0", &["net.client", "notify"]),
        InstallSource::User,
    );
    install(
        &store,
        &manifest("dev.arca.otra", "1.0.0", &["notify"]),
        InstallSource::User,
    );

    store
        .audit(&evento(&a, Capability::NetClient, 100))
        .unwrap();
    store.audit(&evento(&a, Capability::Notify, 200)).unwrap();
    store
        .audit(&evento(&otra, Capability::Notify, 150))
        .unwrap(); // otra app
    store
        .audit(&evento(&a, Capability::NetClient, 300))
        .unwrap();
    // Empate de ts con el evento de Notify: desempata el orden de inserción.
    store.audit(&evento(&a, Capability::Share, 200)).unwrap();

    // Desde 200 (inclusive): 3 eventos de `a`, ninguno de `otra`.
    let evs = store.query_audit(&a, UnixMs::from_millis(200)).unwrap();
    assert_eq!(evs.len(), 3);
    let tss: Vec<i64> = evs.iter().map(|e| e.ts.get()).collect();
    assert_eq!(tss, vec![200, 200, 300]);
    // Empate 200: primero el insertado antes (Notify, luego Share).
    assert_eq!(evs[0].cap, Capability::Notify);
    assert_eq!(evs[1].cap, Capability::Share);
    assert_eq!(evs[2].cap, Capability::NetClient);
    assert_eq!(evs[2].detail, "connect tcp:443");

    // Desde 0: todo.
    assert_eq!(
        store.query_audit(&a, UnixMs::from_millis(0)).unwrap().len(),
        4
    );
    // La otra app solo ve lo suyo.
    assert_eq!(
        store
            .query_audit(&otra, UnixMs::from_millis(0))
            .unwrap()
            .len(),
        1
    );
}

/// Bench spec 11 §6: 10k inserts de audit batched.
///
/// Presupuesto de la spec: **100 ms**. El assert es HOLGADO (×3) porque el
/// tiempo real depende del host de CI: el número medido se imprime y se
/// reporta en el worklog (en dev con `opt-level=1` del workspace y SQLite
/// bundled queda muy por debajo del presupuesto).
#[test]
fn audit_batch_10k_bajo_presupuesto() {
    const N: usize = 10_000;
    const PRESUPUESTO_MS: u128 = 100; // spec 11 §6
    const ASERTO_MS: u128 = 300; // ×3 de holgura para CI ruidoso

    let (_dir, store) = db_fresh();
    let a = app_id("dev.arca.bench");
    install(
        &store,
        &manifest("dev.arca.bench", "1.0.0", &["net.client"]),
        InstallSource::User,
    );

    let eventos: Vec<AuditEvent> = (0..N)
        .map(|i| evento(&a, Capability::NetClient, 1_000 + i as i64))
        .collect();

    let t0 = Instant::now();
    store.audit_batch(&eventos).unwrap();
    let elapsed = t0.elapsed();

    println!(
        "audit_batch {N} inserts: {:.2} ms (presupuesto spec: {PRESUPUESTO_MS} ms)",
        elapsed.as_millis()
    );
    assert!(
        elapsed.as_millis() < ASERTO_MS,
        "10k batched tardó {} ms (presupuesto x3 = {ASERTO_MS} ms)",
        elapsed.as_millis()
    );

    // La vía batch dejó todo dentro (N filas legibles por la query).
    let n = store.query_audit(&a, UnixMs::from_millis(0)).unwrap().len();
    assert_eq!(n, N);
}

/// Batch vacío: no rompe (flush de cola vacía del broker).
#[test]
fn audit_batch_vacio_ok() {
    let (_dir, store) = db_fresh();
    store.audit_batch(&[]).unwrap();
}

// -------------------------------------------------------- cascadas uninstall

/// Uninstall limpia las filas hijas (caps + instancias); audit sobrevive.
#[test]
fn uninstall_limpia_filas_hijas() {
    let (dir, store) = db_fresh();
    let a = app_id("dev.arca.hija");
    let b = app_id("dev.arca.sana");
    install(
        &store,
        &manifest("dev.arca.hija", "1.0.0", &["net.client"]),
        InstallSource::User,
    );
    install(
        &store,
        &manifest("dev.arca.sana", "1.0.0", &["notify"]),
        InstallSource::User,
    );

    // Hijas de A: caps extra, instancia y auditoría.
    let mut tx = store.begin().unwrap();
    store.grant_caps(&mut tx, &a, &[Capability::Share]).unwrap();
    tx.commit().unwrap();
    store
        .register_instance(&instancia(&a, 42, "1.0.0"))
        .unwrap();
    store.audit(&evento(&a, Capability::NetClient, 10)).unwrap();
    store.audit(&evento(&b, Capability::Notify, 10)).unwrap();

    let path = db_path(&dir);
    let mut tx = store.begin().unwrap();
    store.delete_app(&mut tx, &a).unwrap();
    tx.commit().unwrap();

    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM app_caps WHERE app_id = ?1",
            "dev.arca.hija"
        ),
        0
    );
    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM instances WHERE app_id = ?1",
            "dev.arca.hija"
        ),
        0
    );
    // Append-only: la auditoría de la app DESINSTALADA sobrevive (evidencia).
    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM audit_log WHERE app_id = ?1",
            "dev.arca.hija"
        ),
        1
    );
    // La otra app quedó intacta (cascada bien delimitada).
    assert_eq!(
        contar(
            &path,
            "SELECT COUNT(*) FROM app_caps WHERE app_id = ?1",
            "dev.arca.sana"
        ),
        1
    );
    assert!(store.get_app(&b).unwrap().is_some());
}

// ------------------------------------------------------------ misc/contrato

/// WAL queda activado en el archivo (ADR-011; lecturas del launcher por
/// otra conexión).
#[test]
fn wal_activado_en_archivo() {
    let (dir, store) = db_fresh();
    let _ = store.list_apps(Filter::all()).unwrap();
    let modo: String = raw(&db_path(&dir))
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(modo, "wal");
}

/// Contrato de concurrency: Store usable desde varios hilos (Send+Sync).
/// (`Tx` es deliberadamente !Send: el guard del mutex no cruza hilos.)
#[test]
fn store_es_send_y_sync() {
    fn exige<T: Send + Sync>() {}
    exige::<Store>();
}

/// Tipos Debug sin pánico (logs de diagnóstico).
#[test]
fn tipos_debug_sin_panico() {
    let (_dir, store) = db_fresh();
    let tx = store.begin().unwrap();
    let _s = format!("{tx:?}");
    drop(tx);
    let _s = format!("{:?}", store.caps_of(&app_id("dev.arca.nada")));
}
