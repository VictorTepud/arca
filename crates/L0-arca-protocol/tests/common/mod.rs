//! Helpers compartidos de tests: mensajes representativos y generador
//! pseudo-aleatorio determinista (LCG) para el roundtrip de 10k mensajes.

#![allow(dead_code)] // módulo de tests

use arca_protocol::{
    Attach, ControlMsg, Hello, Insets, ShmLayout, SignalMsg, Size, UiCaps, Welcome, WindowMode,
    WindowSpec,
};
use arca_types::{AppId, Capability, Digest, InstanceId, ProtoVersion, WinId};

/// LCG determinista (xorshift64*): sin dependencia de RNG externo.
pub fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed >> 12;
    *seed ^= *seed << 25;
    *seed ^= *seed >> 27;
    seed.wrapping_mul(0x2545F4914F6CDD1D)
}

/// AppId válido generado (letras/dígitos/puntos).
pub fn gen_app_id(seed: &mut u64) -> AppId {
    let n = next(seed) % 8 + 1;
    let mut s = String::from("app");
    for _ in 0..n {
        s.push(match next(seed) % 3 {
            0 => b'a' + (next(seed) % 26) as u8,
            1 => b'0' + (next(seed) % 10) as u8,
            _ => b'.',
        } as char);
    }
    // AppId::new nunca falla con estos caracteres; en tests podemos unwrap.
    AppId::new(&s).unwrap_or_else(|_| AppId::new("app.x").unwrap())
}

/// Digest determinista a partir del seed.
pub fn gen_digest(seed: &mut u64) -> Digest {
    let mut b = [0u8; 32];
    for chunk in b.chunks_mut(8) {
        let v = next(seed).to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&v[..n]);
    }
    Digest(b)
}

/// Mensaje representativo i-ésimo (para golden y humo).
pub fn representative(i: usize) -> (String, ControlMsg) {
    let proto = ProtoVersion::new(1, 0);
    match i {
        0 => ("pause".into(), ControlMsg::Pause),
        1 => (
            "ping".into(),
            ControlMsg::Ping {
                t_ns: 1_234_567_890,
            },
        ),
        2 => (
            "hello".into(),
            ControlMsg::Hello(Hello {
                proto,
                app_id: AppId::new("dev.arca.hello").unwrap(),
                instance: InstanceId::new(1),
                artifact_hash: Digest([0xAB; 32]),
                nonce: [0x42; 16],
            }),
        ),
        3 => (
            "welcome".into(),
            ControlMsg::Welcome(Welcome {
                proto_min: proto,
                layout: ShmLayout {
                    frame_slot_bytes: 4 * 1024 * 1024,
                    atlas_bytes: 1024 * 1024,
                    input_slots: 256,
                    input_slot_bytes: 64,
                },
                caps_granted: vec![Capability::NetClient, Capability::FsVault],
            }),
        ),
        4 => (
            "ready".into(),
            ControlMsg::Ready(arca_protocol::Ready {
                rt_version: 1,
                sdk_version: 1,
                ui_caps: UiCaps {
                    fonts_atlas_damage: true,
                },
            }),
        ),
        5 => (
            "attach".into(),
            ControlMsg::Attach(Attach {
                windows: vec![WindowSpec {
                    win_id: WinId::new(7),
                    size: Size { w: 1080, h: 2400 },
                    scale: 2750,
                    vsync_hz: 60,
                    mode: WindowMode::Full,
                }],
            }),
        ),
        6 => (
            "svc_request".into(),
            ControlMsg::SvcRequest {
                req_id: 99,
                cap: Capability::NetClient,
                payload: vec![1, 2, 3, 4, 5],
            },
        ),
        7 => (
            "svc_response".into(),
            ControlMsg::SvcResponse {
                req_id: 99,
                result: arca_protocol::SvcResult {
                    status: arca_protocol::SvcStatus::Ok,
                    code: 0,
                    payload: vec![9, 8, 7],
                },
            },
        ),
        8 => (
            "config_changed".into(),
            ControlMsg::ConfigChanged {
                win_id: WinId::new(7),
                size: Size { w: 720, h: 1600 },
                scale: 2000,
                insets: Insets {
                    left: 0,
                    top: 84,
                    right: 0,
                    bottom: 48,
                },
                theme: arca_protocol::Theme::Dark,
            },
        ),
        9 => (
            "crash_report".into(),
            ControlMsg::CrashReport {
                signal: 11,
                backtrace_hash: 0xDEAD_BEEF_CAFE,
                minidump_len: 4096,
            },
        ),
        10 => (
            "grant_fd".into(),
            ControlMsg::GrantFd {
                kind: arca_protocol::FdKind::NetSocket,
                req_id: 5,
            },
        ),
        11 => (
            "shutdown".into(),
            ControlMsg::Shutdown {
                reason: arca_protocol::ShutdownReason::Update,
            },
        ),
        _ => panic!("representative: índice fuera de rango"),
    }
}

/// Señales representativas (canal señal).
pub fn representative_signals() -> Vec<(&'static str, SignalMsg)> {
    use arca_protocol::SignalMsg::*;
    vec![
        ("frame_ready", FrameReady { frame_seq: 7 }),
        (
            "frame_tick",
            FrameTick {
                t_ns: 1_725_000_000_000_000_000,
            },
        ),
        ("busy", Busy),
        ("idle", Idle),
        ("pong", Pong { t_ns: 42 }),
    ]
}

/// Genera un mensaje pseudo-aleatorio determinista.
/// Cubre TODAS las variantes de ControlMsg con parámetros variados.
pub fn gen_control_msg(seed: &mut u64) -> ControlMsg {
    use arca_protocol::{FdKind, Ready, ShutdownReason, SvcResult, SvcStatus, Theme};
    let pick = next(seed) % 17;
    match pick {
        0 => ControlMsg::Pause,
        1 => ControlMsg::Resume,
        2 => ControlMsg::Ping { t_ns: next(seed) },
        3 => ControlMsg::Pong { t_ns: next(seed) },
        4 => ControlMsg::Hello(Hello {
            proto: ProtoVersion::new(1, (next(seed) % 3) as u8),
            app_id: gen_app_id(seed),
            instance: InstanceId::new(next(seed) % 1024 + 1),
            artifact_hash: gen_digest(seed),
            nonce: {
                let mut n = [0u8; 16];
                n[..8].copy_from_slice(&next(seed).to_le_bytes());
                n[8..].copy_from_slice(&next(seed).to_le_bytes());
                n
            },
        }),
        5 => ControlMsg::Welcome(Welcome {
            proto_min: ProtoVersion::new(1, 0),
            layout: ShmLayout {
                frame_slot_bytes: next(seed) as u32,
                atlas_bytes: next(seed) as u32,
                input_slots: 256,
                input_slot_bytes: 64,
            },
            caps_granted: Capability::all()
                .iter()
                .filter(|_| next(seed) % 2 == 0)
                .copied()
                .collect(),
        }),
        6 => ControlMsg::Ready(Ready {
            rt_version: next(seed) as u32,
            sdk_version: next(seed) as u32,
            ui_caps: UiCaps {
                fonts_atlas_damage: next(seed) % 2 == 0,
            },
        }),
        7 => ControlMsg::Attach(Attach {
            windows: (0..next(seed) % 4)
                .map(|k| WindowSpec {
                    win_id: WinId::new(k as u32 + 1),
                    size: Size {
                        w: next(seed) as u32,
                        h: next(seed) as u32,
                    },
                    scale: next(seed) as u32 % 5000,
                    vsync_hz: (next(seed) % 3) as u16 * 30 + 60,
                    mode: if next(seed) % 2 == 0 {
                        WindowMode::Full
                    } else {
                        WindowMode::Tile
                    },
                })
                .collect(),
        }),
        8 => ControlMsg::SvcRequest {
            req_id: next(seed),
            cap: Capability::all()[next(seed) as usize % Capability::all().len()],
            payload: (0..next(seed) % 300).map(|_| next(seed) as u8).collect(),
        },
        9 => ControlMsg::SvcResponse {
            req_id: next(seed),
            result: SvcResult {
                status: match next(seed) % 3 {
                    0 => SvcStatus::Ok,
                    1 => SvcStatus::Denied,
                    _ => SvcStatus::Error,
                },
                code: next(seed) as u32,
                payload: (0..next(seed) % 200).map(|_| next(seed) as u8).collect(),
            },
        },
        10 => ControlMsg::ConfigChanged {
            win_id: WinId::new(next(seed) as u32),
            size: Size {
                w: next(seed) as u32,
                h: next(seed) as u32,
            },
            scale: next(seed) as u32 % 5000,
            insets: Insets {
                left: next(seed) as u32,
                top: next(seed) as u32,
                right: next(seed) as u32,
                bottom: next(seed) as u32,
            },
            theme: if next(seed) % 2 == 0 {
                Theme::Light
            } else {
                Theme::Dark
            },
        },
        11 => ControlMsg::WindowOpen {
            win_id: WinId::new(next(seed) as u32),
            size: Size {
                w: next(seed) as u32,
                h: next(seed) as u32,
            },
            scale: next(seed) as u32 % 5000,
        },
        12 => ControlMsg::WindowClose {
            win_id: WinId::new(next(seed) as u32),
        },
        13 => ControlMsg::WindowFocus {
            win_id: WinId::new(next(seed) as u32),
        },
        14 => ControlMsg::CrashReport {
            signal: next(seed) as i32,
            backtrace_hash: next(seed),
            minidump_len: next(seed),
        },
        15 => ControlMsg::GrantFd {
            kind: if next(seed) % 2 == 0 {
                FdKind::NetSocket
            } else {
                FdKind::NetListener
            },
            req_id: next(seed),
        },
        _ => ControlMsg::Shutdown {
            reason: match next(seed) % 5 {
                0 => ShutdownReason::User,
                1 => ShutdownReason::HostGoingAway,
                2 => ShutdownReason::Update,
                3 => ShutdownReason::Unhealthy,
                _ => ShutdownReason::ResourcePressure,
            },
        },
    }
}
