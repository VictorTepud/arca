//! `arca-exec-native` — backend de procesos nativos (ADR-008, spec 14).
//!
//! Capa L1 · unsafe: **sí** (spawn/exec/wait; cada bloque unsafe lleva
//! invariante). Contiene el binario `arca-launch` (`src/bin/launch.rs`).
//!
//! Flujo de [`NativeProcessExec::launch_full`]:
//! 1. valida artefacto (ELF) + construye el [`LaunchSpec`];
//! 2. crea memfd de frames (double-buffer seqlock) + input (ring SPSC),
//!    inicializa ambas regiones y las mapea (lado host);
//! 3. crea socketpair (ctl) + 2 eventfd (señal) + pipes (spec/stdout/stderr);
//! 4. `posix_spawn(arca-launch)` con dup2 fijo: 3=spec, 4=ctl, 5=tick,
//!    6=ready, 1/2=stdout/stderr del host; env del spawn **vacío** (el env
//!    del hijo es hermético: nace de la LaunchSpec, ver `spec.rs`);
//! 5. watcher (hilo): handshake AIPC (valida identidad, entrega memfds por
//!    SCM_RIGHTS) → eventos Hello/Ready → reap **bloqueante** (hilo propio
//!    de `waitpid`: detección de muerte en µs) + watchdog de detach (host
//!    suelto → SIGKILL y reporte — cumple el contrato de `is_attached`);
//! 6. drain de stdout/stderr → `tracing` con target de la app.
//!
//! # Desviaciones v1 (decisión de arquitecto, worklog T15)
//!
//! - **Socketpair directa en vez de UDS `app.sock`**: elimina la carrera
//!   ECONNREFUSED del spawn; `SO_PEERCRED` sigue verificándose (mismo UID).
//!   El `Server`/`Client` UDS de arca-ipc queda para el router multi-instancia
//!   de host-core (T22) y sus tests.
//! - **`Executor::launch` ignora el parámetro `bus`** en v1: la conexión
//!   host↔app solo existe tras el spawn, así que el executor la crea y la
//!   expone por [`NativeInstance::bus`]. El parámetro se mantiene por el
//!   ABI (wasm in-proc sí lo usa) y se loguea el hecho.
//! - **Reap bloqueante + watchdog de detach** (fix de las e2e flaky):
//!   antes, `waitpid(WNOHANG)` cada 5 ms — bajo carga, el drift del
//!   scheduler hacía que `Dead` llegara tarde (el e2e de kill -9 vencía el
//!   presupuesto de 50 ms) y un host que soltaba el handle dejaba el hilo
//!   watcher y el proceso huérfanos vivos para siempre. Ahora un hilo
//!   dedicado hace `waitpid` BLOQUEANTE (µs de latencia) y el watcher
//!   revisa `is_attached` cada 250 ms: host soltado → SIGKILL + reporte.
//! - **Attach default** (1 ventana 1080×2400, 60 Hz): las ventanas reales
//!   las decide el WM en host-core (T22); v1 headless no las necesita.
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod bus;
pub mod spec;
mod watch;

use std::ffi::CString;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arca_exec_abi::BusTransport as _;
use arca_exec_abi::{AppHandle, AppSpec, BusHandle, DeathReason, Executor};
use arca_ipc::{Conn, SignalChannel};
use arca_permissions::TargetArch;
use arca_protocol::ShmLayout;
use arca_shm::{FrameSlots, Memfd, RingSpsc, ShmMap, MAX_FRAME_BYTES};
use arca_types::{ArcaError, Res};
use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
use nix::unistd::pipe2;
use nix::unistd::Pid;
use tracing::{debug, info};

pub use bus::ConnBus;
pub use spec::{validar_env_extra, LaunchSpec};

use crate::watch::WatchCtx;

/// Bytes de input por slot (docs/04 §6).
const INPUT_SLOT_BYTES: usize = 64;

/// Instancia nativa viva con TODOS los handles del lado host.
///
/// `Executor::launch` devuelve solo el [`AppHandle`]; host-core (T22) y los
/// tests usan [`NativeProcessExec::launch_full`] para acceder al bus, a los
/// canales de señal y a las vistas shm.
pub struct NativeInstance {
    /// Handle ABI del ciclo de vida (eventos, shutdown).
    pub handle: AppHandle,
    /// Bus host↔app (se rellena tras el handshake).
    pub bus: Arc<Mutex<ConnBus>>,
    /// Señal host→app (FrameTick wakeup).
    pub tick: SignalChannel,
    /// Señal app→host (FrameReady wakeup).
    pub ready: SignalChannel,
    /// Vista shm de frames (host = lector).
    pub frames: ShmMap,
    /// Vista shm de input (host = escritor).
    pub input: ShmMap,
    /// Pid del proceso app (logs/tests).
    pub pid: Pid,
    /// Geometría shm acordada en WELCOME.
    pub layout: ShmLayout,
}

impl NativeInstance {
    /// Envía un tick (wakeup del frame loop del rt).
    pub fn send_tick(&self) -> Res<()> {
        self.tick.notify()
    }

    /// Espera FrameReady de la app (wakeup del eventfd app→host).
    pub fn wait_ready(&self, d: std::time::Duration) -> Res<Option<u64>> {
        self.ready.wait(d)
    }

    /// Lee el frame más reciente (copia validada por seqlock).
    pub fn read_latest_frame(&self, out: &mut [u8]) -> Option<arca_shm::FrameSnap> {
        let slots = unsafe { FrameSlots::from_bytes(self.frames.as_slice()).ok()? };
        slots.read_latest_into(out)
    }

    /// Escribe un slot de input (eventos hacia la app).
    pub fn push_input(&self, slot: &[u8]) -> Res<arca_shm::PushResult> {
        let ring = unsafe {
            RingSpsc::from_bytes(self.input.as_slice())
                .map_err(|_| ArcaError::Internal("input: attach"))?
        };
        ring.push(slot)
    }

    /// Envía un mensaje de control por el bus (tras handshake).
    pub fn send_ctl(&self, msg: &arca_protocol::ControlMsg) -> Res<()> {
        self.bus
            .lock()
            .map_err(|_| ArcaError::Internal("bus envenenado"))?
            .send_ctl(msg)
    }
}

/// Executor nativo: procesos hijos reales vía `arca-launch` + seccomp.
pub struct NativeProcessExec {
    launch_bin: PathBuf,
    #[allow(dead_code)] // se usa en supports() al compilar para Android (T22)
    arch: TargetArch,
    frame_bytes: usize,
    input_slots: usize,
}

impl NativeProcessExec {
    /// Crea el executor. `launch_bin`: ruta al binario `arca-launch`
    /// (en Android vive en `files/bin/`; en PC, `target/<profile>/arca-launch`).
    pub fn new(launch_bin: impl Into<PathBuf>) -> Res<Self> {
        let p = launch_bin.into();
        if !p.is_file() {
            return Err(ArcaError::Internal(
                "exec-native: launch_bin no existe (compila el bin arca-launch)",
            ));
        }
        Ok(Self {
            launch_bin: p,
            arch: arca_permissions::current_arch()?,
            frame_bytes: MAX_FRAME_BYTES,
            input_slots: 256,
        })
    }

    /// Override del tamaño de frame por slot (tests con shm pequeñas).
    #[must_use]
    pub fn with_frame_bytes(mut self, bytes: usize) -> Self {
        self.frame_bytes = bytes;
        self
    }

    /// Override de slots del ring de input.
    #[must_use]
    pub fn with_input_slots(mut self, slots: usize) -> Self {
        self.input_slots = slots;
        self
    }

    /// Lanza la instancia y devuelve TODOS los handles del host.
    /// Env del hijo hermético (sin extras): ver [`launch_full_with_env`].
    pub fn launch_full(&self, spec: &AppSpec) -> Res<NativeInstance> {
        self.launch_full_with_env(spec, Vec::new())
    }

    /// [`launch_full`] con pares de env extra para el hijo (p. ej. flags de
    /// fault-injection de tests: `ARCA_PING_PANIC=1`). Viajan por la
    /// [`LaunchSpec`] — NO por el entorno de este proceso — así dos
    /// lanzamientos concurrentes (tests en paralelo, instancias distintas)
    /// nunca se contaminan entre sí. Validación fail-closed en
    /// [`crate::validar_env_extra`]: solo claves `ARCA_*`, sin tocar la
    /// identidad de handshake.
    pub fn launch_full_with_env(
        &self,
        spec: &AppSpec,
        env_extra: Vec<(String, String)>,
    ) -> Res<NativeInstance> {
        // 0) re-validación (defensa en profundidad tras supports()).
        if !elf_ok(&spec.artifact.path) {
            return Err(ArcaError::InvalidPackage(
                "exec-native: artefacto no es ELF o no existe",
            ));
        }

        // 1) memfds + regiones (frames C→H con seqlock; input H→C con ring)
        let frames_len = arca_shm::region_len(self.frame_bytes);
        let frames_fd = Memfd::create("arca-frames", frames_len)?;
        let mut frames = ShmMap::from_fd(frames_fd.as_fd(), frames_len)?;
        FrameSlots::init(frames.as_mut_slice(), self.frame_bytes)?;
        let input_len = 64 + INPUT_SLOT_BYTES * self.input_slots;
        let input_fd = Memfd::create("arca-input", input_len)?;
        let mut input = ShmMap::from_fd(input_fd.as_fd(), input_len)?;
        RingSpsc::init(input.as_mut_slice(), INPUT_SLOT_BYTES, self.input_slots)?;
        let layout = ShmLayout {
            frame_slot_bytes: self.frame_bytes as u32,
            atlas_bytes: 0,
            input_slots: self.input_slots as u32,
            input_slot_bytes: INPUT_SLOT_BYTES as u32,
        };

        // 2) canales: socketpair ctl + 2 eventfd (uno por dirección, el fd
        //    ES compartido: eventfd es read/write del MISMO objeto) + pipes
        let (host_ctl, child_ctl) = socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::SOCK_CLOEXEC,
        )
        .map_err(nix_err("socketpair"))?;
        let tick = SignalChannel::new()?;
        let ready = SignalChannel::new()?;
        let (spec_r, spec_w) = pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(nix_err("pipe"))?;
        let (out_r, out_w) = pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(nix_err("pipe"))?;
        let (err_r, err_w) = pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(nix_err("pipe"))?;

        // 3) LaunchSpec (caps como bitmask de Capability::index())
        let caps_bits = spec
            .caps
            .iter()
            .fold(0u32, |acc, c| acc | (1u32 << c.index()));
        crate::spec::validar_env_extra(&env_extra)?; // (fail-closed pre-fork)
        let lspec = LaunchSpec {
            app_path: spec.artifact.path.to_string_lossy().to_string(),
            app_dir: spec.dirs.app_dir.to_string_lossy().to_string(),
            vault_dir: spec.dirs.vault_dir.to_string_lossy().to_string(),
            app_id: spec.app_id.to_string(),
            instance: spec.instance.get(),
            caps_bits,
            artifact: spec.artifact.hash.0,
            env_extra,
        };
        let blob = lspec.encode();

        // 4) posix_spawn con dup2 fijo {3,4,5,6,1,2} + stdin /dev/null
        let prog = self.launch_bin.to_string_lossy().to_string();
        let arg0 = CString::new("arca-launch").map_err(|_| ArcaError::Internal("argv"))?;
        let argv = [arg0];
        // env del spawn: VACÍO. El env del hijo es hermético — lo construye
        // arca-launch desde la LaunchSpec (identidad + env_extra). Nada del
        // entorno de ESTE proceso pasa al hijo (fix e2e flaky: los tests en
        // paralelo compartían proceso y se colaban ARCA_PING_* entre sí).
        let envp: Vec<CString> = Vec::new();
        use nix::spawn::{PosixSpawnAttr, PosixSpawnFileActions};
        let mut actions = PosixSpawnFileActions::init().map_err(nix_err("fa init"))?;
        actions
            .add_dup2(spec_r.as_raw_fd(), 3)
            .map_err(nix_err("dup2 3"))?;
        actions
            .add_dup2(child_ctl.as_raw_fd(), 4)
            .map_err(nix_err("dup2 4"))?;
        // Invariante: eventfd compartido — el hijo opera el MISMO objeto
        // por el fd 5/6 (notify del host despierta al rt; notify del rt
        // despierta al host).
        actions
            .add_dup2(tick.raw_fd(), 5)
            .map_err(nix_err("dup2 5"))?;
        actions
            .add_dup2(ready.raw_fd(), 6)
            .map_err(nix_err("dup2 6"))?;
        actions
            .add_dup2(out_w.as_raw_fd(), 1)
            .map_err(nix_err("dup2 1"))?;
        actions
            .add_dup2(err_w.as_raw_fd(), 2)
            .map_err(nix_err("dup2 2"))?;
        actions
            .add_open(
                0,
                "/dev/null",
                nix::fcntl::OFlag::O_RDONLY,
                nix::sys::stat::Mode::empty(),
            )
            .map_err(nix_err("open null"))?;
        let attr = PosixSpawnAttr::init().map_err(nix_err("attr"))?;
        let pid = nix::spawn::posix_spawn(prog.as_str(), &actions, &attr, &argv, &envp)
            .map_err(nix_err("posix_spawn"))?;
        info!(target: "arca::exec-native", pid = pid.as_raw(), app = %spec.app_id, "spawn OK");

        // 5) el padre ya no necesita los extremos hijo (tick/ready NO se
        //    sueltan: comparten el eventfd con el hijo).
        drop(spec_r);
        drop(child_ctl);
        drop(out_w);
        drop(err_w);

        // 6) escribe la spec: [len u32][blob] y cierra (el hijo lee exact).
        {
            use std::io::Write as _;
            let mut w = std::io::BufWriter::new(std::fs::File::from(spec_w));
            w.write_all(&(blob.len() as u32).to_le_bytes())
                .and_then(|_| w.write_all(&blob))
                .map_err(|e| ArcaError::Io(std::io::Error::other(format!("spec pipe: {e}"))))?;
        }

        // 7) handle ABI + bus compartido (se llena tras el handshake)
        let bus = ConnBus::empty();
        let bus_handle = BusHandle::new(bus.clone());
        let bus_ret = bus.clone();
        let (handle, driver) = AppHandle::new_pair(spec.app_id.clone(), spec.instance, bus_handle);
        let d = driver.clone();
        driver.install_kill(move || {
            // Invariante: SIGKILL al pid; el watcher reporta la muerte real.
            let r = nix::sys::signal::kill(pid, nix::sys::signal::SIGKILL).map_err(nix_err("kill"));
            if r.is_err() {
                d.report_death(DeathReason::Lost, None);
            }
            r
        });

        // 8) watcher: handshake + reap; drains de stdout/stderr.
        let windows = vec![default_window()];
        let conn = Conn::from_fd(host_ctl)?;
        let app_id = spec.app_id.to_string();
        let app_id_err = app_id.clone();
        std::thread::spawn(move || {
            watch::watch(WatchCtx {
                pid,
                conn,
                spec: lspec,
                // Invariante de orden WELCOME_FDS: [0] frames, [1] input.
                // Los Memfd viven en el watcher: siguen abiertos hasta el
                // sendmsg del handshake (drop = cierre).
                memfds: vec![frames_fd, input_fd],
                layout,
                windows,
                bus: bus.clone(),
                driver,
            })
        });
        std::thread::spawn(move || watch::drain_pipe(out_r, app_id.clone(), "stdout"));
        std::thread::spawn(move || watch::drain_pipe(err_r, app_id_err, "stderr"));

        Ok(NativeInstance {
            handle,
            bus: bus_ret,
            tick,
            ready,
            frames,
            input,
            pid,
            layout,
        })
    }
}

impl Executor for NativeProcessExec {
    fn name(&self) -> &'static str {
        arca_exec_abi::EXECUTOR_NAME_NATIVE
    }

    fn supports(&self, spec: &AppSpec) -> Res<bool> {
        // v1 PC: existe + magic ELF. (En Android: host-libre + ELF aarch64.)
        Ok(spec.artifact.path.is_file() && elf_ok(&spec.artifact.path))
    }

    fn launch(&self, spec: AppSpec, bus: BusHandle) -> Res<AppHandle> {
        // Desviación v1 documentada (módulo docs): el bus del ABI no se usa
        // en native — la conexión nace del spawn (socketpair).
        debug!(target: "arca::exec-native", "v1: bus del ABI ignorado (socketpair propia)");
        let _ = bus;
        self.launch_full(&spec).map(|i| i.handle)
    }
}

/// Ventana default del Attach v1 (T22 pondrá las reales del WM).
fn default_window() -> arca_protocol::WindowSpec {
    arca_protocol::WindowSpec {
        win_id: arca_types::WinId::new(1),
        size: arca_protocol::Size { w: 1080, h: 2400 },
        scale: 1000,
        vsync_hz: 60,
        mode: arca_protocol::WindowMode::Full,
    }
}

fn elf_ok(p: &Path) -> bool {
    use std::io::Read as _;
    let mut f = match std::fs::File::open(p) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).is_ok() && magic == [0x7f, b'E', b'L', b'F']
}

fn nix_err(ctx: &'static str) -> impl Fn(nix::errno::Errno) -> ArcaError {
    move |e: nix::errno::Errno| ArcaError::Io(std::io::Error::other(format!("{ctx}: {e}")))
}
