//! [`CapabilitySet`] — bitset de capabilities concedidas.
//!
//! Set canónico del ecosistema (spec 07 §3): `arca-store` mantiene una copia
//! local del bitset por persistencia, pero **el dueño del modelo vive aquí**;
//! los índices de bit provienen de `arca_types::Capability::index()` (0..=10),
//! compartidos por ambos crates para que el bitset sea intercambiable.

use arca_pkg_model::Manifest;
use arca_types::Capability;

/// Conjunto de capabilities concedidas a una sub-app.
///
/// Bitset `u32` sobre [`Capability::index`] (estable por contrato de
/// `arca-types`). Operaciones O(1), sin asignaciones.
///
/// ```
/// use arca_permissions::CapabilitySet;
/// use arca_types::Capability;
///
/// let vacio = CapabilitySet::empty();
/// assert!(!vacio.has(Capability::NetClient));
///
/// let caps = CapabilitySet::from_iter([Capability::NetClient, Capability::NetClient]);
/// assert!(caps.has(Capability::NetClient)); // duplicados: idempotente
/// assert_eq!(caps.bits(), 1); // bit 0 = net-client
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapabilitySet(
    /// Bitset crudo: bit `i` = [`Capability`] con `index() == i`.
    u32,
);

impl CapabilitySet {
    /// Set vacío (ninguna capability concedida): el sandbox base.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Reconstruye desde bits crudos (LaunchSpec de arca-launch / store).
    /// Bits desconocidos se conservan (fail-open solo en bits FUTUROS no
    /// usados por este binario: se documentan como desconocidos).
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Bits crudos del bitset (estables entre `arca-store` y este crate).
    #[must_use]
    pub const fn bits(&self) -> u32 {
        self.0
    }

    /// ¿La capability `c` está concedida?
    #[must_use]
    pub const fn has(&self, c: Capability) -> bool {
        self.0 & (1 << c.index()) != 0
    }

    /// Set con las capabilities que el manifest **solicita**
    /// (`manifest.requested_caps()`); el instalador decide qué concede.
    #[must_use]
    pub fn from_manifest(m: &Manifest) -> Self {
        m.requested_caps().iter().copied().collect()
    }

    /// Capabilities concedidas, en orden de declaración de `Capability`
    /// (determinista; útil para `explain` y paneles de diagnóstico).
    pub fn iter(&self) -> impl Iterator<Item = Capability> + use<'_> {
        Capability::all().iter().copied().filter(|c| self.has(*c))
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        let mut bits = 0u32;
        for c in iter {
            bits |= 1 << c.index();
        }
        Self(bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manifest mínimo válido con `perms` dado (helper local de tests).
    fn manifest_con(perms: &str) -> Manifest {
        let toml = format!(
            r#"
[package]
id = "dev.arca.demo"
name = "Demo"
version = "0.1.0"
min_host = "1.0.0"
api_level = 1

[runtime]
backend_pref = "any"
entry = "app"
respawn = "never"
perms = [{perms}]

[artifacts.native]
path = "bin/native-aarch64/app"
sha256 = "0101010101010101010101010101010101010101010101010101010101010101"

[profile]
launch_budget_ms = 120
max_frame_mb = 2
"#
        );
        Manifest::parse(toml.as_bytes()).unwrap_or_else(|e| panic!("manifest inválido: {e}"))
    }

    #[test]
    fn empty_no_tiene_nada() {
        let caps = CapabilitySet::empty();
        for c in Capability::all() {
            assert!(!caps.has(*c), "{} no debería estar", c);
        }
        assert_eq!(caps.bits(), 0);
        assert_eq!(caps.iter().count(), 0);
    }

    #[test]
    fn from_iter_bits_y_duplicados() {
        let caps = CapabilitySet::from_iter([
            Capability::NetClient,
            Capability::NetClient, // duplicado: idempotente
            Capability::FsVault,
        ]);
        assert_eq!(caps.bits(), (1 << 0) | (1 << 8));
        assert!(caps.has(Capability::NetClient));
        assert!(caps.has(Capability::FsVault));
        assert!(!caps.has(Capability::NetServer));
    }

    #[test]
    fn todos_los_indices_caben_en_u32() {
        let todas: CapabilitySet = Capability::all().iter().copied().collect();
        assert_eq!(todas.bits(), (1u32 << Capability::count()) - 1);
        assert_eq!(todas.iter().count(), Capability::count());
    }

    #[test]
    fn iter_sigue_el_orden_de_declaracion() {
        let caps = CapabilitySet::from_iter([
            Capability::BackgroundAudio, // índice 10
            Capability::NetClient,       // índice 0
        ]);
        let orden: Vec<Capability> = caps.iter().collect();
        assert_eq!(
            orden,
            vec![Capability::NetClient, Capability::BackgroundAudio]
        );
    }

    #[test]
    fn from_manifest_lee_requested_caps() {
        // OJO: el dialecto del manifest usa puntos ("net.client"), no el
        // kebab canónico de Capability::as_str ("net-client").
        let m = manifest_con(r#""net.client", "fs.vault""#);
        let caps = CapabilitySet::from_manifest(&m);
        assert!(caps.has(Capability::NetClient));
        assert!(caps.has(Capability::FsVault));
        assert!(!caps.has(Capability::NetServer));
        assert_eq!(caps.iter().count(), 2);
    }

    #[test]
    fn from_manifest_sin_perms_es_vacio() {
        let m = manifest_con("");
        assert_eq!(CapabilitySet::from_manifest(&m), CapabilitySet::empty());
    }
}
