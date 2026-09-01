//! Lo que un executor necesita lanzar: [`AppSpec`], [`ArtifactRef`] y
//! [`AppDirs`] (spec 13 §3) + re-export de [`RespawnPolicy`].

use std::path::PathBuf;

use arca_types::{AppId, Capability, Digest, InstanceId};

/// Re-export canónico: la política de respawn vive en `arca-pkg-model`
/// (docs/06 §3) y este crate la consume, nunca la duplica (desviación 3
/// decidida por el arquitecto).
pub use arca_pkg_model::RespawnPolicy;

/// Referencia al artefacto ejecutable de la instancia (desviación 2).
///
/// Cubre ambos backends por igual: path al binario nativo o al módulo wasm.
/// El digest blake3 es el mismo que viaja en el `Hello` del handshake
/// (anti-sustitución, docs/04 §3) y `size_bytes` el tamaño verificado por
/// el instalador al extraer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    /// Path absoluto al artefacto en disco (bin nativo o módulo wasm).
    pub path: PathBuf,
    /// Digest blake3 del contenido (el executor lo re-contrasta en spawn).
    pub hash: Digest,
    /// Tamaño en bytes del artefacto en disco.
    pub size_bytes: u64,
}

/// Directorios privados de la instancia (construcción del sandbox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    /// Raíz de la app instalada (bin/assets/icons — solo lectura para la
    /// instancia; la actualiza el instalador).
    pub app_dir: PathBuf,
    /// Bóveda privada de datos (RW, aislada por app; capability `FsVault`).
    pub vault_dir: PathBuf,
}

/// Todo lo que un [`Executor`](crate::Executor) necesita lanzar una
/// instancia (spec 13 §3). Dato puro: lo construye host-core con la
/// información del store/instalador y se lo entrega por valor a `launch`.
///
/// El mismo spec sirve para `arca.home` (sub-app de sistema): cero campos
/// ni caminos especiales.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSpec {
    /// Identidad de la app instalada.
    pub app_id: AppId,
    /// Id de instancia asignado por el host (monotónico por arranque).
    pub instance: InstanceId,
    /// Artefacto a ejecutar (bin nativo o módulo wasm + digest + tamaño).
    pub artifact: ArtifactRef,
    /// Capabilities efectivamente concedidas (subconjunto de lo pedido en
    /// el manifest; viajan al proceso en el `Welcome` del handshake).
    /// Desviación 1: `Vec<Capability>` en vez del `CapabilitySet` canónico,
    /// que vive en `arca-permissions` y docs/08 §3 lo prohíbe como
    /// dependencia de este crate.
    pub caps: Vec<Capability>,
    /// Directorios privados de la instancia (sandbox de filesystem).
    pub dirs: AppDirs,
    /// Política de respawn: la CONSULTA host-core tras un `Dead` (este
    /// crate no relanza; el ABI solo reporta).
    pub respawn: RespawnPolicy,
    /// ¿La sub-app pinta síncronamente (bloquea el frame)? Modo de
    /// presentación declarado en el manifest (`runtime.ui.sync`).
    pub sync_ui: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El spec es dato puro: clonable, comparable y con Debug útil para
    /// los logs del host (identidad + artefacto visibles).
    #[test]
    fn app_spec_es_datos_puros() {
        let a = AppSpec {
            app_id: AppId::new("com.example.app").expect("id válida"),
            instance: InstanceId::new(3),
            artifact: ArtifactRef {
                path: PathBuf::from("/data/apps/x/bin/app.so"),
                hash: Digest::of(b"binario"),
                size_bytes: 7,
            },
            caps: vec![Capability::Notify, Capability::FsVault],
            dirs: AppDirs {
                app_dir: PathBuf::from("/data/apps/x"),
                vault_dir: PathBuf::from("/data/vault/x"),
            },
            respawn: RespawnPolicy::OnCrash,
            sync_ui: true,
        };
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(
            a,
            AppSpec {
                sync_ui: false,
                ..b.clone()
            }
        );
        let dbg = format!("{a:?}");
        assert!(dbg.contains("com.example.app") && dbg.contains("app.so"));
        // Re-export apunta al tipo dueño (no hay duplicado del enum).
        assert_eq!(RespawnPolicy::OnCrash.as_str(), "on-crash");
    }
}
