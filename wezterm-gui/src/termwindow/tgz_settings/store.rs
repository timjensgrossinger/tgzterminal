//! Reads and writes the user's `wezterm.lua` for the Settings tab.
//!
//! A change is only written once the edited text has been evaluated as a
//! config: a file that no longer loads never reaches disk. The first write
//! of a run keeps the file as it was beside it (`<name>.tgz-bak`).

use super::lua_edit::{self, EditError, LuaValue, PathStatus};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Why a change was not written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ApplyError {
    /// The file computes this setting on `line`.
    Locked {
        line: usize,
    },
    /// The file does not end in `return <name>` or `return { ... }`.
    Shape,
    /// The edited file did not evaluate; nothing was written.
    Invalid(String),
    Io(String),
}

impl ApplyError {
    pub(crate) fn message(&self, file: &Path) -> String {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "the config file".to_string());
        match self {
            ApplyError::Locked { line } => {
                format!("{name} sets this in code on line {line}. Change it there.")
            }
            ApplyError::Shape => format!(
                "{name} must end in `return config` or `return {{ ... }}` for Settings to edit it."
            ),
            ApplyError::Invalid(err) => format!("Not saved, the config would not load: {err}"),
            ApplyError::Io(err) => format!("Could not write {name}: {err}"),
        }
    }
}

impl From<EditError> for ApplyError {
    fn from(err: EditError) -> Self {
        match err {
            EditError::Locked { line } => ApplyError::Locked { line },
            EditError::UnsupportedShape => ApplyError::Shape,
        }
    }
}

/// The config file in use, or the one to create when there is none.
pub(crate) fn config_path() -> PathBuf {
    // Set by the loader to the file it adopted.
    if let Some(path) = std::env::var_os("WEZTERM_CONFIG_FILE") {
        return PathBuf::from(path);
    }
    // No file was adopted, but one may exist that failed to load: edit that
    // one rather than shadowing it with a new file.
    let mut candidates = vec![config::HOME_DIR.join(".wezterm.lua")];
    candidates.extend(config::CONFIG_DIRS.iter().map(|d| d.join("wezterm.lua")));
    if let Some(existing) = candidates.iter().find(|p| p.is_file()) {
        return existing.clone();
    }
    match config::portable_exe_dir().filter(|_| config::portable_mode()) {
        Some(dir) => dir.join("wezterm.lua"),
        None => config::CONFIG_DIRS
            .first()
            .map(|d| d.join("wezterm.lua"))
            .unwrap_or_else(|| config::HOME_DIR.join(".wezterm.lua")),
    }
}

/// The file's text, or a starter file's when it does not exist yet.
fn read(path: &Path) -> Result<(String, bool), ApplyError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok((text, true)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok((lua_edit::new_file(), false)),
        Err(err) => Err(ApplyError::Io(err.to_string())),
    }
}

/// Where each of `paths` stands in the config file. `None` when the file
/// cannot be edited at all (see [`ApplyError::Shape`]).
pub(crate) fn statuses(path: &Path, paths: &[&[&str]]) -> Option<Vec<PathStatus>> {
    let (text, _) = read(path).ok()?;
    paths
        .iter()
        .map(|p| lua_edit::status(&text, p).ok())
        .collect()
}

static BACKED_UP: AtomicBool = AtomicBool::new(false);

fn write(path: &Path, text: &str, existed: bool) -> std::io::Result<()> {
    // A dotfile is often a link into a repository: write what it points at.
    let target = if existed {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "wezterm.lua".to_string());

    if existed && !BACKED_UP.swap(true, Ordering::SeqCst) {
        if let Err(err) = std::fs::copy(&target, dir.join(format!("{name}.tgz-bak"))) {
            BACKED_UP.store(false, Ordering::SeqCst);
            return Err(err);
        }
    }

    let tmp = dir.join(format!(".{name}.tgz-tmp"));
    std::fs::write(&tmp, text)?;
    if existed {
        if let Ok(meta) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&tmp, meta.permissions());
        }
    }
    std::fs::rename(&tmp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Set `key` to `value` in the config file (`None`: forget a value the
/// Settings tab recorded), then reload the config.
pub(crate) fn apply(file: &Path, key: &[&str], value: Option<&LuaValue>) -> Result<(), ApplyError> {
    let (text, existed) = read(file)?;
    let edited = match value {
        Some(value) => lua_edit::set_value(&text, key, value)?,
        None => match lua_edit::unset_value(&text, key)? {
            Some(edited) => edited,
            None => return Ok(()),
        },
    };
    if edited == text && existed {
        return Ok(());
    }
    config::Config::check_lua_text(file, &edited)
        .map_err(|err| ApplyError::Invalid(format!("{err:#}")))?;
    write(file, &edited, existed).map_err(|err| ApplyError::Io(err.to_string()))?;
    config::reload();
    Ok(())
}
