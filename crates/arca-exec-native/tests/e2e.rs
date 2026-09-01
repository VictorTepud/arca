//! e2e del motor nativo F0-F1 (r2): spawn + sandbox básico + AIPC + vigilancia.
//!
//! Requisito: compilar ANTES la sub-app de prueba (estática, musl):
//!
//! ```text
//! rustup target add x86_64-unknown-linux-musl
//! cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl
//! cargo test -p arca-exec-native --test e2e -- --nocapture
//! ```
//!
//! Las 6 pruebas:
//! 1. `e2e_spawn_handshake_ping`            — ciclo feliz: spawn → ping → shutdown → exit 0
//! 2. `e2e_log_drenado_con_prefijo`         — logs de la sub-app drenados y etiquetados
//! 3. `e2e_100_spawns_sin_zombis`           — estrés: 100 lanzamientos, cero zombis
//! 4. `e2e_panic_de_la_app_exit_101`        — 🔧 pánico → exit code 101 (fallaba en r1)
//! 5. `e2e_spawn_handshake_ping_kill9_dead` — 🔧 SIGKILL → muerte detectada (fallaba en r1)
//! 6. `e2e_canal_cerrado_exit_0`            — cierre del canal → apagado solo y limpio

use std::path::PathBuf;
use std::time::{Duration, Instant};

use arca_exec_native::{Evento, Instancia, Modo, SpawnCfg};

/// Localiza el binario de la sub-app de prueba. Prefiere la versión estática
/// (musl) porque es la que imita lo que hará Android; si no está, acepta la
/// del host como último recurso.
fn bin_ping() -> PathBuf {
    let raiz = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("raíz del workspace")
        .to_path_buf();

    let musl = raiz.join("target/x86_64-unknown-linux-musl/debug/arca-ping");
    if musl.exists() {
        return musl;
    }
    let host = raiz.join("target/debug/arca-ping");
    if host.exists() {
        return host;
    }
    panic!(
        "No encontré el binario `arca-ping`. Compílalo antes con:\n  \
         rustup target add x86_64-unknown-linux-musl\n  \
         cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl"
    );
}

/// Lanza una instancia de dev.arca.ping en el modo pedido.
fn lanzar(modo: Modo) -> Instancia {
    arca_log::init();
    let cfg = SpawnCfg::new(bin_ping(), "dev.arca.ping").modo(modo);
    Instancia::lanzar(cfg).expect("lanzar dev.arca.ping")
}

/// Cuenta los procesos zombi cuyo nombre sea `arca-ping` (escaneo de /proc).
fn zombis_arca_ping() -> usize {
    let mut n = 0;
    let dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return 0,
    };
    for e in dir.flatten() {
        let nombre = e.file_name();
        let Some(pid) = nombre.to_str() else { continue };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        let Some(abre) = stat.find('(') else { continue };
        let Some(cierra) = stat.rfind(')') else { continue };
        if abre >= cierra {
            continue;
        }
        let comm = &stat[abre + 1..cierra];
        let estado = stat[cierra + 1..].trim_start();
        let estado = estado.split_whitespace().next().unwrap_or("");
        if estado == "Z" && comm == "arca-ping" {
            n += 1;
        }
    }
    n
}

/// Verifica que no queden zombis globales de arca-ping, con reintentos para
/// no chocar con instancias de otros tests corriendo en paralelo.
fn sin_zombis_globales() {
    for _ in 0..8 {
        if zombis_arca_ping() == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(zombis_arca_ping(), 0, "quedaron procesos zombi de arca-ping");
}

/// Verifica que un pid concreto ya no exista como zombi (el vigía lo
/// recolectó). `waitpid` devolviendo el propio pid significaría "sigue
/// muerto sin recolectar".
fn sin_zombi_concreto(pid: i32) {
    let r = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    assert_ne!(
        r, pid,
        "el proceso {pid} seguía en estado zombi: el vigía no lo recolectó"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 1) Ciclo feliz
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_spawn_handshake_ping() {
    let mut ins = lanzar(Modo::Serve);

    let d = ins.ping().expect("ping");
    assert!(d < Duration::from_secs(2), "ping demasiado lento: {d:?}");

    ins.apagar().expect("apagar");
    let ev = ins.finalizar(Duration::from_secs(5)).expect("salida");
    assert!(
        matches!(ev, Evento::Salida { code: 0, .. }),
        "esperaba Salida{{code:0}}, llegó {ev:?}"
    );

    let err = ins.stderr_texto();
    assert!(
        err.contains("Shutdown recibido") && err.contains("apagado limpio"),
        "stderr de la sub-app inesperado:\n{err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2) Drenado de logs
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_log_drenado_con_prefijo() {
    let mut ins = lanzar(Modo::Serve);
    std::thread::sleep(Duration::from_millis(150)); // darle tiempo a la sub-app a arrancar y loguear

    let err = ins.stderr_texto();
    assert!(
        err.contains("log de sub-app listo"),
        "el stderr debía traer la línea de arranque; llegó:\n{err}"
    );
    assert!(
        err.contains("instance="),
        "la línea de arranque debía llevar el número de instancia:\n{err}"
    );

    let _ = ins.ping();
    let _ = ins.apagar();
    let _ = ins.finalizar(Duration::from_secs(5));
}

// ─────────────────────────────────────────────────────────────────────────────
// 3) Estrés: 100 lanzamientos + apagados, sin zombis
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_100_spawns_sin_zombis() {
    let t0 = Instant::now();
    let mut pids = Vec::with_capacity(100);

    for _ in 0..100 {
        let mut ins = lanzar(Modo::Serve);
        ins.ping().expect("ping");
        ins.apagar().expect("apagar");
        let ev = ins.finalizar(Duration::from_secs(5)).expect("salida");
        assert!(
            matches!(ev, Evento::Salida { code: 0, .. }),
            "esperaba Salida{{code:0}}, llegó {ev:?}"
        );
        pids.push(ins.pid());
        drop(ins); // el Drop remonta los hilos y garantiza la recolección
    }

    let total = t0.elapsed();
    println!(
        "100 spawns+shutdown en {:.9}s ({:.6}ms/spawn)",
        total.as_secs_f64(),
        total.as_secs_f64() * 10.0
    );
    assert!(total < Duration::from_secs(30), "100 spawns tardaron demasiado: {total:?}");

    // El vigía recolectó a todos: ningún pid nuestro sigue en zombi…
    for pid in &pids {
        sin_zombi_concreto(*pid);
    }
    // …y en el sistema entero tampoco hay zombis de arca-ping.
    sin_zombis_globales();
}

// ─────────────────────────────────────────────────────────────────────────────
// 4) 🔧 Pánico de la sub-app → exit code 101   (fallaba en r1)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_panic_de_la_app_exit_101() {
    let mut ins = lanzar(Modo::Panic);

    let ev = ins
        .finalizar(Duration::from_secs(5))
        .expect("la sub-app en modo panic debía morir rápido");
    assert!(
        matches!(ev, Evento::Salida { code: 101, .. }),
        "esperaba Salida{{code:101}} (pánico de Rust en el hilo main), llegó {ev:?}"
    );

    let err = ins.stderr_texto();
    assert!(
        err.contains("panicked"),
        "el stderr debía contener el mensaje de pánico; llegó:\n{err}"
    );
    assert!(
        err.contains("boom controlado"),
        "el stderr debía contener nuestro mensaje de pánico; llegó:\n{err}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5) 🔧 spawn + handshake + ping + kill -9 → muerte detectada   (fallaba en r1)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_spawn_handshake_ping_kill9_dead() {
    let mut ins = lanzar(Modo::Serve);

    let d = ins.ping().expect("ping antes del kill");
    assert!(d < Duration::from_secs(2));

    ins.matar9(); // SIGKILL: no puede ser ignorado ni capturado

    let ev = ins
        .finalizar(Duration::from_secs(3))
        .expect("el vigía debía reportar la muerte por SIGKILL enseguida");
    assert!(
        matches!(ev, Evento::MuertoPorSenal { senal: 9, .. }),
        "esperaba MuertoPorSenal{{senal:9}}, llegó {ev:?}"
    );

    // El vigía ya lo recolectó: ese pid no puede seguir en estado zombi.
    sin_zombi_concreto(ins.pid());
    sin_zombis_globales();
}

// ─────────────────────────────────────────────────────────────────────────────
// 6) Cierre del canal sin SHUTDOWN → la sub-app se apaga sola (exit 0)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn e2e_canal_cerrado_exit_0() {
    let mut ins = lanzar(Modo::Serve);

    ins.cerrar_canal(); // sin mensaje de apagado: solo cerramos el socket

    let ev = ins
        .finalizar(Duration::from_secs(5))
        .expect("la sub-app debía notar el EOF del canal y salir sola");
    assert!(
        matches!(ev, Evento::Salida { code: 0, .. }),
        "esperaba Salida{{code:0}} al cerrarse el canal, llegó {ev:?}"
    );

    let err = ins.stderr_texto();
    assert!(
        err.contains("canal cerrado"),
        "la sub-app debía loguear que vio el canal cerrado; llegó:\n{err}"
    );
}
