//! Repositorio de apps + capabilities concedidas (spec 11 §3).
//!
//! Instalación/update de registro = [`Store::upsert_app`] (UNA transacción
//! del llamador); uninstall = [`Store::delete_app`] (las filas hijas caen
//! por `ON DELETE CASCADE`; el audit sobrevive).

use arca_pkg_model::Manifest;
use arca_types::{AppId, ArcaError, Capability, Res};
use rusqlite::{params, Connection};

use crate::model::{AppRecord, CapabilitySet, Filter, InstallSource, UnixMs};
use crate::tx::Tx;
use crate::Store;

/// Mapa sqlite → [`ArcaError`] de ESTE módulo.
fn db(ctx: &'static str, e: rusqlite::Error) -> ArcaError {
    tracing::error!(target: "arca::arca-store::apps", ctx, error = %e, "fallo sqlite");
    ArcaError::Internal(ctx)
}

/// Fila cruda de `apps` (orden de columnas compartido por SELECT/INSERT).
type FilaApp = (
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    i64,
    i64,
);

/// Lee una fila de apps como tupla cruda (todo texto/entero: sin fallos).
fn fila_de_apps(r: &rusqlite::Row<'_>) -> rusqlite::Result<FilaApp> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

/// Tupla cruda → [`AppRecord`]. Fallo = registry corrupto (error interno,
/// detalle al log). `id_conocido` evita re-validar el AppId del caller.
fn app_de_fila(id_conocido: Option<AppId>, f: FilaApp) -> Res<AppRecord> {
    let (
        id_s,
        name,
        version,
        min_host,
        api_level,
        description,
        tags,
        src,
        installed_at,
        updated_at,
    ) = f;
    let id = match id_conocido {
        Some(i) => i,
        None => AppId::new(&id_s)?, // ya validado al escribir; aquí sería corrupción
    };
    let installed_from = match InstallSource::from_name(&src) {
        Some(s) => s,
        None => {
            tracing::error!(
                target: "arca::arca-store::apps",
                origen = %src,
                "installed_from corrupto en registry"
            );
            return Err(ArcaError::Internal(
                "store: installed_from corrupto en registry",
            ));
        }
    };
    let api_level = match u32::try_from(api_level) {
        Ok(v) => v,
        Err(_) => {
            tracing::error!(
                target: "arca::arca-store::apps",
                api_level,
                "api_level fuera de rango en registry"
            );
            return Err(ArcaError::Internal("store: api_level corrupto en registry"));
        }
    };
    // tags: 'a,b,c' (charset [a-z0-9-] del manifest: sin comas que escapar)
    let tags = tags
        .split(',')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();
    Ok(AppRecord {
        id,
        name,
        version,
        min_host,
        api_level,
        description,
        tags,
        installed_from,
        installed_at: UnixMs::from_millis(installed_at),
        updated_at: UnixMs::from_millis(updated_at),
    })
}

/// ¿Existe la app? (NotFound si no — usado por grant/revoke/caps_of).
fn ensure_app(conn: &Connection, id: &AppId) -> Res<()> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM apps WHERE id = ?1",
            params![id.as_str()],
            |r| r.get(0),
        )
        .map_err(|e| db("store: existencia de app", e))?;
    if n == 0 {
        Err(ArcaError::NotFound(id.clone()))
    } else {
        Ok(())
    }
}

impl Store {
    /// Instala o actualiza el REGISTRO de una app (spec 11 §3: install/update).
    ///
    /// Dentro de la `tx` del llamador (installer coordina filesystem:
    /// archivos primero, commit de db al final):
    ///
    /// 1. Upsert de la fila `apps` (en update se preserva `installed_at`
    ///    y avanza `updated_at`).
    /// 2. Las capabilities del manifest quedan **concedidas** (granted al
    ///    instalar, spec 11 §3): `INSERT OR IGNORE` — una cap ya concedida
    ///    conserva su `granted_at` original.
    /// 3. Se retiran las caps que el manifest ya no pide (el manifest manda
    ///    en cada update; el usuario puede revocar después con
    ///    [`Store::revoke_cap`]).
    pub fn upsert_app(&self, tx: &mut Tx, m: &Manifest, installed_from: InstallSource) -> Res<()> {
        let p = &m.package;
        let ahora = UnixMs::now().get();
        let tags = p.tags.join(",");
        let conn = &tx.guard;
        conn.execute(
            "INSERT INTO apps (id, name, version, min_host, api_level, description, tags, \
             installed_from, installed_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(id) DO UPDATE SET \
                name = excluded.name, version = excluded.version, \
                min_host = excluded.min_host, api_level = excluded.api_level, \
                description = excluded.description, tags = excluded.tags, \
                installed_from = excluded.installed_from, updated_at = excluded.updated_at",
            params![
                p.id.as_str(),
                p.name.as_str(),
                p.version.to_string(),
                p.min_host.to_string(),
                p.api_level,
                p.description.as_str(),
                tags,
                installed_from.as_str(),
                ahora,
                ahora
            ],
        )
        .map_err(|e| db("store: upsert de app", e))?;
        let caps = m.requested_caps();
        {
            let mut stmt = conn
                .prepare(
                    "INSERT OR IGNORE INTO app_caps (app_id, cap, granted_at) \
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| db("store: preparar caps de install", e))?;
            for c in caps {
                stmt.execute(params![p.id.as_str(), c.as_str(), ahora])
                    .map_err(|e| db("store: cap concedida al instalar", e))?;
            }
        }
        // Retiradas del manifest: DELETE ... NOT IN (caps pedidas).
        if caps.is_empty() {
            conn.execute(
                "DELETE FROM app_caps WHERE app_id = ?1",
                params![p.id.as_str()],
            )
            .map_err(|e| db("store: retirar caps del manifest", e))?;
        } else {
            let mut sql = String::from("DELETE FROM app_caps WHERE app_id = ?1 AND cap NOT IN (");
            for i in 0..caps.len() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push_str(&format!("?{}", i + 2));
            }
            sql.push(')');
            let mut args: Vec<String> = vec![p.id.as_str().to_string()];
            args.extend(caps.iter().map(|c| c.as_str().to_string()));
            conn.execute(&sql, rusqlite::params_from_iter(args))
                .map_err(|e| db("store: retirar caps del manifest", e))?;
        }
        Ok(())
    }

    /// Registro de una app instalada (`None` si no está).
    pub fn get_app(&self, id: &AppId) -> Res<Option<AppRecord>> {
        let conn = self.lock()?;
        match conn.query_row(
            "SELECT id, name, version, min_host, api_level, description, tags, \
             installed_from, installed_at, updated_at FROM apps WHERE id = ?1",
            params![id.as_str()],
            fila_de_apps,
        ) {
            Ok(f) => Ok(Some(app_de_fila(Some(id.clone()), f)?)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db("store: get_app", e)),
        }
    }

    /// Apps instaladas según filtro, ordenadas por nombre (determinista).
    ///
    /// Una sola query compilada: los predicatos ausentes viajan como NULL
    /// (`:cap IS NULL OR ...`) y el JOIN con `app_caps` solo filtra cuando
    /// hay capability pedida.
    pub fn list_apps(&self, filter: Filter) -> Res<Vec<AppRecord>> {
        let conn = self.lock()?;
        let cap = filter.cap().map(|c| c.as_str());
        let src = filter.source().map(|s| s.as_str());
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT a.id, a.name, a.version, a.min_host, a.api_level, \
                 a.description, a.tags, a.installed_from, a.installed_at, a.updated_at \
                 FROM apps a LEFT JOIN app_caps c ON c.app_id = a.id \
                 WHERE (:cap IS NULL OR c.cap = :cap) \
                   AND (:src IS NULL OR a.installed_from = :src) \
                 ORDER BY a.name, a.id",
            )
            .map_err(|e| db("store: preparar list_apps", e))?;
        let filas = stmt
            .query_map(
                rusqlite::named_params! { ":cap": cap, ":src": src },
                fila_de_apps,
            )
            .map_err(|e| db("store: list_apps", e))?;
        let mut out = Vec::new();
        for f in filas {
            let f = f.map_err(|e| db("store: list_apps (fila)", e))?;
            out.push(app_de_fila(None, f)?);
        }
        Ok(out)
    }

    /// Desinstala el REGISTRO (spec 11 §3: uninstall) dentro de la `tx`.
    ///
    /// Las filas hijas (`app_caps`, `instances`) caen por `ON DELETE
    /// CASCADE`; el audit NO se toca (append-only, ver esquema).
    /// App inexistente → [`ArcaError::NotFound`] (que el llamador decida si
    /// es idempotencia de sweep o bug).
    pub fn delete_app(&self, tx: &mut Tx, id: &AppId) -> Res<()> {
        let conn = &tx.guard;
        let n = conn
            .execute("DELETE FROM apps WHERE id = ?1", params![id.as_str()])
            .map_err(|e| db("store: delete de app", e))?;
        if n == 0 {
            return Err(ArcaError::NotFound(id.clone()));
        }
        Ok(())
    }

    /// Concede capabilities a una app instalada (dentro de la `tx`).
    ///
    /// Idempotente: conceder dos veces conserva el `granted_at` original.
    pub fn grant_caps(&self, tx: &mut Tx, id: &AppId, caps: &[Capability]) -> Res<()> {
        let conn = &tx.guard;
        ensure_app(conn, id)?;
        let mut stmt = conn
            .prepare(
                "INSERT OR IGNORE INTO app_caps (app_id, cap, granted_at) \
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(|e| db("store: preparar grant", e))?;
        for c in caps {
            stmt.execute(params![id.as_str(), c.as_str(), UnixMs::now().get()])
                .map_err(|e| db("store: grant de capability", e))?;
        }
        Ok(())
    }

    /// Revoca UNA capability (dentro de la `tx`).
    ///
    /// Revocar algo ya revocado es `Ok(())` (idempotente); app inexistente →
    /// [`ArcaError::NotFound`].
    pub fn revoke_cap(&self, tx: &mut Tx, id: &AppId, cap: Capability) -> Res<()> {
        let conn = &tx.guard;
        ensure_app(conn, id)?;
        conn.execute(
            "DELETE FROM app_caps WHERE app_id = ?1 AND cap = ?2",
            params![id.as_str(), cap.as_str()],
        )
        .map_err(|e| db("store: revoke de capability", e))?;
        Ok(())
    }

    /// Capabilities concedidas de una app (para seccomp y broker).
    ///
    /// App inexistente → [`ArcaError::NotFound`]: en el flujo de lanzamiento
    /// (docs/10 §2) es un error real, no un vacío silencioso.
    pub fn caps_of(&self, id: &AppId) -> Res<CapabilitySet> {
        let conn = self.lock()?;
        ensure_app(&conn, id)?;
        let mut stmt = conn
            .prepare("SELECT cap FROM app_caps WHERE app_id = ?1 ORDER BY cap")
            .map_err(|e| db("store: preparar caps_of", e))?;
        let mut rows = stmt
            .query(params![id.as_str()])
            .map_err(|e| db("store: caps_of", e))?;
        let mut set = CapabilitySet::empty();
        while let Some(fila) = rows.next().map_err(|e| db("store: caps_of (fila)", e))? {
            let cap_s: String = fila.get(0).map_err(|e| db("store: caps_of (columna)", e))?;
            match Capability::from_name(&cap_s) {
                Some(c) => set.insert(c),
                None => {
                    tracing::error!(
                        target: "arca::arca-store::apps",
                        cap = %cap_s,
                        "capability desconocida en registry"
                    );
                    return Err(ArcaError::Internal(
                        "store: capability corrupta en registry",
                    ));
                }
            }
        }
        Ok(set)
    }
}
