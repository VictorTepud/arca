//! Identidades inmutables del ecosistema (spec 01 §3).

use crate::error::ArcaError;
use smol_str::SmolStr;

/// Identidad de una sub-app instalada. `^[[a-z0-9].]{3,64}$` validado en
/// constructor — regex incumplida = [`ArcaError::Internal`] con contexto
/// (spec 01 §5, fila 1).
///
/// Formato recomendado estilo DNS inverso: `com.autor.app`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct AppId(pub SmolStr);

impl AppId {
    /// Valida y construye. Solo `[a-z0-9.]`, longitud 3..=64.
    pub fn new(s: &str) -> Result<Self, ArcaError> {
        let len = s.len();
        if !(3..=64).contains(&len) {
            return Err(ArcaError::Internal("AppId: longitud debe ser 3..=64"));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.')
        {
            return Err(ArcaError::Internal("AppId: solo [a-z0-9.]"));
        }
        Ok(Self(SmolStr::new(s)))
    }

    /// String interno (barato: SmolStr inline).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for AppId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for AppId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = SmolStr::deserialize(d)?;
        AppId::new(&s).map_err(|_| serde::de::Error::custom(format!("AppId inválido: {s}")))
    }
}

/// Instancia en ejecución de una sub-app. Monotónico por arranque del host
/// (el host es el único asignador: no usar aleatorio, spec 01 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct InstanceId(u64);

impl InstanceId {
    /// Primer valor válido (0 se reserva como "ninguno").
    pub const FIRST: InstanceId = InstanceId(1);

    /// Construye desde un u64 ya asignado por el host.
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// Valor crudo (para wire/logs).
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "inst:{}", self.0)
    }
}

/// Identidad de una ventana en el host (WM asigna; reutilizable tras cerrar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct WinId(u32);

impl WinId {
    /// Valor "sin ventana" (input global, errores).
    pub const NONE: WinId = WinId(0);

    /// Construye desde u32.
    #[must_use]
    pub const fn new(v: u32) -> Self {
        Self(v)
    }

    /// Valor crudo.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for WinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "win:{}", self.0)
    }
}

/// Sesión de ejecución (aleatoria, 128 bits): distingue dos corridas distintas
/// de la MISMA app (p. ej. tras update o crash+respawn). Se genera en el host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Sesión nula (solo para consts antes del handshake).
    pub const ZERO: SessionId = SessionId([0u8; 16]);

    /// Construye desde bytes crudos.
    #[must_use]
    pub const fn from_bytes(b: [u8; 16]) -> Self {
        Self(b)
    }

    /// Bytes crudos.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 30 casos válidos/inválidos (spec 01 §6, table-driven).
    #[test]
    fn app_id_table() {
        let mut ok: Vec<String> = [
            "abc",
            "a.b",
            "com.example.app",
            "dev.arca.home",
            "x9z",
            "123",
            "a.b.c.d.e.f",
            "app1.app2",
            "abc.",
            ".abc",
            "a..b",
            "com.exa.mple",
            "0.0.0",
            "aaa",
            "a.b.c",
            "com.x.y.z.w.v.u.t.s",
            "q.r",
            "com..",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        ok.push("z".repeat(64));
        for s in &ok {
            assert!(AppId::new(s).is_ok(), "debía ser válido: {s}");
        }
        let mut bad: Vec<String> = [
            "",
            "a",
            "ab",
            "AB",
            "Abc",
            "a-b",
            "a b",
            "a_b",
            "app!",
            "com.\u{00e9}xito",
            "com.-x",
            "\u{4e2d}\u{6587}",
            "WITH-DASH",
            "a\tb",
            "a\nb",
            "com/app",
            "com:app",
            "com;app",
            "x y",
            "COM.EXAMPLE",
            "\u{1f600}.app",
            "a.b c",
            "a\t",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        bad.push("a".repeat(65));
        bad.push("b".repeat(100));
        for s in &bad {
            assert!(AppId::new(s).is_err(), "debía ser inválido: {s:?}");
        }
        // La spec exige ≥30 casos table-driven (regex-exacta: puntos en
        // cualquier posición SÍ son válidos; el saneo extra vive en pkg-model).
        assert!(ok.len() + bad.len() >= 30, "tabla mínima de 30 casos");
    }

    #[test]
    fn ids_display() {
        let id = AppId::new("com.example.app");
        let s = id.map(|x| x.to_string()).map_err(|_| ()).ok();
        assert_eq!(s.as_deref(), Some("com.example.app"));
        assert_eq!(InstanceId::new(7).to_string(), "inst:7");
        assert_eq!(WinId::new(3).to_string(), "win:3");
        assert_eq!(SessionId::ZERO.to_string(), "0".repeat(32));
    }

    #[test]
    fn instance_ordenable() {
        // El host compara instancias para ordenar p. ej. la lista LRU.
        assert!(InstanceId::new(2) > InstanceId::new(1));
        assert_eq!(InstanceId::FIRST, InstanceId::new(1));
    }
}
