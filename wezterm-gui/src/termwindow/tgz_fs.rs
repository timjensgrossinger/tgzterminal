//! Replacing a file that may hold something private (settings with tokens,
//! project paths, ssh hosts) without it ever being readable by others.
//!
//! The new contents go to a temporary file beside the target, created with
//! its final mode, then renamed over it. Writing first and narrowing the
//! mode afterwards left a window, or for good when the narrowing failed,
//! where the contents had the umask's default mode, usually 0644.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The temporary `path` is written to before it is renamed over the target.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.tgz-tmp"))
}

/// The mode a replacement keeps: the existing file's, else `default`.
pub(crate) fn existing_mode_or(path: &Path, default: u32) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            return meta.permissions().mode() & 0o777;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    default
}

/// Write `bytes` to a fresh file at `path`, created with `mode` (Unix only;
/// the umask can only take bits away). Fails if `path` exists.
fn create_with_mode(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Replace `path` with `bytes`, as a file of `mode`. The file is either the
/// old one or the whole new one, and never readable beyond `mode`. Creates
/// the parent directory when missing.
pub(crate) fn replace_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    // Left by an earlier write that did not finish; its mode is unknown.
    match std::fs::remove_file(&temp) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let result = create_with_mode(&temp, bytes, mode).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_file_has_the_new_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/settings.json");
        replace_file(&path, b"one", 0o600).unwrap();
        replace_file(&path, b"two", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn a_stale_temp_file_does_not_stop_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(temp_path(&path), "half").unwrap();
        replace_file(&path, b"whole", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"whole");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_created_with_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");
        replace_file(&path, b"secret", 0o600).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(existing_mode_or(&path, 0o644), 0o600);
        assert_eq!(existing_mode_or(&dir.path().join("missing"), 0o640), 0o640);
    }
}
