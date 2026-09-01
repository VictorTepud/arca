//! Sinks de extracción: dónde y cómo se escriben los bytes.
//!
//! El trait [`EntrySink`] es el contrato dinámico que recibe
//! [`crate::Archive::extract`]; [`DirSink`] es la implementación de
//! filesystem **sandboxeada** que usa el installer:
//!
//! - escribe SOLO dentro de `root` (nunca sale: los paths llegan ya
//!   saneados como [`RelPath`]);
//! - directorios con permisos `0700`, archivos `0600` (spec 09 §4);
//! - escribe primero a `<destino>.arca-tmp` y renombra al finalizar, para
//!   que una interrupción no deje archivos parciales con nombre final
//!   (spec 09 §6: "no quedan archivos parciales sin .tmp");
//! - copia **streaming con buffer fijo de 1 MiB** (invariante de memoria
//!   O(1) por archivo, spec 09 §4).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use arca_types::{ArcaError, Res};

use crate::path::RelPath;

/// Tamaño del buffer fijo de copia (spec 09 §4: "buffers fijos 1 MB").
pub const COPY_BUF_BYTES: usize = 1024 * 1024;

/// Sufijo temporal de los archivos en curso.
pub const TMP_SUFFIX: &str = ".arca-tmp";

/// Destino de los bytes extraídos (contrato `dyn` de `Archive::extract`).
///
/// NOTA(agent): la spec 09 dibuja `pub struct EntrySink` Y a la vez
/// `extract(sink: &mut dyn EntrySink)`; un `struct` no puede ser `dyn`, así
/// que el **contrato** es este trait (la firma de `extract` queda literal a
/// la spec) y la implementación de filesystem se llama [`DirSink`].
pub trait EntrySink {
    /// Crea un directorio (los padres necesarios también) bajo la raíz.
    fn mkdir(&mut self, rel: &RelPath) -> Res<()>;

    /// Escribe el contenido de una entrada de forma **streaming** y devuelve
    /// los bytes escritos.
    ///
    /// El implementador ES responsable de consumir `data` hasta EOF: en un
    /// bloque sólido 7z hay que leer todos los bytes para poder avanzar a la
    /// siguiente entrada (y para que se verifique su CRC).
    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn Read) -> Res<u64>;

    /// Directorio raíz bajo el que escribe este sink (diagnóstico).
    fn root(&self) -> &Path;
}

/// Modo de un path/permiso en Unix (0700 dirs, 0600 archivos).
#[cfg(unix)]
fn unix_mode_dir() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(0o700)
}

/// Sink de filesystem: escribe SOLO bajo `root`, con permisos restringidos
/// y atomicidad por archivo (`.arca-tmp` + rename).
///
/// El `root` se crea perezosamente en el primer uso (así `new` es infalible
/// como pide la spec) con permisos `0700`.
#[derive(Debug)]
pub struct DirSink {
    root: PathBuf,
    /// Buffer fijo reutilizado en todas las copias (memoria O(1)).
    buf: Vec<u8>,
}

impl DirSink {
    /// Crea el sink sobre `root` (no crea el directorio todavía).
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            buf: vec![0u8; COPY_BUF_BYTES],
        }
    }

    /// Ruta final de una entrada saneada.
    fn final_path(&self, rel: &RelPath) -> PathBuf {
        self.root.join(rel.as_path())
    }

    /// Crea (si faltan) `root` y todos los directorios de `rel` con 0700.
    fn mkdir_all_rel(&self, rel: Option<&RelPath>) -> Res<()> {
        if !self.root.exists() {
            fs::create_dir(&self.root).map_err(ArcaError::Io)?;
            #[cfg(unix)]
            {
                let _ = fs::set_permissions(&self.root, unix_mode_dir());
            }
        }
        if let Some(rel) = rel {
            // Crear componente a componente para fijar 0700 en cada nivel.
            let mut cur = self.root.clone();
            for comp in rel.as_path().components() {
                cur.push(comp);
                if !cur.exists() {
                    let created = fs::create_dir(&cur);
                    match created {
                        Ok(()) => {
                            #[cfg(unix)]
                            {
                                let _ = fs::set_permissions(&cur, unix_mode_dir());
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(ArcaError::Io(e)),
                    }
                }
            }
        }
        Ok(())
    }
}

impl EntrySink for DirSink {
    fn mkdir(&mut self, rel: &RelPath) -> Res<()> {
        self.mkdir_all_rel(Some(rel))
    }

    fn write_entry(&mut self, rel: &RelPath, data: &mut dyn Read) -> Res<u64> {
        let final_path = self.final_path(rel);
        // Última línea contra duplicados (el pre-escaneo de `Archive` ya lo
        // detecta): un paquete no puede reescribir un archivo ya extraído.
        if final_path.exists() {
            return Err(ArcaError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "entrada duplicada al extraer",
            )));
        }
        // Padres con 0700 (incluye el root si es la primera escritura).
        let parent_rel = rel
            .as_str()
            .rsplit_once('/')
            .and_then(|(dir, _)| crate::sanitize_entry_path(dir));
        self.mkdir_all_rel(parent_rel.as_ref())?;

        let tmp_path = {
            let mut p = final_path.clone().into_os_string();
            p.push(TMP_SUFFIX);
            PathBuf::from(p)
        };

        // create_new (O_CREAT|O_EXCL): no sigue symlinks preexistentes y
        // además detecta extracciones duplicadas.
        let mut file = {
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&tmp_path).map_err(ArcaError::Io)?
        };

        let mut copy = || -> Res<u64> {
            let mut written: u64 = 0;
            loop {
                let n = data.read(&mut self.buf).map_err(ArcaError::Io)?;
                if n == 0 {
                    break;
                }
                file.write_all(&self.buf[..n]).map_err(ArcaError::Io)?;
                written += n as u64;
            }
            file.sync_data().map_err(ArcaError::Io)?;
            Ok(written)
        };
        match copy() {
            Ok(n) => {
                // Atómico en el mismo filesystem: renombrar el .tmp al final.
                fs::rename(&tmp_path, &final_path).map_err(ArcaError::Io)?;
                Ok(n)
            }
            Err(e) => {
                // Error a mitad de copia: no dejar parciales sueltos.
                let _ = fs::remove_file(&tmp_path);
                Err(e)
            }
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize_entry_path;

    fn rel(s: &str) -> RelPath {
        sanitize_entry_path(s).unwrap()
    }

    #[test]
    fn escribe_archivo_con_permisos_y_contenido() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = DirSink::new(tmp.path().join("root"));
        let mut data = b"contenido de prueba!".as_slice();
        let n = sink.write_entry(&rel("a/b/c.txt"), &mut data).unwrap();
        assert_eq!(n, 20);

        let out = tmp.path().join("root/a/b/c.txt");
        assert_eq!(std::fs::read(&out).unwrap(), b"contenido de prueba!");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&out).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(tmp.path().join("root/a"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        // Sin .tmp residuales.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join("root"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(TMP_SUFFIX))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn error_de_lectura_no_deja_tmp_ni_final() {
        /// Reader que falla a mitad de stream.
        struct FlakyReader {
            sent: usize,
        }
        impl Read for FlakyReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.sent == 0 {
                    self.sent = 1;
                    buf[..3].copy_from_slice(b"abc");
                    Ok(3)
                } else {
                    Err(std::io::Error::other("fallo inyectado"))
                }
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = DirSink::new(tmp.path().join("root"));
        let mut flaky = FlakyReader { sent: 0 };
        let err = sink.write_entry(&rel("x.bin"), &mut flaky).unwrap_err();
        assert!(matches!(err, ArcaError::Io(_)));
        assert!(!tmp.path().join("root/x.bin").exists());
        assert!(!tmp.path().join("root/x.bin.arca-tmp").exists());
    }

    #[test]
    fn archivo_duplicado_se_rechaza() {
        // create_new: la segunda escritura del mismo path final debe fallar
        // (los paquetes no pueden tener entradas duplicadas; la capa Archive
        // lo detecta antes, aquí es la última línea).
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = DirSink::new(tmp.path().join("root"));
        let mut d1 = b"uno".as_slice();
        sink.write_entry(&rel("dup.txt"), &mut d1).unwrap();
        let mut d2 = b"dos".as_slice();
        let err = sink.write_entry(&rel("dup.txt"), &mut d2).unwrap_err();
        assert!(matches!(err, ArcaError::Io(_)));
        // El contenido del primero queda intacto.
        assert_eq!(
            std::fs::read(tmp.path().join("root/dup.txt")).unwrap(),
            b"uno"
        );
    }

    #[test]
    fn mkdir_crea_jerarquia_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = DirSink::new(tmp.path().join("root"));
        sink.mkdir(&rel("assets/fonts")).unwrap();
        assert!(tmp.path().join("root/assets/fonts").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(tmp.path().join("root/assets/fonts"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o700);
        }
    }
}
