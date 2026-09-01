//! `arca-ping` — sub-app de prueba del ciclo completo (spec 22 §6: "app de
//! test ping"). Compone: handshake → frame loop → echo Ping/Pong → Shutdown.
//!
//! Variables de entorno (las fija el test/host — passthrough ARCA_*):
//! - `ARCA_PING_PANIC=1`: pánico inyectado en el primer tick (→ exit 101).
//! - `ARCA_PING_SOCKET=1`: intenta `socket(AF_INET)` (muere por SIGSYS del
//!   seccomp — prueba que el filtro está ACTIVO).
//! - `ARCA_PING_TICKS=N`: apagado limpio tras N ticks (exit 0).

use arca_rt::AppCtx;
use arca_types::Res;

fn main() {
    std::process::exit(arca_rt::arca_main(ping_loop));
}

fn ping_loop(ctx: &mut AppCtx) -> Res<()> {
    let limite: Option<u64> = std::env::var("ARCA_PING_TICKS")
        .ok()
        .and_then(|s| s.parse().ok());

    if std::env::var("ARCA_PING_SOCKET").as_deref() == Ok("1") {
        // Debe morir por SIGSYS (KILL_PROCESS del filtro) — nunca llegar aquí.
        let fd = nix::sys::socket::socket(
            nix::sys::socket::AddressFamily::Inet,
            nix::sys::socket::SockType::Stream,
            nix::sys::socket::SockFlag::empty(),
            None,
        );
        eprintln!("arca-ping: socket() devolvió {fd:?} (¡seccomp NO activo!)");
        if let Ok(f) = fd {
            drop(f);
        }
    }

    if std::env::var("ARCA_PING_PANIC").as_deref() == Ok("1") && ctx.ticks == 1 {
        panic!("ping: pánico inyectado (ARCA_PING_PANIC=1)");
    }

    ctx.dirty = true; // publica frame cada tick
    if let Some(n) = limite {
        if ctx.ticks >= n {
            ctx.exit(0);
        }
    }
    Ok(())
}

/// Referencia para que AppCtx no quede como import no usado en doc-builds.
#[allow(dead_code)]
fn _doc(_: &AppCtx) {}
