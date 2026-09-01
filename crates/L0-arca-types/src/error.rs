//! Errores base de todo el ecosistema Arca.
//!
//! Todos los crates Arca usan [`Res<T>`] como resultado canónico. Los crates
//! de capa superior pueden envolver este enum (thiserror `#[source]`), nunca
//! duplicarlo (spec 01 §5: "dos definiciones de Digest/ArcaError" = bug).

use crate::caps::Capability;
use crate::ids::AppId;
use crate::version::ProtoVersion;

/// Resultado canónico de Arca.
pub type Res<T> = Result<T, ArcaError>;

/// Error base compartido por todo el ecosistema.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArcaError {
    /// La app referenciada no existe (ni instalada ni en el store).
    #[error("app no encontrada: {0}")]
    NotFound(AppId),
    /// Una operación requiere una capability que la app no tiene concedida.
    #[error("capability denegada: {cap}")]
    PermissionDenied {
        /// Capability requerida.
        cap: Capability,
    },
    /// El par habla una versión de AIPC incompatible.
    #[error("protocolo incompatible: local {have}, remoto {want}")]
    ProtocolMismatch {
        /// Versión que habla este lado.
        have: ProtoVersion,
        /// Versión que habla el otro lado.
        want: ProtoVersion,
    },
    /// Un frame/mensaje excede el límite de bytes del canal.
    #[error("frame de {bytes} B excede el límite de {limit} B")]
    FrameOverflow {
        /// Tamaño real del frame.
        bytes: usize,
        /// Límite permitido.
        limit: usize,
    },
    /// La verificación de firma ed25519 del paquete falló.
    #[error("firma inválida")]
    InvalidSignature,
    /// El contenido del paquete no coincide con lo firmado/declarado
    /// (sha de artefacto, archivo faltante/sobrante, manifest alterado).
    /// El contexto es estático a propósito: el detalle dinámico lo loguea
    /// la capa que llama (installer/7z), no el tipo de error.
    #[error("paquete inválido: {0}")]
    InvalidPackage(&'static str),
    /// Una trama AIPC no supera el framing/validación rkyv (magic, CRC,
    /// longitud o payload corruptos). El detalle dinámico va al log de la
    /// capa que llama (ipc), no al error (spec 03 §5).
    #[error("trama AIPC inválida: {0}")]
    InvalidFrame(&'static str),
    /// Error de E/S subyacente.
    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),
    /// Error interno con contexto estático (nunca datos dinámicos: evita leaks
    /// de información hacia los logs del host).
    #[error("error interno: {0}")]
    Internal(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_son_estables() {
        // Estos textos viajan a logs de diagnóstico: no cambiar a la ligera.
        if let Ok(id) = AppId::new("com.x.y") {
            let e = ArcaError::NotFound(id);
            assert!(e.to_string().contains("com.x.y"));
        }
        let e = ArcaError::FrameOverflow {
            bytes: 10,
            limit: 5,
        };
        assert_eq!(e.to_string(), "frame de 10 B excede el límite de 5 B");
        let e = ArcaError::Internal("boom");
        assert!(e.to_string().contains("boom"));
    }

    #[test]
    fn io_from_fn() {
        fn falla() -> Res<()> {
            Err(ArcaError::from(std::io::Error::other("disk")))?;
            Ok(())
        }
        assert!(matches!(falla(), Err(ArcaError::Io(_))));
    }
}
