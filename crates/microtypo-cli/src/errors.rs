use std::io;
use std::path::Path;

use microtypo_core::{Error, ErrorKind};

#[derive(Debug)]
pub enum CliError {
    Usage(String),
    Engine(Error),
    Other(String),
}

impl CliError {
    pub fn message(&self) -> &str {
        match self {
            CliError::Usage(m) | CliError::Other(m) => m,
            CliError::Engine(e) => &e.message,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Usage(_) => 2,
            CliError::Engine(e) if e.kind == ErrorKind::Config => 2,
            CliError::Engine(_) | CliError::Other(_) => 1,
        }
    }
}

impl From<Error> for CliError {
    fn from(e: Error) -> Self {
        CliError::Engine(e)
    }
}

pub type Result<T> = std::result::Result<T, CliError>;

pub fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

fn errno(e: &io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(target_os = "linux")]
    let (eloop, enametoolong) = (40, 36);
    #[cfg(not(target_os = "linux"))]
    let (eloop, enametoolong) = (62, 63);
    Some(match e.raw_os_error()? {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        5 => ("EIO", "i/o error"),
        13 => ("EACCES", "permission denied"),
        16 => ("EBUSY", "resource busy or locked"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        22 => ("EINVAL", "invalid argument"),
        24 => ("EMFILE", "too many open files"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        n if n == eloop => ("ELOOP", "too many symbolic links encountered"),
        n if n == enametoolong => ("ENAMETOOLONG", "name too long"),
        _ => return None,
    })
}

pub fn is_missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

pub fn if_present<T>(result: io::Result<T>, error: impl FnOnce(&io::Error) -> CliError) -> Result<Option<T>> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(e) if is_missing(&e) => Ok(None),
        Err(e) => Err(error(&e)),
    }
}

pub fn system(e: &io::Error, syscall: &str, path: Option<&Path>, dest: Option<&Path>) -> CliError {
    let Some((code, description)) = errno(e) else {
        return CliError::Other(e.to_string());
    };
    let path = path.map_or_default(|p| format!(" '{}'", p.display()));
    let dest = dest.map_or_default(|d| format!(" -> '{}'", d.display()));
    CliError::Other(format!("{code}: {description}, {syscall}{path}{dest}"))
}
