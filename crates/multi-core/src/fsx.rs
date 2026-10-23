//! Small filesystem helpers that attach the offending path to I/O errors.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| Error::io(path, e))
}

/// Read a file, returning `None` when it does not exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent)?;
    }
    fs::write(path, contents).map_err(|e| Error::io(path, e))
}

pub fn create_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| Error::io(path, e))
}

pub fn remove_file(path: &Path) -> Result<()> {
    fs::remove_file(path).map_err(|e| Error::io(path, e))
}

pub fn remove_dir_all(path: &Path) -> Result<()> {
    fs::remove_dir_all(path).map_err(|e| Error::io(path, e))
}

/// Remove a path whatever it is: symlink, file or directory.
pub fn remove_path(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() {
        remove_dir_all(path)
    } else {
        remove_file(path)
    }
}

/// `true` when the path exists as a symlink, even a dangling one.
pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Like `Path::canonicalize`, but falls back to the input when the path does
/// not exist (mirrors Python's non-strict `Path.resolve()`).
pub fn resolve(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    // Resolve the longest existing prefix and re-append the rest.
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
    let mut resolved = fs::canonicalize(&existing).unwrap_or(existing);
    for part in tail.iter().rev() {
        resolved.push(part);
    }
    resolved
}

/// `true` when `child` is `parent` or located underneath it (after resolving
/// symlinks).
pub fn is_inside(child: &Path, parent: &Path) -> bool {
    resolve(child).starts_with(resolve(parent))
}

pub fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(source).map_err(|e| Error::io(source, e))?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(|e| Error::io(source, e))?;
        symlink(&target, destination)?;
        return Ok(());
    }
    if meta.is_dir() {
        create_dir_all(destination)?;
        for entry in fs::read_dir(source).map_err(|e| Error::io(source, e))? {
            let entry = entry.map_err(|e| Error::io(source, e))?;
            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = destination.parent() {
            create_dir_all(parent)?;
        }
        fs::copy(source, destination).map_err(|e| Error::io(destination, e))?;
        Ok(())
    }
}

/// Move a directory into place, falling back to copy+delete when the rename
/// crosses filesystems.
pub fn move_path(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        create_dir_all(parent)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_recursively(source, destination)?;
            remove_path(source)
        }
    }
}

#[cfg(unix)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| Error::io(link, e))
}

#[cfg(windows)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link).map_err(|e| Error::io(link, e))
    } else {
        std::os::windows::fs::symlink_file(target, link).map_err(|e| Error::io(link, e))
    }
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Sorted list of entries directly inside `dir` (empty when it is missing).
pub fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(read) => read
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::io(dir, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(Error::io(dir, e)),
    };
    entries.sort();
    Ok(entries)
}

/// Directory entries whose file name ends with one of `extensions`, sorted by
/// name.
pub fn files_with_extensions(dir: &Path, extensions: &[&str]) -> Result<Vec<PathBuf>> {
    Ok(sorted_entries(dir)?
        .into_iter()
        .filter(|p| p.is_file())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| extensions.contains(&e))
                .unwrap_or(false)
        })
        .collect())
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

static MULTI_HOME_OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Redirect the per-user state directory (registry, recent users) for the
/// rest of the process. Only the first call takes effect; used by tests and
/// embedding apps.
pub fn set_multi_home_dir(path: PathBuf) {
    let _ = MULTI_HOME_OVERRIDE.set(path);
}

/// The per-user multi state directory: the programmatic override, else
/// `$MULTI_HOME`, else `~/.multi`.
pub fn multi_home_dir() -> PathBuf {
    if let Some(path) = MULTI_HOME_OVERRIDE.get() {
        return path.clone();
    }
    if let Some(path) = std::env::var_os("MULTI_HOME").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".multi")
}
