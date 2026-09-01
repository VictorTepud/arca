//! Tipos de fila del registro (filas ↔ structs, spec 11 §2).
//!
//! Todo lo que [`crate::Store`] persiste y devuelve vive aquí. Dos tipos del
//! contrato de spec 11 §3 NO existen en los crates permitidos y se definen
//! localmente (documentado):
//!
//! - `DateTime` → [`UnixMs`]: arca-types solo expone reloj monotónico
//!   ([`arca_types::now_mono_ns`]) y está cerrado (T02); el auditoría necesita
//!   reloj de PARED para "query por tiempo".
//! - `CapabilitySet` → [`CapabilitySet`]: el canónico vivirá en
//!   `arca-permissions` (T14), que NO está entre las dependencias permitidas
//!   de spec 11 §2 (y hoy es esqueleto). Este bitset mínimo es convertible 1:1
//!   cuando T14 aterrice.

use std::time::{SystemTime, UNIX_EPOCH};

use arca_types::{AppId, Capability, InstanceId};
use serde::{Deserialize, Serialize};

/// Marca temporal unix en milisegundos (reloj de pared).
///
/// El valor 0 significa "origen unix" (o reloj del sistema roto hacia atrás:
/// se satura, nunca se pánico).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UnixMs(i64);

impl UnixMs {
    /// Momento actual (reloj de pared del sistema).
    ///
    /// Si el reloj está antes del epoch (dispositivo sin batería/CMOS), se
    /// satura a 0: el auditoría ordena por `ts`, y un valor negativo solo
    /// añadiría ruido.
    #[must_use]
    pub fn now() -> Self {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // u128 → i64 saturado, sin `as` silencioso.
        Self(ms.min(i64::MAX as u128) as i64)
    }

    /// Construye desde milisegundos unix crudos.
    #[must_use]
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Valor crudo (para SQL/logs).
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for UnixMs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

/// Origen de instalación de un paquete: quién lo trajo al dispositivo.
///
/// Se persiste como texto canónico (`user`/`bundled`/`dev`) en `apps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstallSource {
    /// El usuario lo eligió con el SAF picker (docs/10 §1).
    User,
    /// Venía embebido en el host/ROM (devapps, `arca.home`).
    Bundled,
    /// Sideload de desarrollo (`arca-tools-dev`, adb).
    Dev,
}

impl InstallSource {
    /// Nombre canónico en columna SQL (estable, minúsculas).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Bundled => "bundled",
            Self::Dev => "dev",
        }
    }

    /// Parse del nombre canónico (`None` si es desconocido).
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Self::User,
            "bundled" => Self::Bundled,
            "dev" => Self::Dev,
            _ => return None,
        })
    }
}

impl std::fmt::Display for InstallSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Cómo terminó una instancia (docs/10 §9: crash → respawn según manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    /// Terminó por sí misma con código de salida (`exit:<code>`).
    Exited {
        /// Código de salida del proceso (wasm: exit code del runtime).
        code: i32,
    },
    /// Murió por señal/crash (minidump emitido por `arca-rt`).
    Crashed,
    /// El host lo mató (freezer, shutdown, uninstall).
    Killed,
}

impl Outcome {
    /// Forma canónica en columna SQL `instances.outcome`.
    #[must_use]
    pub fn as_sql(&self) -> String {
        match *self {
            Self::Exited { code } => format!("exit:{code}"),
            Self::Crashed => "crash".into(),
            Self::Killed => "killed".into(),
        }
    }

    /// Parse de la columna SQL (`None` si es desconocida).
    #[must_use]
    pub fn from_sql(s: &str) -> Option<Self> {
        Some(match s {
            "crash" => Self::Crashed,
            "killed" => Self::Killed,
            _ => {
                let (pref, code) = s.split_once(':')?;
                if pref != "exit" {
                    return None;
                }
                Self::Exited {
                    code: code.parse().ok()?,
                }
            }
        })
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Exited { code } => write!(f, "exit:{code}"),
            Self::Crashed => f.write_str("crash"),
            Self::Killed => f.write_str("killed"),
        }
    }
}

/// Fila de `apps`: una sub-app instalada tal como la ve el launcher.
///
/// Persistencia deliberadamente ESCALAR (sin el manifest completo: el
/// filesystem ya lo tiene en `apps/<id>/current/manifest.toml`; añadir
/// columnas nuevas = migración futura versionada).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRecord {
    /// Id de la app.
    pub id: AppId,
    /// Nombre visible (para el launcher).
    pub name: String,
    /// Versión instalada (semver como texto; parsea el llamador si compara).
    pub version: String,
    /// Versión mínima de host declarada (metadato de diagnóstico).
    pub min_host: String,
    /// Nivel del contrato ABI/UI.
    pub api_level: u32,
    /// Descripción (metadato del manifest).
    pub description: String,
    /// Tags de store (charset `[a-z0-9-]`: se persisten `'a,b,c'` sin escapes).
    pub tags: Vec<String>,
    /// Origen de la instalación.
    pub installed_from: InstallSource,
    /// Cuándo se instaló (no cambia en updates).
    pub installed_at: UnixMs,
    /// Último update del registro (v2 del esquema).
    pub updated_at: UnixMs,
}

/// Spawn de una instancia registrado al lanzar (histórico de ejecución).
///
/// Sin `Serialize`: `InstanceId` no lo implementa (T02 cerrado) y la regla
/// de no duplicar tipos lo deja fuera del wire por ahora; el host consulta
/// esto en proceso.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRecord {
    /// Id asignado por el host (monotónico por arranque, spec 01 §3).
    pub instance_id: InstanceId,
    /// App que corre.
    pub app_id: AppId,
    /// Versión del paquete en el momento del spawn (tras update, la nueva).
    pub version: String,
    /// Momento del spawn.
    pub started_at: UnixMs,
}

/// Evento de auditoría: uso de una capability por una app.
///
/// Lo escribe `svc-broker` (net/clipboard/notify usados — spec 11 §3) y lo
/// consulta el panel de diagnóstico por app/tiempo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// App que usó la capability.
    pub app_id: AppId,
    /// Capability ejercida.
    pub cap: Capability,
    /// Momento del uso (reloj de pared).
    pub ts: UnixMs,
    /// Detalle corto (p. ej. `connect tcp:443`); vacío si no aplica.
    pub detail: String,
}

/// Filtro de listado para [`crate::Store::list_apps`].
///
/// Builder inmutable: parte de [`Filter::all`] y se encadena
/// (`.with_cap(..)` / `.from(..)`); el store compila UNA query con los
/// predicados presentes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Solo apps con esta capability concedida.
    cap: Option<Capability>,
    /// Solo apps instaladas desde este origen.
    source: Option<InstallSource>,
}

impl Filter {
    /// Sin condiciones: todas las apps instaladas.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            cap: None,
            source: None,
        }
    }

    /// Restringe a apps con la capability concedida (panel de permisos).
    #[must_use]
    pub const fn with_cap(mut self, cap: Capability) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Restringe por origen de instalación.
    #[must_use]
    pub const fn from(mut self, source: InstallSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Capability pedida, si la hay.
    #[must_use]
    pub const fn cap(&self) -> Option<Capability> {
        self.cap
    }

    /// Origen pedido, si lo hay.
    #[must_use]
    pub const fn source(&self) -> Option<InstallSource> {
        self.source
    }
}

/// Conjunto de capabilities concedidas a una app (bitset `u16`).
///
/// NOTA(agent): el contrato de spec 11 §3 devuelve `CapabilitySet`, tipo que
/// pertenecerá a `arca-permissions` (T14). Como ese crate NO está en las
/// dependencias permitidas de spec 11 §2, aquí vive un bitset mínimo basado
/// en [`Capability::index`] (≤ 16 capabilities en v1). Conversión 1:1 cuando
/// T14 aterrice (mismo índice).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CapabilitySet {
    /// Bit por capability (`1 << cap.index()`).
    bits: u16,
}

impl CapabilitySet {
    /// Conjunto vacío.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Inserta una capability (idempotente).
    pub fn insert(&mut self, cap: Capability) {
        self.bits |= 1 << cap.index();
    }

    /// ¿Contiene la capability?
    #[must_use]
    pub const fn contains(&self, cap: Capability) -> bool {
        self.bits & (1 << cap.index()) != 0
    }

    /// Número de capabilities presentes.
    #[must_use]
    pub const fn len(&self) -> usize {
        // popcount const (no hay const::count_ones estable en todos los
        // rustc del workspace... sí lo hay desde 1.32; se itera para claridad).
        let mut n = 0;
        let mut b = self.bits;
        while b != 0 {
            n += (b & 1) as usize;
            b >>= 1;
        }
        n
    }

    /// ¿Vacío?
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Capabilities presentes, en orden de declaración del enum.
    pub fn iter(&self) -> impl Iterator<Item = Capability> {
        // Copia (Self es Copy): el iterador devuelto no debe capturar el
        // lifetime de `&self` (RPIT de edition 2021 no lo captura solo).
        let bits = *self;
        Capability::all()
            .iter()
            .copied()
            .filter(move |c| bits.contains(*c))
    }
}

impl std::iter::FromIterator<Capability> for CapabilitySet {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        let mut set = Self::empty();
        for c in iter {
            set.insert(c);
        }
        set
    }
}

impl std::fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for c in self.iter() {
            if !first {
                f.write_str(",")?;
            }
            first = false;
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_set_insert_contains_iter() {
        let mut s = CapabilitySet::empty();
        assert!(s.is_empty());
        s.insert(Capability::NetClient);
        s.insert(Capability::Notify);
        s.insert(Capability::NetClient); // idempotente
        assert_eq!(s.len(), 2);
        assert!(s.contains(Capability::NetClient));
        assert!(s.contains(Capability::Notify));
        assert!(!s.contains(Capability::FsVault));
        let v: Vec<Capability> = s.iter().collect();
        assert_eq!(v, vec![Capability::NetClient, Capability::Notify]);
        assert_eq!(s.to_string(), "net-client,notify");
    }

    #[test]
    fn capability_set_from_iter() {
        let s: CapabilitySet = [Capability::Share, Capability::Vibrate]
            .into_iter()
            .collect();
        assert!(s.contains(Capability::Share) && s.contains(Capability::Vibrate));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn outcome_sql_roundtrip() {
        for o in [
            Outcome::Exited { code: 0 },
            Outcome::Exited { code: -11 },
            Outcome::Crashed,
            Outcome::Killed,
        ] {
            assert_eq!(Outcome::from_sql(&o.as_sql()), Some(o), "{o:?}");
        }
        assert_eq!(Outcome::from_sql("exit:nope"), None);
        assert_eq!(Outcome::from_sql("nonsense"), None);
        assert_eq!(Outcome::from_sql("exit:"), None);
    }

    #[test]
    fn install_source_roundtrip() {
        for s in [
            InstallSource::User,
            InstallSource::Bundled,
            InstallSource::Dev,
        ] {
            assert_eq!(InstallSource::from_name(s.as_str()), Some(s));
        }
        assert_eq!(InstallSource::from_name("USER"), None);
    }

    #[test]
    fn unix_ms_orden_y_display() {
        assert!(UnixMs::now().get() > 1_600_000_000_000); // ~2020
        assert!(UnixMs::from_millis(5) < UnixMs::from_millis(6));
        assert_eq!(UnixMs::from_millis(42).to_string(), "42ms");
    }

    #[test]
    fn filter_builder() {
        let f = Filter::all()
            .with_cap(Capability::NetClient)
            .from(InstallSource::Dev);
        assert_eq!(f.cap(), Some(Capability::NetClient));
        assert_eq!(f.source(), Some(InstallSource::Dev));
        assert_eq!(Filter::all(), Filter::default());
    }
}
