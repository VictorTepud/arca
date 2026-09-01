//! [`explain`] — decisiones legibles para el panel de permisos.
//!
//! Cada [`Decision`] traduce una capability concedida a su efecto CONCRETO
//! sobre el sandbox: qué syscalls/concesiones implica (y qué NO implica).
//! Es la cara amable del "dueño del modelo de permisos": el mismo crate que
//! compila el BPF explica por qué el panel muestra lo que muestra.

use arca_types::Capability;

use crate::capset::CapabilitySet;

/// Efecto de una capability concedida, en lenguaje de sandbox (para el
/// panel de diagnóstico de permisos del host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Capability a la que aplica la decisión.
    pub cap: Capability,
    /// Efecto concreto: syscalls/concesiones que añade (o explicita que no
    /// añade ninguna). Texto estable, apto para tests de UI.
    pub effect: String,
}

/// Explica las capabilities CONCEDIDAS de `caps`, en orden de declaración
/// de `Capability` (determinista).
///
/// ```
/// use arca_permissions::{explain, CapabilitySet};
/// use arca_types::Capability;
///
/// let caps = CapabilitySet::from_iter([Capability::NetClient]);
/// let decisiones = explain(&caps);
/// assert_eq!(decisiones.len(), 1);
/// assert_eq!(decisiones[0].cap, Capability::NetClient);
/// ```
#[must_use]
pub fn explain(caps: &CapabilitySet) -> Vec<Decision> {
    caps.iter()
        .map(|c| Decision {
            cap: c,
            effect: effect_of(c).to_owned(),
        })
        .collect()
}

/// Efecto estático por capability (v1, spec 07 §3: mapeo capability→efecto).
const fn effect_of(c: Capability) -> &'static str {
    match c {
        Capability::NetClient => {
            "net-client → el broker entrega sockets YA conectados (connect() nunca existe en el \
             sandbox: la app solo hace send/recv sobre fds heredados)"
        }
        Capability::NetServer => {
            "net-server → el broker entrega fds de escucha/aceptados (bind/listen/accept viven en \
             el host); ninguna syscall de red nueva"
        }
        Capability::ClipboardRead => {
            "clipboard-read → lecturas del portapapeles vía mensaje al svc-broker (sin syscalls \
             nuevas)"
        }
        Capability::ClipboardWrite => {
            "clipboard-write → escrituras del portapapeles vía mensaje al svc-broker (sin \
             syscalls nuevas)"
        }
        Capability::Notify => {
            "notify → notificaciones locales vía svc-broker (sin syscalls nuevas)"
        }
        Capability::Share => {
            "share → compartir/intents Android vía svc-broker (sin syscalls nuevas)"
        }
        Capability::OpenUri => {
            "open-uri → apertura de URIs externas vía svc-broker (sin syscalls nuevas)"
        }
        Capability::Vibrate => "vibrate → vibración corta vía svc-broker (sin syscalls nuevas)",
        Capability::FsVault => {
            "fs-vault → añade la bóveda a allowed_paths; E/S de archivos mediada por el broker \
             (openat sigue bloqueado por seccomp)"
        }
        Capability::SystemStoreRead => {
            "system-store-read → consultas al registro vía svc-broker (sin syscalls nuevas)"
        }
        Capability::BackgroundAudio => {
            "background-audio → v1: sin cambios en el sandbox (futuro: shm de audio)"
        }
        // `Capability` es #[non_exhaustive]: el host viejo que ve una
        // capability nueva la trata SIN efecto (fail-closed: el sandbox
        // base ya es default-deny).
        _ => "capability desconocida para este host → sin cambios en el sandbox (fail-closed)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_vacio_no_hay_decisiones() {
        assert!(explain(&CapabilitySet::empty()).is_empty());
    }

    #[test]
    fn explain_orden_por_declaracion() {
        let caps = CapabilitySet::from_iter([
            Capability::BackgroundAudio,
            Capability::FsVault,
            Capability::NetClient,
        ]);
        let d: Vec<Capability> = explain(&caps).into_iter().map(|x| x.cap).collect();
        assert_eq!(
            d,
            vec![
                Capability::NetClient,
                Capability::FsVault,
                Capability::BackgroundAudio,
            ]
        );
    }

    #[test]
    fn explain_net_client_explica_el_patron_socket_pasado() {
        let caps = CapabilitySet::from_iter([Capability::NetClient]);
        let d = &explain(&caps)[0];
        assert!(d.effect.contains("sockets YA conectados"));
        // La promesa clave: la capability NO abre sockets crudos.
        assert!(d.effect.contains("connect() nunca existe"));
    }

    #[test]
    fn explain_fs_vault_explica_openat_bloqueado() {
        let caps = CapabilitySet::from_iter([Capability::FsVault]);
        let d = &explain(&caps)[0];
        assert!(d.effect.contains("allowed_paths"));
        assert!(d.effect.contains("openat"));
    }

    #[test]
    fn explain_background_audio_es_stub_honesto() {
        let caps = CapabilitySet::from_iter([Capability::BackgroundAudio]);
        let d = &explain(&caps)[0];
        assert!(d.effect.contains("sin cambios"));
    }

    #[test]
    fn explain_todas_tienen_texto_no_vacio() {
        let todas = CapabilitySet::from_iter(Capability::all().iter().copied());
        let decisiones = explain(&todas);
        assert_eq!(decisiones.len(), Capability::count());
        for d in &decisiones {
            assert!(!d.effect.is_empty(), "{} sin efecto", d.cap);
        }
    }
}
