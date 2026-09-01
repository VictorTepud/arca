//! Validación de paths de socket: filesystem, jamás abstract namespace.

use std::path::Path;

use arca_types::{ArcaError, Res};

/// Rechaza paths vacíos, con NUL interior (abstract namespace de Linux:
/// `sun_path[0] == 0`) o con prefijo `@` (convención abstract de algunas
/// libs). El abstract namespace NO tiene permisos — prohibido (docs/01 §4).
pub fn ensure_filesystem_path(p: &Path) -> Res<()> {
    let bytes = p
        .as_os_str()
        .to_str()
        .map(str::as_bytes)
        .ok_or(ArcaError::Internal("ipc: path de socket no es UTF-8"))?;
    if bytes.is_empty() {
        return Err(ArcaError::Internal("ipc: path de socket vacío"));
    }
    if bytes.contains(&0u8) {
        // Abstract namespace: sun_path empieza (o contiene) NUL.
        return Err(ArcaError::Internal(
            "ipc: path con NUL (abstract namespace PROHIBIDO: docs/01 §4)",
        ));
    }
    if bytes.first() == Some(&b'@') {
        return Err(ArcaError::Internal(
            "ipc: prefijo @ = abstract namespace PROHIBIDO (docs/01 §4)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn acepta_paths_normales() {
        assert!(ensure_filesystem_path(&p("/tmp/arca/runtime/inst-1/app.sock")).is_ok());
        assert!(ensure_filesystem_path(&p("relativo.sock")).is_ok());
    }

    #[test]
    fn rechaza_abstract() {
        // NUL interior: OsStr desde bytes crudos (válido en Linux).
        use std::os::unix::ffi::OsStrExt as _;
        let abstracto = std::ffi::OsStr::from_bytes(b"\0arca-secret");
        assert!(ensure_filesystem_path(Path::new(abstracto)).is_err());
        assert!(ensure_filesystem_path(&p("@arca")).is_err());
        assert!(ensure_filesystem_path(&p("")).is_err());
    }
}
