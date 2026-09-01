//! `arca-rt` — runtime que se enlaza dentro de cada sub-app nativa.
//!
//! Capa L2 · unsafe: sí-lite (fds heredados, señal). Contrato: `specs/arca-22`
//! (headless F2: sin UI todavía — el FrameBuilder escribe un frame mínimo
//! reconocible; T17/T21 lo sustituyen por MeshFrame real).
//!
//! Entry point: [`arca_main`]. El binario de la app NO tiene main propio:
//! ```text
//! // main.rs de una sub-app:
//! fn main() { std::process::exit(arca_rt::arca_main(|ctx| { ...; Ok(()) })) }
//! ```
//!
//! Contrato de fds heredados (spec 14 §3, los fija arca-launch):
//! ```text
//! 4 = socket ctl (AIPC, socketpair con el host)
//! 5 = eventfd signal-in  (host → app: FrameTick)
//! 6 = eventfd signal-out (app → host: FrameReady)
//! ```
//!
//! Exit codes (spec 22 §5):
//! - `0`  apagado limpio (respuesta a `Shutdown`)
//! - `101` panic de la app atrapado (CrashReport best-effort enviado)
//! - `102` handshake muerto/tardío (> 2 s)
//! - señal SIGSYS/kill del seccomp: la mata el kernel (KILL_PROCESS)
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use std::os::fd::{AsFd as _, FromRawFd, OwnedFd, RawFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use arca_ipc::{handshake_client, Conn, SignalChannel};
use arca_protocol::{ControlMsg, Hello, ShmLayout, SignalMsg};
use arca_shm::{FrameSlots, RingSpsc, ShmMap};
use arca_types::{AppId, Capability, Digest, InstanceId, Res, WinId, PROTO_VERSION};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use tracing::{debug, info, warn};

/// fd del socket ctl heredado.
const FD_CTL: RawFd = 4;
/// fd del eventfd de ticks (host → app).
const FD_SIG_IN: RawFd = 5;
/// fd del eventfd de ready (app → host).
const FD_SIG_OUT: RawFd = 6;

/// Ticks recibidos en el último drain.
pub type Tick = u64;

/// Contexto de la app dentro del frame loop.
pub struct AppCtx {
    /// Id de la app.
    pub app_id: AppId,
    /// Instancia de esta corrida.
    pub instance: InstanceId,
    /// Ventanas lógicas vivas (del Attach + WindowOpen/Close).
    pub windows: Vec<WinId>,
    /// Capabilities concedidas (del Welcome).
    pub caps: Vec<Capability>,
    /// Escala por-mil actual (ConfigChanged).
    pub scale_pm: u32,
    /// Tamaño lógico de la ventana primaria.
    pub size: (u32, u32),
    /// Slots de input drenados en el tick actual.
    pub input_slots: usize,
    /// Contador de frames publicados.
    pub frame_seq: u64,
    /// Marca "hay que publicar frame en este tick".
    pub dirty: bool,
    /// Contador de ticks procesados.
    pub ticks: u64,
    conn: Option<Conn>,
    frames: Option<FrameSlots>,
    input: Option<RingSpsc>,
    ready_out: Option<SignalChannel>,
    input_total: u64,
}

impl AppCtx {
    /// Publica el frame del tick (seqlock + FrameReady). Frame mínimo v1:
    /// `[frame_seq u64][ticks u64][input_total u64]` — reconocible y barato.
    pub fn publish_frame(&mut self) -> Res<()> {
        let Some(frames) = &self.frames else {
            return Err(arca_types::ArcaError::Internal("rt: sin shm de frames"));
        };
        self.frame_seq += 1;
        let which = (self.frame_seq % 2) as usize;
        let mut w = frames.begin_write(which)?;
        let p = w.payload();
        p.fill(0);
        p[..8].copy_from_slice(&self.frame_seq.to_le_bytes());
        p[8..16].copy_from_slice(&self.ticks.to_le_bytes());
        p[16..24].copy_from_slice(&self.input_total.to_le_bytes());
        w.publish()?;
        if let Some(sig) = &self.ready_out {
            sig.notify()?; // FrameReady → host
        }
        Ok(())
    }

    /// Envía un mensaje de control al host (svc-broker etc.).
    pub fn send_ctl(&mut self, msg: &ControlMsg) -> Res<()> {
        match &mut self.conn {
            Some(c) => c.send_ctl(msg, next_seq(), &[]),
            None => Err(arca_types::ArcaError::Internal("rt: sin conexión")),
        }
    }

    /// Envía una señal por el canal socket (Busy/Idle/Pong de señal).
    pub fn send_signal(&mut self, s: &SignalMsg) -> Res<()> {
        match &mut self.conn {
            Some(c) => c.send_signal(s, next_seq()),
            None => Err(arca_types::ArcaError::Internal("rt: sin conexión")),
        }
    }

    /// Pide el apagado limpio de la app (el host manda Shutdown).
    pub fn exit(&mut self, _code: i32) -> ! {
        std::process::exit(0)
    }
}

fn next_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Identidad desde el env mínimo de arca-launch.
fn hello_from_env() -> Res<Hello> {
    let app_id = AppId::new(
        &std::env::var("ARCA_APP_ID")
            .map_err(|_| arca_types::ArcaError::Internal("rt: falta ARCA_APP_ID"))?,
    )?;
    let instance: u64 = std::env::var("ARCA_INSTANCE")
        .map_err(|_| arca_types::ArcaError::Internal("rt: falta ARCA_INSTANCE"))?
        .parse()
        .map_err(|_| arca_types::ArcaError::Internal("rt: ARCA_INSTANCE no numérico"))?;
    let artifact = Digest::from_hex(
        &std::env::var("ARCA_ARTIFACT")
            .map_err(|_| arca_types::ArcaError::Internal("rt: falta ARCA_ARTIFACT"))?,
    )?;
    let mut nonce = [0u8; 16];
    getrandom_nonce(&mut nonce)?;
    Ok(Hello {
        proto: PROTO_VERSION,
        app_id,
        instance: InstanceId::new(instance),
        artifact_hash: artifact,
        nonce,
    })
}

/// Nonce del handshake: /dev/urandom NO está permitido por seccomp (openat):
/// mezcla de clocks monotónicos + pid (suficiente anti-replay por proceso).
fn getrandom_nonce(out: &mut [u8; 16]) -> Res<()> {
    let a = arca_types::now_mono_ns();
    let b = a ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let c = a.rotate_left(17) ^ b.rotate_left(31);
    out[..8].copy_from_slice(&a.to_le_bytes());
    out[8..].copy_from_slice(&c.to_le_bytes());
    Ok(())
}

/// Entry point de TODA sub-app nativa (spec 22 §3).
///
/// Ciclo (docs/04 §8): el runtime gobierna el bucle — la app solo reacciona
/// a ticks. Pánico de la app → CrashReport + exit(101) (nunca SIGABRT crudo).
pub fn arca_main<F>(f: F) -> i32
where
    F: FnMut(&mut AppCtx) -> Res<()>,
{
    // 1) crash handler: hook de pánico con backtrace a stderr (el host drena)
    install_panic_hook();
    let _ = arca_log::init_subapp(InstanceId::new(
        std::env::var("ARCA_INSTANCE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    ));

    // 2) identidad + fds heredados
    let hello = match hello_from_env() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("arca-rt: identidad inválida: {e}");
            return 102;
        }
    };
    // Invariante: ownership EXCLUSIVA de los fds 4/5/6 (nadie más los usa).
    let conn = unsafe { OwnedFd::from_raw_fd(FD_CTL) };
    let sig_in = unsafe { OwnedFd::from_raw_fd(FD_SIG_IN) };
    let sig_out = unsafe { OwnedFd::from_raw_fd(FD_SIG_OUT) };
    let conn = match Conn::from_fd(conn) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("arca-rt: ctl fd inválido: {e}");
            return 102;
        }
    };
    let sig_in = SignalChannel::from_owned(sig_in);
    let sig_out = SignalChannel::from_owned(sig_out);

    // 3) handshake (deadline 2 s dentro de arca-ipc) → exit 102 si falla
    let ses = match handshake_client(conn, &hello) {
        Ok(s) => s,
        Err(e) => {
            warn!(target: "arca::rt", err = %e, "handshake falló (host muerto?)");
            eprintln!("arca-rt: handshake falló: {e}");
            return 102;
        }
    };
    let lay: ShmLayout = ses.welcome.layout;
    let frames_map = match ShmMap::from_fd(
        ses.memfds[0].as_fd(),
        arca_shm::region_len(lay.frame_slot_bytes as usize),
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("arca-rt: map frames: {e}");
            return 102;
        }
    };
    let input_len = 64 + lay.input_slot_bytes as usize * lay.input_slots as usize;
    let input_map = match ShmMap::from_fd(ses.memfds[1].as_fd(), input_len) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("arca-rt: map input: {e}");
            return 102;
        }
    };
    // Invariante: las vistas unsafe viven mientras los mapeos.
    let frames = unsafe { FrameSlots::from_bytes(frames_map.as_slice()) };
    let input = unsafe { RingSpsc::from_bytes(input_map.as_slice()) };
    if frames.is_err() || input.is_err() {
        eprintln!("arca-rt: shm con geometría corrupta");
        return 102;
    }
    let (frames, input) = match (frames, input) {
        (Ok(f), Ok(i)) => (f, i),
        _ => {
            eprintln!("arca-rt: shm con geometría corrupta");
            return 102;
        }
    };

    let primaria = ses.attach.windows.first().map(|w| w.win_id);
    let mut ctx = AppCtx {
        app_id: hello.app_id.clone(),
        instance: hello.instance,
        windows: ses.attach.windows.iter().map(|w| w.win_id).collect(),
        caps: ses.welcome.caps_granted.clone(),
        scale_pm: primaria
            .and_then(|w| ses.attach.windows.iter().find(|x| x.win_id == w))
            .map_or(1000, |x| x.scale),
        size: primaria
            .and_then(|w| ses.attach.windows.iter().find(|x| x.win_id == w))
            .map_or((1080, 2400), |x| (x.size.w, x.size.h)),
        input_slots: 0,
        frame_seq: 0,
        dirty: true,
        ticks: 0,
        conn: Some(ses.conn),
        frames: Some(frames),
        input: Some(input),
        ready_out: Some(sig_out),
        input_total: 0,
    };

    // 4) frame loop controlado por el host (spec 22 §4)
    run_loop(&mut ctx, sig_in, f)
}

/// Bucle principal: poll de {señal, ctl} + catch_unwind del callback.
fn run_loop<F>(ctx: &mut AppCtx, sig_in: SignalChannel, mut f: F) -> i32
where
    F: FnMut(&mut AppCtx) -> Res<()>,
{
    let conn_fd = ctx.conn.as_ref().map_or(-1, |c| c.raw_fd());
    #[allow(unused_mut)] // T22: la política de pausa la gobierna host-core (con reanudación)
    let mut paused = false;
    let mut buf = Vec::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    loop {
        // poll: señal de tick + legibilidad del ctl (sin bloqueo infinito)
        let mut pfd = [
            PollFd::new(sig_in.as_fd(), PollFlags::POLLIN),
            PollFd::new(
                unsafe { std::os::fd::BorrowedFd::borrow_raw(conn_fd) },
                PollFlags::POLLIN,
            ),
        ];
        let timeout =
            PollTimeout::try_from(Duration::from_millis(100)).unwrap_or(PollTimeout::NONE);
        let r = poll(&mut pfd, timeout);
        if r.is_err() {
            continue; // EINTR y similares
        }
        let tick = pfd[0]
            .revents()
            .is_some_and(|e| e.contains(PollFlags::POLLIN));
        let ctl = pfd[1]
            .revents()
            .is_some_and(|e| e.contains(PollFlags::POLLIN));

        if tick {
            let _ = sig_in.try_wait(); // drena el contador del eventfd
            if paused {
                continue; // congelada: no produce frames (docs/04 §9)
            }
            // drain de input del ring (Vsync marca límites de tick)
            if let Some(ring) = &ctx.input {
                let n = ring.pop_each(256, |_| true).unwrap_or(0);
                ctx.input_slots = n;
                ctx.input_total += n as u64;
            }
            ctx.ticks += 1;
            ctx.dirty = true;
            // callback de la app BAJO catch_unwind (spec 22 §4)
            {
                let r = catch_unwind(AssertUnwindSafe(|| f(ctx)));
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        // error tipado de la app: crash report + 101
                        warn!(target: "arca::rt", err = %e, "la app devolvió error");
                        crash_report(ctx, 0, 0);
                        return 101;
                    }
                    Err(panic) => {
                        let msg = panic_msg(&panic);
                        eprintln!("arca-rt: PANIC de la app: {msg}");
                        let hash = panic_hash(&msg);
                        crash_report(ctx, 0, hash);
                        return 101;
                    }
                }
            }
            if ctx.dirty {
                if let Err(e) = ctx.publish_frame() {
                    warn!(target: "arca::rt", err = %e, "publish_frame falló");
                }
            }
        }

        if ctl {
            let Some(conn) = &mut ctx.conn else {
                return 102;
            };
            if conn.deadline_ms() == 0 {
                let _ = conn.set_deadline(500);
            }
            match conn.recv_ctl(&mut buf, &mut fds) {
                Ok((_, archived)) => {
                    let msg = rkyv::deserialize::<ControlMsg, rkyv::rancor::Error>(archived);
                    match msg {
                        Ok(m) => {
                            if let Some(code) = handle_ctl(ctx, m) {
                                info!(target: "arca::rt", code, "apagado limpio");
                                return code;
                            }
                        }
                        Err(e) => warn!(target: "arca::rt", err = %e, "ctl ilegible"),
                    }
                }
                Err(arca_types::ArcaError::Internal(m)) if m.contains("deadline") => {
                    let _ = conn.set_deadline(500);
                }
                Err(e) => {
                    // El host cerró o murió: sin él no hay ciclo de vida.
                    warn!(target: "arca::rt", err = %e, "ctl caído (host muerto)");
                    return 102;
                }
            }
        }
    }
}

/// Despacha mensajes de control. `Some(code)` = terminar el proceso.
fn handle_ctl(ctx: &mut AppCtx, m: ControlMsg) -> Option<i32> {
    match m {
        ControlMsg::Ping { t_ns } => {
            let _ = ctx.send_ctl(&ControlMsg::Pong { t_ns });
            None
        }
        ControlMsg::Shutdown { reason } => {
            debug!(target: "arca::rt", ?reason, "Shutdown recibido");
            Some(0)
        }
        ControlMsg::Pause => {
            let _ = ctx.send_signal(&SignalMsg::Busy);
            None
        }
        ControlMsg::Resume => None,
        ControlMsg::ConfigChanged {
            win_id,
            size,
            scale,
            ..
        } => {
            if ctx.windows.first() == Some(&win_id) {
                ctx.size = (size.w, size.h);
                ctx.scale_pm = scale;
            }
            None
        }
        ControlMsg::WindowOpen { win_id, .. } => {
            ctx.windows.push(win_id);
            None
        }
        ControlMsg::WindowClose { win_id } => {
            ctx.windows.retain(|w| *w != win_id);
            None
        }
        ControlMsg::WindowFocus { .. } => None,
        // El resto (svc, grant, health) llega tipado al sdk en F3.
        other => {
            debug!(target: "arca::rt", msg = ?other, "ctl ignorado (v1)");
            None
        }
    }
}

/// CrashReport best-effort por el ctl vivo (spec 22 §4) + exit code 101.
fn crash_report(ctx: &mut AppCtx, signal: i32, backtrace_hash: u64) {
    let _ = ctx.send_ctl(&ControlMsg::CrashReport {
        signal,
        backtrace_hash,
        minidump_len: 0,
    });
}

/// Hash estable del mensaje de pánico (agregación del panel, T21 lo mejora).
fn panic_hash(msg: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in msg.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "<no-string>".into()
    }
}

fn install_panic_hook() {
    // NOTA de sandbox: sin openat NO hay backtrace simbólico (leer
    // /proc/self/maps es lo primero que hace Backtrace). v1: mensaje +
    // location (viaja por el drain del host); el hash del location alimenta
    // el CrashReport. El minidump real es T21 (crash handler con maps
    // pre-abiertas antes de exec).
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        eprintln!("arca-rt panic: {loc} :: {info}");
    }));
}
