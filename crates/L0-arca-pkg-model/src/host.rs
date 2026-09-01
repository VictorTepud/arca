//! Variante del host Arca (ADR-001/ADR-003).

/// Variante de compilación del host que instala el paquete.
///
/// - **Libre** (targetSdk 28, la "grieta de Termux", ADR-003): ejecución
///   nativa permitida; WASM también disponible. Default: nativo.
/// - **Moderno** (targetSdk 35, contingencia): solo WASM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostVariant {
    /// Host-libre: procesos nativos + wasm.
    Libre,
    /// Host-moderno: solo wasm.
    Moderno,
}

impl HostVariant {
    /// ¿Puede ejecutar procesos nativos? Solo Libre (ADR-003).
    #[must_use]
    pub const fn can_native(self) -> bool {
        matches!(self, Self::Libre)
    }

    /// Nombre canónico (logs/wire).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Libre => "libre",
            Self::Moderno => "moderno",
        }
    }
}

impl std::fmt::Display for HostVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_libre_ejecuta_nativo() {
        assert!(HostVariant::Libre.can_native());
        assert!(!HostVariant::Moderno.can_native());
        assert_eq!(HostVariant::Libre.as_str(), "libre");
        assert_eq!(HostVariant::Moderno.to_string(), "moderno");
    }
}
