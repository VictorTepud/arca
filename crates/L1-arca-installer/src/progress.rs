//! Progreso de instalación (spec 12 §3: callback; §6: ≤ cada 256 KiB).
//!
//! El installer emite [`InstallProgress`] por fase con fracción `[0,1]`. Las
//! fases de extracción (Manifest/Extract) heredan la granularidad de
//! `arca-7z` (un callback cada 256 KiB descomprimidos); Verify/Commit son
//! cortas y emiten 0.0 → 1.0.

/// Fase del flujo de instalación (spec 12 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstallPhase {
    /// Lectura del manifest + firma a memoria + validación de layout.
    Manifest,
    /// Extracción completa a staging (verify-while-extract).
    Extract,
    /// `StreamingVerifier::finish`: shas + digest + firma ed25519.
    Verify,
    /// Renames atómicos + transacción del store.
    Commit,
}

impl InstallPhase {
    /// Nombre canónico (logs/UI).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Extract => "extract",
            Self::Verify => "verify",
            Self::Commit => "commit",
        }
    }
}

impl std::fmt::Display for InstallPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evento de progreso: fase + fracción de esa fase en `[0,1]`.
///
/// La fracción es RELATIVA a la fase (la extracción en dos pasadas — ver
/// docs del crate — hace que Manifest y Extract cubran cada una todo el
/// paquete: es la semántica "barra por paso" de docs/10 §1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InstallProgress {
    /// Fase en curso.
    pub phase: InstallPhase,
    /// Fracción de la fase en `[0.0, 1.0]`.
    pub frac: f64,
}

impl std::fmt::Display for InstallProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {:.0}%", self.phase, self.frac * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_y_nombres() {
        assert_eq!(InstallPhase::Manifest.as_str(), "manifest");
        assert_eq!(InstallPhase::Extract.to_string(), "extract");
        assert_eq!(InstallPhase::Verify.as_str(), "verify");
        assert_eq!(InstallPhase::Commit.as_str(), "commit");
        let p = InstallProgress {
            phase: InstallPhase::Extract,
            frac: 0.5,
        };
        assert!(p.to_string().contains("50%"));
    }
}
