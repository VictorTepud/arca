//! `LaunchSpec`: el blob binario que viaja por el fd 3 hacia arca-launch.
//!
//! Formato propio (determinista, sin deps de serialización — es el único
//! canal antes de que exista protocolo alguno):
//! ```text
//!  0  magic    "ARCLNC1\0" (8 B)
//!  8  version  u16 = 2
//! 10  rsvd     u16
//! 12  caps_bits u32          (bitmask de Capability::index())
//! 16  instance u64
//! 24  artifact [u8; 32]      (blake3 del binario)
//! 56  app_path  u16 + bytes
//! ..  app_dir   u16 + bytes  (cwd del hijo)
//! ..  vault_dir u16 + bytes
//! ..  app_id    u16 + bytes
//! ..  env_extra u16 count + pares (u16 klen + k, u16 vlen + v)
//! ..  crc32     u32          (crc32fast de TODO lo anterior)
//! ```
//!
//! v2 (fix de las e2e flaky): se añade `env_extra` — el env del hijo pasa a
//! ser **hermético**: nace de esta spec (identidad + pares explícitos), NO
//! del entorno del proceso host. Antes el lanzador filtraba las `ARCA_*`
//! del entorno del host, y como los tests corren en paralelo dentro de UN
//! proceso, el `ARCA_PING_PANIC=1` de un test se colaba al hijo de OTRO
//! test (dos e2e fallaban según el interleaving de la máquina).

use arca_types::{ArcaError, Res};

/// Magic del blob de lanzamiento.
pub const MAGIC: &[u8; 8] = b"ARCLNC1\0";
/// Versión del formato.
pub const VERSION: u16 = 2;

/// Máximo de pares `env_extra` (mantiene el blob acotado).
pub const ENV_EXTRA_MAX: usize = 16;
/// Longitud máxima de clave/valor de un par.
const ENV_STR_MAX: usize = 256;

/// Claves reservadas de identidad: `env_extra` no puede sobreescribirlas
/// (el handshake del rt las valida — sustituirlas rompería la instancia).
const ENV_RESERVADAS: [&str; 4] = [
    "ARCA_APP_ID",
    "ARCA_INSTANCE",
    "ARCA_ARTIFACT",
    "ARCA_VAULT",
];

/// Valida un par `env_extra`: clave ASCII `ARCA_*` no reservada, sin NUL,
/// longitudes acotadas (fail-closed: un par inválido aborta el lanzamiento
/// ANTES del fork).
pub fn validar_env_extra(pares: &[(String, String)]) -> Res<()> {
    let bad = |m: &'static str| ArcaError::InvalidPackage(m);
    if pares.len() > ENV_EXTRA_MAX {
        return Err(bad("launch spec: demasiados pares env_extra"));
    }
    for (k, v) in pares {
        if k.is_empty() || k.len() > ENV_STR_MAX || v.len() > ENV_STR_MAX {
            return Err(bad("launch spec: env_extra de longitud inválida"));
        }
        if !k.is_ascii() || k.contains('\0') || v.contains('\0') {
            return Err(bad("launch spec: env_extra con caracteres inválidos"));
        }
        if !k.starts_with("ARCA_") {
            return Err(bad("launch spec: env_extra debe ser ARCA_*"));
        }
        if ENV_RESERVADAS.contains(&k.as_str()) {
            return Err(bad("launch spec: env_extra no puede tocar la identidad"));
        }
    }
    Ok(())
}

/// Especificación de lanzamiento (lado host → arca-launch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// Ruta absoluta al binario ELF estático-PIE de la app.
    pub app_path: String,
    /// Directorio de la app (cwd del hijo + allowed_paths del perfil).
    pub app_dir: String,
    /// Bóveda de la app (allowed_paths del perfil; FsVault).
    pub vault_dir: String,
    /// Id de la app (env ARCA_APP_ID para el handshake del rt).
    pub app_id: String,
    /// Instancia asignada por el host.
    pub instance: u64,
    /// Bitmask de capabilities concedidas.
    pub caps_bits: u32,
    /// Digest blake3 del artefacto (anti-sustitución del handshake).
    pub artifact: [u8; 32],
    /// Pares de env extra para el hijo (solo `ARCA_*`, ver
    /// [`validar_env_extra`]). El env del hijo es hermético: identidad de
    /// la spec + estos pares — NADA del entorno del proceso host.
    pub env_extra: Vec<(String, String)>,
}

impl LaunchSpec {
    /// Hex del artifact digest (env ARCA_ARTIFACT).
    #[must_use]
    pub fn artifact_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.artifact {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Serializa a bytes (determinista).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        fn put_str(out: &mut Vec<u8>, s: &str) {
            let b = s.as_bytes();
            out.extend_from_slice(&(b.len() as u16).to_le_bytes());
            out.extend_from_slice(b);
        }
        let mut out = Vec::with_capacity(160);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&self.caps_bits.to_le_bytes());
        out.extend_from_slice(&self.instance.to_le_bytes());
        out.extend_from_slice(&self.artifact);
        put_str(&mut out, &self.app_path);
        put_str(&mut out, &self.app_dir);
        put_str(&mut out, &self.vault_dir);
        put_str(&mut out, &self.app_id);
        // env_extra: u16 count + pares (u16 klen, k, u16 vlen, v)
        out.extend_from_slice(&(self.env_extra.len() as u16).to_le_bytes());
        for (k, v) in &self.env_extra {
            put_str(&mut out, k);
            put_str(&mut out, v);
        }
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parsea y valida (magic, versión, longitudes, crc).
    pub fn decode(buf: &[u8]) -> Res<Self> {
        let bad = |m: &'static str| ArcaError::InvalidFrame(m);
        if buf.len() < 8 + 2 + 2 + 4 + 8 + 32 + 4 {
            return Err(bad("launch spec: más corta que la cabecera mínima"));
        }
        if &buf[..8] != MAGIC {
            return Err(bad("launch spec: magic inválido"));
        }
        if u16::from_le_bytes([buf[8], buf[9]]) != VERSION {
            return Err(bad("launch spec: versión no soportada"));
        }
        let body = &buf[..buf.len() - 4];
        let crc = u32::from_le_bytes([
            buf[buf.len() - 4],
            buf[buf.len() - 3],
            buf[buf.len() - 2],
            buf[buf.len() - 1],
        ]);
        if crc32fast::hash(body) != crc {
            return Err(bad("launch spec: crc32 no coincide"));
        }
        let mut p = 12usize;
        let caps_bits = u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
        p += 4;
        let instance = u64::from_le_bytes([
            buf[p],
            buf[p + 1],
            buf[p + 2],
            buf[p + 3],
            buf[p + 4],
            buf[p + 5],
            buf[p + 6],
            buf[p + 7],
        ]);
        p += 8;
        let mut artifact = [0u8; 32];
        artifact.copy_from_slice(&buf[p..p + 32]);
        p += 32;
        let get_str = |p: &mut usize| -> Res<String> {
            if *p + 2 > body.len() {
                return Err(bad("launch spec: truncada en string"));
            }
            let n = u16::from_le_bytes([body[*p], body[*p + 1]]) as usize;
            *p += 2;
            if *p + n > body.len() {
                return Err(bad("launch spec: string desbordado"));
            }
            let s = std::str::from_utf8(&body[*p..*p + n])
                .map_err(|_| bad("launch spec: string no UTF-8"))?;
            *p += n;
            Ok(s.to_owned())
        };
        let app_path = get_str(&mut p)?;
        let app_dir = get_str(&mut p)?;
        let vault_dir = get_str(&mut p)?;
        let app_id = get_str(&mut p)?;
        // env_extra: u16 count + pares
        if p + 2 > body.len() {
            return Err(bad("launch spec: truncada en env_extra"));
        }
        let n_env = u16::from_le_bytes([body[p], body[p + 1]]) as usize;
        p += 2;
        if n_env > ENV_EXTRA_MAX {
            return Err(bad("launch spec: env_extra desbordado"));
        }
        let mut env_extra = Vec::with_capacity(n_env);
        for _ in 0..n_env {
            let k = get_str(&mut p)?;
            let v = get_str(&mut p)?;
            env_extra.push((k, v));
        }
        // El resto del cuerpo (salvo el crc) debe estar consumido exactamente:
        // bytes sobrantes = spec malformada (fail-closed).
        if p != body.len() {
            return Err(bad("launch spec: bytes sobrantes"));
        }
        let spec = Self {
            app_path,
            app_dir,
            vault_dir,
            app_id,
            instance,
            caps_bits,
            artifact,
            env_extra,
        };
        validar_env_extra(&spec.env_extra)?;
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ejemplo() -> LaunchSpec {
        LaunchSpec {
            app_path: "/apps/demo/bin/app".into(),
            app_dir: "/apps/demo".into(),
            vault_dir: "/vault/demo".into(),
            app_id: "dev.arca.demo".into(),
            instance: 7,
            caps_bits: 0b11,
            artifact: [0xAB; 32],
            env_extra: vec![("ARCA_PING_PANIC".into(), "1".into())],
        }
    }

    #[test]
    fn roundtrip() {
        let s = ejemplo();
        let b = s.encode();
        let back = LaunchSpec::decode(&b).expect("decode");
        assert_eq!(back, s);
        assert_eq!(back.artifact_hex().len(), 64);
    }

    #[test]
    fn corrupciones_rechazadas() {
        let b = ejemplo().encode();
        // magic
        let mut x = b.clone();
        x[0] = b'X';
        assert!(LaunchSpec::decode(&x).is_err());
        // crc (byte del medio)
        let mut x = b.clone();
        let n = x.len();
        x[n - 2] ^= 0xFF;
        assert!(LaunchSpec::decode(&x).is_err());
        // truncado
        assert!(LaunchSpec::decode(&b[..b.len() / 2]).is_err());
    }

    #[test]
    fn determinista() {
        let s = ejemplo();
        assert_eq!(s.encode(), s.encode());
    }

    #[test]
    fn version_1_rechazada() {
        // v1 (sin env_extra) ya no se acepta: host y launcher se buildan
        // juntos del mismo repo; mezclar versiones sería un bug silencioso.
        let mut b = ejemplo().encode();
        b[8] = 1;
        b[9] = 0;
        // re-crc del cuerpo mutado
        let body_len = b.len() - 4;
        let crc = crc32fast::hash(&b[..body_len]);
        b[body_len..].copy_from_slice(&crc.to_le_bytes());
        assert!(LaunchSpec::decode(&b).is_err());
    }

    #[test]
    fn env_extra_validado() {
        // clave sin prefijo ARCA_
        assert!(validar_env_extra(&[("PING".into(), "1".into())]).is_err());
        // clave reservada de identidad
        assert!(validar_env_extra(&[("ARCA_APP_ID".into(), "x".into())]).is_err());
        // con NUL
        assert!(validar_env_extra(&[("ARCA_X\u{0}Y".into(), "1".into())]).is_err());
        // demasiados pares
        let muchos: Vec<(String, String)> = (0..ENV_EXTRA_MAX + 1)
            .map(|i| (format!("ARCA_K{i}"), "v".into()))
            .collect();
        assert!(validar_env_extra(&muchos).is_err());
        // válido
        assert!(validar_env_extra(&[("ARCA_PING_PANIC".into(), "1".into())]).is_ok());
        // vacío
        assert!(validar_env_extra(&[]).is_ok());
    }

    #[test]
    fn bytes_sobrantes_rechazados() {
        let mut b = ejemplo().encode();
        // insertar 3 bytes basura antes del crc → cuerpo no consumido exacto
        let body_len = b.len() - 4;
        b.truncate(body_len);
        b.extend_from_slice(b"\0\0\0");
        let crc = crc32fast::hash(&b);
        b.extend_from_slice(&crc.to_le_bytes());
        assert!(LaunchSpec::decode(&b).is_err());
    }
}
