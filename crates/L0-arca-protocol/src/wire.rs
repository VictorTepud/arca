//! Tipos de mensaje AIPC v1 (canal control + señal) — docs/04 §4.
//!
//! Desviación documentada de spec 03 §3 (decisión de arquitecto, worklog T10):
//! - `caps_granted: Vec<Capability>` en vez de `SmallVec<Capability, 8>`:
//!   rkyv 0.8 no tiene soporte first-class de smallvec sin feature extra y la
//!   asignación ocurre UNA vez por handshake (no en path caliente).
//! - `GrantFd` lleva `req_id` (además de `kind`) para correlacionar el fd que
//!   llega por SCM_RIGHTS con el `SvcRequest` que lo pidió. El campo `fd` de
//!   docs/04 §4 no viaja en el payload: va en el ancillary del mismo sendmsg.
//!
//! NOTA de lints: `#![allow(missing_docs)]` es EXCLUSIVAMENTE por los tipos
//! `Archived*`/`*Resolver` que genera la macro de rkyv (sin docs propias);
//! todo tipo propio de este módulo sí está documentado campo a campo.

#![allow(missing_docs)]

use arca_types::{AppId, Capability, Digest, InstanceId, ProtoVersion, WinId};

/// Tamaño en píxeles lógicos (post-escala de densidad).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Size {
    /// Ancho en px lógicos.
    pub w: u32,
    /// Alto en px lógicos.
    pub h: u32,
}

/// Insets del sistema en px físicos (status bar, nav bar, teclado).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Insets {
    /// Izquierda.
    pub left: u32,
    /// Arriba.
    pub top: u32,
    /// Derecha.
    pub right: u32,
    /// Abajo.
    pub bottom: u32,
}

/// Tema visual del sistema (viaja en `ConfigChanged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum Theme {
    /// Claro.
    Light,
    /// Oscuro.
    Dark,
}

/// Escala de densidad en **por-mil** (1000 = 1.0x, 275 = 0.275x). Entero en
/// el wire = bytes deterministas y sin NaN/precision drift.
pub type ScalePm = u32;

/// Por qué el host pide el apagado de la instancia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum ShutdownReason {
    /// El usuario cerró la app/ventana.
    User,
    /// El host completo se apaga (todas las instancias reciben Shutdown).
    HostGoingAway,
    /// Update in-place del paquete (docs/10 §7).
    Update,
    /// Watchdog: 3 ping sin respuesta o frame stall sin `Busy`.
    Unhealthy,
    /// Presión de memoria: el host recoge la instancia menos usada.
    ResourcePressure,
}

/// Resultado de un pedido al svc-broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum SvcStatus {
    /// El servicio se ejecutó; payload = respuesta.
    Ok,
    /// Denegado por política (sin la capability).
    Denied,
    /// El servicio corrió pero falló; `code` = errno/estado interno.
    Error,
}

/// Resultado completo de un `SvcRequest` (bounded por MAX_CTL_PAYLOAD).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct SvcResult {
    /// Estado de la operación.
    pub status: SvcStatus,
    /// Código de error (0 si Ok/Denied; errno o código de servicio si Error).
    pub code: u32,
    /// Payload de respuesta (vacío si no aplica).
    pub payload: Vec<u8>,
}

/// Clase de fd concedido por el broker (el fd va por SCM_RIGHTS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum FdKind {
    /// Socket TCP/UDP YA CONECTADO (patrón "socket pasado", docs/07 §4).
    NetSocket,
    /// Socket de escucha concedido por `net-server` (muy restringido).
    NetListener,
}

/// Modo de presentación de una ventana (docs/06 wm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum WindowMode {
    /// Ventana "real" (Activity dedicada; split-screen/freeform del SO).
    Full,
    /// Tile del grid multitarea del host (móvil).
    Tile,
}

/// Una ventana lógica creada en el ATTACH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct WindowSpec {
    /// Id de ventana (asigna el WM del host).
    pub win_id: WinId,
    /// Tamaño inicial en px lógicos.
    pub size: Size,
    /// Escala por-mil.
    pub scale: ScalePm,
    /// Frecuencia de vsync del display que la muestra.
    pub vsync_hz: u16,
    /// Modo de presentación.
    pub mode: WindowMode,
}

/// Geometría de la shm que el host entrega en `Welcome` (los memfd van por
/// SCM_RIGHTS; esto describe cómo interpretarlos).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct ShmLayout {
    /// Bytes por slot de frame (2 slots; spec 05: MAX_FRAME_BYTES).
    pub frame_slot_bytes: u32,
    /// Bytes de la región de staging del atlas.
    pub atlas_bytes: u32,
    /// Número de slots del ring de input.
    pub input_slots: u32,
    /// Bytes por slot de input (spec 05: 64).
    pub input_slot_bytes: u32,
}

/// Capacidades UI que el runtime anuncia en `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct UiCaps {
    /// ¿Sabe reportar damage incremental del atlas de fuentes?
    pub fonts_atlas_damage: bool,
}

/// Primer mensaje del handshake (C→H, docs/04 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Hello {
    /// Versión AIPC máxima que habla el cliente.
    pub proto: ProtoVersion,
    /// Identidad declarada de la app (el host la contrasta con la esperada).
    pub app_id: AppId,
    /// Id de instancia asignado por el host (va en el launch spec).
    pub instance: InstanceId,
    /// Digest blake3 del artefacto ejecutado (anti-sustitución).
    pub artifact_hash: Digest,
    /// Nonce aleatorio de 16 B (anti-replay del handshake).
    pub nonce: [u8; 16],
}

/// Respuesta del host (H→C) con la geometría shm y capabilities concedidas.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Welcome {
    /// Versión mínima que el host acepta (rechazo limpio si incompatible).
    pub proto_min: ProtoVersion,
    /// Geometría de los memfd que llegan por SCM_RIGHTS.
    pub layout: ShmLayout,
    /// Capabilities efectivamente concedidas (subset de las pedidas).
    pub caps_granted: Vec<Capability>,
}

/// El runtime está listo (C→H) tras mapear la shm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Ready {
    /// Versión del runtime arca-rt.
    pub rt_version: u32,
    /// Versión del SDK con el que se compiló la app.
    pub sdk_version: u32,
    /// Capacidades UI del runtime.
    pub ui_caps: UiCaps,
}

/// Ventanas iniciales (H→C); tras esto la instancia está "viva".
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub struct Attach {
    /// Ventanas lógicas iniciales (≥1 en v1).
    pub windows: Vec<WindowSpec>,
}

/// Mensajes del canal de control (docs/04 §4). Fuente de verdad de variantes:
/// esta tabla. Campo nuevo = minor bump + golden test NUEVO.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum ControlMsg {
    // ── lifecycle ─────────────────────────────────────────────────────────
    /// Congelar el loop de frames (background/oclusión total).
    Pause,
    /// Reanudar el loop de frames.
    Resume,
    /// Métricas/insets/tema cambiaron (rotación, dark mode, teclado).
    ConfigChanged {
        /// Ventana afectada.
        win_id: WinId,
        /// Nuevo tamaño lógico.
        size: Size,
        /// Nueva escala por-mil.
        scale: ScalePm,
        /// Insets actuales.
        insets: Insets,
        /// Tema actual.
        theme: Theme,
    },
    /// Ventana nueva (multi-ventana lógica).
    WindowOpen {
        /// Id asignado.
        win_id: WinId,
        /// Tamaño inicial.
        size: Size,
        /// Escala por-mil.
        scale: ScalePm,
    },
    /// Cerrar ventana.
    WindowClose {
        /// Id a cerrar.
        win_id: WinId,
    },
    /// Focus movido a esta ventana.
    WindowFocus {
        /// Id con foco.
        win_id: WinId,
    },
    /// Apagado solicitado por el host.
    Shutdown {
        /// Motivo (logs/decisión de respawn).
        reason: ShutdownReason,
    },

    // ── handshake (docs/04 §3) ────────────────────────────────────────────
    /// C→H: identidad del cliente.
    Hello(Hello),
    /// H→C: shm + capabilities (fds por SCM_RIGHTS aparte).
    Welcome(Welcome),
    /// C→H: runtime listo.
    Ready(Ready),
    /// H→C: ventanas iniciales; empiezan los ticks.
    Attach(Attach),

    // ── servicios (broker) ────────────────────────────────────────────────
    /// C→H: pedido de servicio.
    SvcRequest {
        /// Correlación de ida.
        req_id: u64,
        /// Capability que autoriza el servicio pedido.
        cap: Capability,
        /// Payload del servicio (bounded MAX_CTL_PAYLOAD).
        payload: Vec<u8>,
    },
    /// H→C: respuesta del broker.
    SvcResponse {
        /// Correlación de vuelta.
        req_id: u64,
        /// Resultado.
        result: SvcResult,
    },

    // ── salud (docs/04 §9) ────────────────────────────────────────────────
    /// Latido del host.
    Ping {
        /// Marca de tiempo monotónica del emisor (ns).
        t_ns: u64,
    },
    /// Respuesta al latido.
    Pong {
        /// Eco del t_ns del Ping.
        t_ns: u64,
    },
    /// Crash report best-effort del runtime antes de morir.
    CrashReport {
        /// Señal que mató al proceso (0 si panic de Rust).
        signal: i32,
        /// Hash estable del backtrace (agregación en panel).
        backtrace_hash: u64,
        /// Longitud del minidump escrito (0 si no alcanzó a escribirse).
        minidump_len: u64,
    },

    // ── recursos ──────────────────────────────────────────────────────────
    /// H→C: fd concedido (viaja por SCM_RIGHTS en el mismo sendmsg).
    GrantFd {
        /// Clase de fd.
        kind: FdKind,
        /// SvcRequest al que responde.
        req_id: u64,
    },
}

/// Mensajes del canal de señal (QoS mínimo; docs/04 §4).
///
/// Dos codificaciones coexisten (ADR-005 "un protocolo, dos transportes"):
/// - Por **socket** con el mismo framing AIPC ([`crate::encode_signal_into`]).
/// - Por **eventfd** como un único u64 taggeado ([`crate::framing::encode_signal_wire`]),
///   que es el path caliente real del host (0 syscalls de framing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
#[allow(missing_docs)] // Archived<T> autogenerado por rkyv: sin docs
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)]
#[rkyv(derive(Debug, PartialEq), compare(PartialEq))]
pub enum SignalMsg {
    /// La app publicó un frame nuevo en shm (C→H).
    FrameReady {
        /// Seq del frame publicado (paridad del seqlock implícita).
        frame_seq: u64,
    },
    /// Tic de render del host (H→C): drenar input y producir frame.
    FrameTick {
        /// Timestamp monotónico del vsync (ns).
        t_ns: u64,
    },
    /// La app está trabajando (no mandes watchdog-kill).
    Busy,
    /// La app quedó inactiva (puede dormir entre ticks).
    Idle,
    /// Respuesta de señal a Ping (eco de t_ns).
    Pong {
        /// Echo del t_ns.
        t_ns: u64,
    },
}

/// Discriminante de [`SignalMsg`] para el wire eventfd (u64 taggeado).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalKind {
    /// Frame listo.
    FrameReady,
    /// Tic de vsync.
    FrameTick,
    /// Ocupado.
    Busy,
    /// Inactivo.
    Idle,
    /// Pong.
    Pong,
}

impl SignalKind {
    /// Byte de discriminante (estable en wire; NO renumerar).
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::FrameReady => 1,
            Self::FrameTick => 2,
            Self::Busy => 3,
            Self::Idle => 4,
            Self::Pong => 5,
        }
    }

    /// Parse del byte de discriminante.
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            1 => Self::FrameReady,
            2 => Self::FrameTick,
            3 => Self::Busy,
            4 => Self::Idle,
            5 => Self::Pong,
            _ => return None,
        })
    }
}
