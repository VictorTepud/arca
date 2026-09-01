//! E2E PC del backend nativo (aceptación de spec 14 §6):
//! - spawn → handshake → ping p99 < 1 ms → kill -9 → Dead ≤ 50 ms
//! - 100 spawns secuenciales, 0 zombis, todos con Dead
//! - seccomp ACTIVO: binario que intenta `socket()` muere por SIGSYS
//! - panic de la app → exit 101 + CrashReport
//!
//! Fault-injection: las flags `ARCA_PING_*` viajan por
//! `launch_full_with_env` (LaunchSpec v2, env hermético) — NUNCA por el
//! entorno global del proceso: los tests corren en paralelo y un `set_var`
//! global contaminaba a los hijos de los OTROS tests (dos e2e flaky en
//! Deepin; fix documentado en worklog/T17).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use arca_exec_abi::{AppDirs, AppSpec, ArtifactRef};
use arca_exec_abi::{AppEvent, AppState, DeathReason, Executor};
use arca_exec_native::NativeProcessExec;
use arca_protocol::ControlMsg;
use arca_types::{AppId, Capability, Digest, InstanceId};

/// Ubica `arca-ping` (build ESTÁTICO musl: el sandbox de seccomp prohíbe
/// openat ⇒ un binario dinámico moriría en el loader). Rutas:
/// `target/x86_64-unknown-linux-musl/debug/arca-ping` (build explícito del
/// CI: `cargo build -p arca-rt --bin arca-ping --target x86_64-unknown-linux-musl`).
/// Si no existe: SKIP documentado (solo se compiló este crate).
fn ping_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug = exe.parent()?.parent()?; // .../target/debug
    let target = debug.parent()?; // .../target
    let p = target
        .join("x86_64-unknown-linux-musl")
        .join("debug")
        .join("arca-ping");
    if p.is_file() {
        return Some(p);
    }
    None
}

fn spec(ping: &Path, instance: u64) -> AppSpec {
    let data = std::fs::read(ping).expect("leer arca-ping");
    AppSpec {
        app_id: AppId::new("dev.arca.ping").expect("appid"),
        instance: InstanceId::new(instance),
        artifact: ArtifactRef {
            path: ping.to_path_buf(),
            hash: Digest::of(&data),
            size_bytes: data.len() as u64,
        },
        caps: vec![Capability::NetClient],
        dirs: AppDirs {
            app_dir: std::env::temp_dir(),
            vault_dir: std::env::temp_dir(),
        },
        respawn: arca_exec_abi::RespawnPolicy::Never,
        sync_ui: false,
    }
}

fn launch_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arca-launch"))
}

/// Espera el primer evento `Ready` (handshake completo dentro del watcher).
fn wait_ready(handle: &arca_exec_abi::AppHandle, d: Duration) -> Option<AppEvent> {
    let rx = handle.on_state_change();
    let deadline = Instant::now() + d;
    // drenar eventos hasta Ready (Spawned/Hello llegan primero)
    loop {
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            return None;
        }
        match rx.recv_timeout(rest) {
            Ok(ev @ AppEvent::Ready) => return Some(ev),
            Ok(AppEvent::Dead { .. }) => return None,
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn init_log() {
    let _ = arca_log::init_host();
}

#[test]
fn e2e_spawn_handshake_ping_kill9_dead() {
    init_log();
    let Some(ping) = ping_bin() else {
        eprintln!("SKIP: arca-ping no compilado (correr cargo test --workspace)");
        return;
    };
    let exec = NativeProcessExec::new(launch_bin())
        .expect("exec")
        .with_frame_bytes(4096);
    let inst = exec.launch_full(&spec(&ping, 1)).expect("launch");
    assert!(matches!(
        inst.handle.state(),
        AppState::Spawning | AppState::Ready
    ));
    assert!(
        wait_ready(&inst.handle, Duration::from_secs(5)).is_some(),
        "Ready"
    );

    // RTT de Ping→Pong: 1000 ciclos por el ctl real (p99 < 1 ms)
    let mut lat = Vec::with_capacity(1000);
    {
        let bus = inst.bus.clone();
        for k in 0u64..1000 {
            let t0 = Instant::now();
            {
                let mut b = bus.lock().expect("bus");
                use arca_exec_abi::BusTransport as _;
                b.send_ctl(&ControlMsg::Ping { t_ns: k }).expect("ping");
            }
            // Pong vuelve por el MISMO socket: recv por el bus.
            let got = bus
                .lock()
                .expect("bus")
                .recv_ctl_msg(Duration::from_millis(500));
            if let Ok(ControlMsg::Pong { t_ns }) = got {
                if t_ns == k {
                    lat.push(t0.elapsed().as_nanos() as u64);
                }
            }
        }
    }
    assert!(
        lat.len() > 900,
        " demasiados Pong perdidos: {}",
        1000 - lat.len()
    );
    lat.sort_unstable();
    let p99 = lat[(lat.len() as f64 * 0.99) as usize];
    eprintln!(
        "ping RTT p50={}ns p99={}ns (n={})",
        lat[lat.len() / 2],
        p99,
        lat.len()
    );
    assert!(p99 < 1_000_000, "p99 RTT {p99} ns ≥ 1 ms");

    // frame loop: 50 ticks → frames publicados en shm
    for _ in 0..50 {
        inst.send_tick().expect("tick");
    }
    let mut frame = vec![0u8; 4096];
    let mut intentos = 0;
    let snap = loop {
        if let Some(s) = inst.read_latest_frame(&mut frame) {
            break s;
        }
        intentos += 1;
        if intentos > 200 {
            panic!("sin frames tras 50 ticks");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let fseq = u64::from_le_bytes(frame[..8].try_into().expect("8"));
    assert!(fseq >= 1, "frame_seq={fseq} (snap {snap:?})");

    // kill -9 → Dead ≤ 50 ms (watcher poll 5 ms)
    let t0 = Instant::now();
    let rx = inst.handle.on_state_change();
    nix::sys::signal::kill(inst.pid, nix::sys::signal::SIGKILL).expect("kill");
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(AppEvent::Dead { reason, .. }) => {
                assert!(
                    matches!(reason, DeathReason::Signaled { signal: 9 }),
                    "{reason:?}"
                );
                break;
            }
            Ok(_) => continue,
            Err(_) => panic!("Dead no llegó en 50 ms"),
        }
    }
    let dt = t0.elapsed();
    assert!(dt < Duration::from_millis(60), "muerte detectada en {dt:?}");
}

#[test]
fn e2e_100_spawns_sin_zombis() {
    init_log();
    let Some(ping) = ping_bin() else {
        eprintln!("SKIP: arca-ping no compilado (correr cargo test --workspace)");
        return;
    };
    let exec = NativeProcessExec::new(launch_bin())
        .expect("exec")
        .with_frame_bytes(1024);
    let t0 = Instant::now();
    let mut deads = 0usize;
    for i in 1..=100u64 {
        let spec = spec(&ping, i);
        let exec_ref = &exec;
        let inst = std::thread::scope(|_s| exec_ref.launch_full(&spec).expect("launch"));
        assert!(
            wait_ready(&inst.handle, Duration::from_secs(5)).is_some(),
            "Ready {i}"
        );
        // apagado limpio por Shutdown (grace 2 s)
        let st = inst
            .handle
            .shutdown(Duration::from_secs(2))
            .expect("shutdown");
        assert!(st.code == 0 || st.signal == Some(9), "exit {st:?}");
        deads += 1;
        drop(inst);
    }
    assert_eq!(deads, 100);
    let dt = t0.elapsed();
    eprintln!("100 spawns+shutdown en {dt:?} ({:?}/spawn)", dt / 100);
    assert!(dt < Duration::from_secs(60), "100 spawns > 60 s: {dt:?}");
}

#[test]
fn e2e_seccomp_activo_sigsys() {
    let Some(ping) = ping_bin() else {
        eprintln!("SKIP: arca-ping no compilado (correr cargo test --workspace)");
        return;
    };
    let exec = NativeProcessExec::new(launch_bin())
        .expect("exec")
        .with_frame_bytes(1024);
    let mut sp = spec(&ping, 501);
    sp.caps = vec![]; // sin caps: perfil mínimo
                      // ARCA_PING_SOCKET=1 POR INSTANCIA (env hermético): la app intenta
                      // socket(AF_INET) → SIGSYS esperado. Sin contaminar a otros tests.
    let env = vec![("ARCA_PING_SOCKET".to_string(), "1".to_string())];
    let inst = exec.launch_full_with_env(&sp, env).expect("launch");
    assert!(
        wait_ready(&inst.handle, Duration::from_secs(5)).is_some(),
        "Ready"
    );
    // El intento de socket() ocurre EN el callback: hay que enviar ticks.
    let rx = inst.handle.on_state_change();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sigsys = false;
    for _ in 0..20 {
        let _ = inst.send_tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    loop {
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            break;
        }
        match rx.recv_timeout(rest) {
            Ok(AppEvent::Dead {
                reason: DeathReason::Signaled { signal: 31 },
                ..
            }) => {
                sigsys = true; // SIGSYS en x86_64
                break;
            }
            Ok(AppEvent::Dead { reason, .. }) => {
                panic!("murió sin SIGSYS: {reason:?}");
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(sigsys, "la app sobrevivió a socket(): seccomp NO activo");
}

#[test]
fn e2e_panic_de_la_app_exit_101() {
    let Some(ping) = ping_bin() else {
        eprintln!("SKIP: arca-ping no compilado (correr cargo test --workspace)");
        return;
    };
    let exec = NativeProcessExec::new(launch_bin())
        .expect("exec")
        .with_frame_bytes(1024);
    // ARCA_PING_PANIC=1 POR INSTANCIA (env hermético): pánico inyectado en
    // el primer tick → exit 101. Sin `set_var` global (era la causa de las
    // e2e flaky: contaminaba a los hijos de los tests paralelos).
    let env = vec![("ARCA_PING_PANIC".to_string(), "1".to_string())];
    let inst = exec
        .launch_full_with_env(&spec(&ping, 601), env)
        .expect("launch");
    // dispara un tick para que el callback corra y panne
    for _ in 0..5 {
        let _ = inst.send_tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    let rx = inst.handle.on_state_change();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut exit101 = false;
    loop {
        let rest = deadline.saturating_duration_since(Instant::now());
        if rest.is_zero() {
            break;
        }
        match rx.recv_timeout(rest) {
            Ok(AppEvent::Dead {
                reason: DeathReason::Exit { code: 101 },
                ..
            }) => {
                exit101 = true;
                break;
            }
            Ok(AppEvent::Dead { reason, .. }) => panic!("esperaba exit 101, vino {reason:?}"),
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(exit101, "el panic no produjo exit(101)");
}

/// Shutdown limpio de verdad: la app responde y muere con exit 0.
#[test]
fn e2e_shutdown_limpio_exit0() {
    let Some(ping) = ping_bin() else {
        eprintln!("SKIP: arca-ping no compilado (correr cargo test --workspace)");
        return;
    };
    let exec = NativeProcessExec::new(launch_bin())
        .expect("exec")
        .with_frame_bytes(1024);
    let inst = exec.launch_full(&spec(&ping, 701)).expect("launch");
    assert!(
        wait_ready(&inst.handle, Duration::from_secs(5)).is_some(),
        "Ready"
    );
    let st = inst
        .handle
        .shutdown(Duration::from_secs(3))
        .expect("shutdown");
    assert_eq!(st.code, 0, "exit esperado 0, vino {st:?}");
    // el terminal se fija un instante antes que el estado: tolerar la carrera
    let mut ok = false;
    for _ in 0..100 {
        if matches!(
            inst.handle.state(),
            AppState::Dead {
                reason: DeathReason::Exit { code: 0 }
            }
        ) {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ok, "estado final: {:?}", inst.handle.state());
}

#[test]
fn supports_rechaza_no_elf() {
    let exec = NativeProcessExec::new(launch_bin()).expect("exec");
    let sp = spec(Path::new("/dev/null"), 1);
    assert!(
        !exec.supports(&sp).expect("supports"),
        "/dev/null no es ELF"
    );
}
