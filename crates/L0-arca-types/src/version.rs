//! Versionado del protocolo AIPC (spec 01 §3, docs/04-protocolo-aipc.md).

use std::cmp::Ordering;

/// Versión de protocolo `AIPC-x.y` hablada por un extremo del canal.
///
/// Regla de compatibilidad (sempre la misma): **misma `major`, `minor` ≥**.
/// Un bump de `major` = protocolo incompatible (rechazar handshake); un bump
/// de `minor` = mensajes nuevos, viejos intactos (golden tests obligatorios
/// en `arca-protocol`, spec 01 §5 fila 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(Debug, PartialEq), compare(PartialEq)))]
pub struct ProtoVersion {
    /// Major: rompe wire-compat.
    pub major: u8,
    /// Minor: adiciones compatibles.
    pub minor: u8,
}

impl ProtoVersion {
    /// Constructor const-friendly (invariante: sin allocación, spec 01 §4).
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// ¿Puedo yo (=`self`) hablar con un par que anuncia `want`?
    ///
    /// `self` es la versión LOCAL; `want` la que el par pidió en handshake.
    /// Compatibilidad = misma major y local_minor ≥ remota (el menor de los
    /// dos manda en el wire).
    #[must_use]
    pub const fn is_compatible(self, want: Self) -> bool {
        self.major == want.major && self.minor >= want.minor
    }

    /// Versión efectiva a usar en la conexión (la menor de las compatibles).
    /// `None` si las major difieren. Cada lado anuncia su MÁXIMO soportado y
    /// la conexión usa el mínimo de ambos rangos.
    #[must_use]
    pub const fn negotiate(self, want: Self) -> Option<Self> {
        if self.major == want.major {
            Some(Self::new(
                self.major,
                if self.minor < want.minor {
                    self.minor
                } else {
                    want.minor
                },
            ))
        } else {
            None
        }
    }

    /// Empaquetado wire u16 (major<<8 | minor) — golden-test estable.
    #[must_use]
    pub const fn to_wire(self) -> u16 {
        ((self.major as u16) << 8) | self.minor as u16
    }

    /// Desempaquetado desde wire u16.
    #[must_use]
    pub const fn from_wire(v: u16) -> Self {
        Self::new((v >> 8) as u8, (v & 0xff) as u8)
    }
}

impl std::fmt::Display for ProtoVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl PartialOrd for ProtoVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ProtoVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibilidad_misma_major_minor_le() {
        // (local, remota, esperado)
        let casos = [
            (ProtoVersion::new(1, 0), ProtoVersion::new(1, 0), true),
            (ProtoVersion::new(1, 2), ProtoVersion::new(1, 0), true),
            (ProtoVersion::new(1, 0), ProtoVersion::new(1, 2), false),
            (ProtoVersion::new(2, 0), ProtoVersion::new(1, 9), false),
            (ProtoVersion::new(1, 9), ProtoVersion::new(2, 0), false),
            (ProtoVersion::new(0, 1), ProtoVersion::new(0, 1), true),
        ];
        for (have, want, exp) in casos {
            assert_eq!(have.is_compatible(want), exp, "{have} vs {want}");
        }
    }

    #[test]
    fn negotiate_toma_el_menor() {
        let a = ProtoVersion::new(1, 3);
        let b = ProtoVersion::new(1, 5);
        assert_eq!(a.negotiate(b), Some(ProtoVersion::new(1, 3)));
        assert_eq!(b.negotiate(a), Some(ProtoVersion::new(1, 3)));
        assert_eq!(a.negotiate(ProtoVersion::new(2, 0)), None);
    }

    #[test]
    fn wire_roundtrip() {
        for (maj, min) in [(0u8, 0u8), (1, 0), (1, 7), (2, 255), (255, 255)] {
            let v = ProtoVersion::new(maj, min);
            assert_eq!(ProtoVersion::from_wire(v.to_wire()), v, "{v}");
        }
        assert_eq!(ProtoVersion::new(1, 0).to_wire(), 0x0100);
    }

    #[test]
    fn ord_total() {
        let mut v = [
            ProtoVersion::new(1, 2),
            ProtoVersion::new(0, 9),
            ProtoVersion::new(1, 10),
        ];
        v.sort();
        assert_eq!(v[0], ProtoVersion::new(0, 9));
        assert_eq!(v[2], ProtoVersion::new(1, 10)); // 1.10 > 1.2 (numérico, no lexicográfico)
    }
}
