//! Capabilities del sistema de permisos.
//!
//! Se definen AQUÍ (no en `arca-permissions`) para evitar el ciclo
//! permissions↔protocol (spec 01 §3): ambos consumen este enum, nadie lo posee.

/// Capability concreta que una sub-app puede solicitar en su manifest.
///
/// El enum es `#[non_exhaustive]`: nuevas capabilities se añaden sin romper
/// la compatibilidad wire (spec 07 define el mapeo capability→seccomp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub enum Capability {
    /// Conectarse a Internet como cliente (TCP/UDP out vía svc-broker).
    NetClient,
    /// Escuchar conexiones entrantes (muy restringido).
    NetServer,
    /// Leer el portapapeles del sistema.
    ClipboardRead,
    /// Escribir en el portapapeles del sistema.
    ClipboardWrite,
    /// Mostrar notificaciones locales.
    Notify,
    /// Compartir contenido con otras apps Android.
    Share,
    /// Abrir URIs externas (http/mailto/intents).
    OpenUri,
    /// Vibración corta (feedback háptico).
    Vibrate,
    /// Bóveda de archivos privada por app (FsVault en svc-broker).
    FsVault,
    /// Leer datos de la store de sistema (métricas, permisos).
    SystemStoreRead,
    /// Reproducir audio en segundo plano.
    BackgroundAudio,
}

impl Capability {
    /// Nombre canónico en wire/manifest (kebab-lowercase, estable).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NetClient => "net-client",
            Self::NetServer => "net-server",
            Self::ClipboardRead => "clipboard-read",
            Self::ClipboardWrite => "clipboard-write",
            Self::Notify => "notify",
            Self::Share => "share",
            Self::OpenUri => "open-uri",
            Self::Vibrate => "vibrate",
            Self::FsVault => "fs-vault",
            Self::SystemStoreRead => "system-store-read",
            Self::BackgroundAudio => "background-audio",
        }
    }

    /// Máximo de capabilities conocidas (para reservar buffers/arrays).
    #[must_use]
    pub const fn count() -> usize {
        11
    }

    /// Todas las capabilities conocidas, en orden de declaración.
    #[must_use]
    pub fn all() -> &'static [Capability] {
        &[
            Self::NetClient,
            Self::NetServer,
            Self::ClipboardRead,
            Self::ClipboardWrite,
            Self::Notify,
            Self::Share,
            Self::OpenUri,
            Self::Vibrate,
            Self::FsVault,
            Self::SystemStoreRead,
            Self::BackgroundAudio,
        ]
    }

    /// Parse del nombre canónico (o `None` si es desconocida).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "net-client" => Self::NetClient,
            "net-server" => Self::NetServer,
            "clipboard-read" => Self::ClipboardRead,
            "clipboard-write" => Self::ClipboardWrite,
            "notify" => Self::Notify,
            "share" => Self::Share,
            "open-uri" => Self::OpenUri,
            "vibrate" => Self::Vibrate,
            "fs-vault" => Self::FsVault,
            "system-store-read" => Self::SystemStoreRead,
            "background-audio" => Self::BackgroundAudio,
            _ => return None,
        })
    }

    /// Índice estable (bit-position en CapabilitySet de `arca-permissions`).
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::NetClient => 0,
            Self::NetServer => 1,
            Self::ClipboardRead => 2,
            Self::ClipboardWrite => 3,
            Self::Notify => 4,
            Self::Share => 5,
            Self::OpenUri => 6,
            Self::Vibrate => 7,
            Self::FsVault => 8,
            Self::SystemStoreRead => 9,
            Self::BackgroundAudio => 10,
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Capability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_name(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("capability desconocida: {s}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize as _;

    #[test]
    fn roundtrip_str() {
        for c in Capability::all() {
            assert_eq!(Capability::from_name(c.as_str()), Some(*c), "{c}");
        }
        assert!(Capability::from_name("NET-CLIENT").is_none());
        assert!(Capability::from_name("").is_none());
        assert!(Capability::from_name("nonsense").is_none());
    }

    #[test]
    fn indices_y_count_consistentes() {
        assert_eq!(Capability::count(), Capability::all().len());
        for (i, c) in Capability::all().iter().enumerate() {
            assert_eq!(c.index(), i as u32);
        }
    }

    #[test]
    fn serde_roundtrip() {
        use serde::de::value::StrDeserializer;
        use serde::de::IntoDeserializer;
        let c = Capability::FsVault;
        // Serializar a str canónico
        let d: StrDeserializer<'_, serde::de::value::Error> = c.as_str().into_deserializer();
        let back = Capability::deserialize(d);
        assert!(matches!(back, Ok(x) if x == c));
        // Capability desconocida rechazada
        let bad: StrDeserializer<'_, serde::de::value::Error> = "nonsense".into_deserializer();
        assert!(Capability::deserialize(bad).is_err());
    }
}
