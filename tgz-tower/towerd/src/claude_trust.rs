//! Pre-trusting the per-flight folders Tower creates, for Claude Code.
//!
//! Claude Code asks "do you trust this folder?" the first time it runs in a
//! project, and remembers the answer per project path in `~/.claude.json`
//! (`projects[<path>].hasTrustDialogAccepted`). The project is the git
//! repository a folder belongs to, so a trusted parent does not cover a
//! flight folder that is a repository of its own: each one is trusted by
//! its exact path.
//!
//! Tower only ever adds trust for folders it created itself under its
//! flights root, never for your projects. The file is Claude's: the edit is
//! minimal (one key), written atomically, and the original is backed up once.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`.
pub fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(".claude.json"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude.json"))
}

/// Is exactly `dir` marked trusted in this config?
pub fn is_trusted(config: &Value, dir: &Path) -> bool {
    config
        .get("projects")
        .and_then(Value::as_object)
        .and_then(|p| p.get(&dir.display().to_string()))
        .and_then(|p| p.get("hasTrustDialogAccepted"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Mark `dir` trusted, keeping everything else (and any existing entry).
pub fn add_trust(config: &mut Value, dir: &Path) {
    if !config.is_object() {
        *config = json!({});
    }
    let obj = config.as_object_mut().expect("object");
    let projects = obj.entry("projects").or_insert_with(|| json!({}));
    if !projects.is_object() {
        *projects = json!({});
    }
    let entry = projects
        .as_object_mut()
        .expect("object")
        .entry(dir.display().to_string())
        .or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    entry
        .as_object_mut()
        .expect("object")
        .insert("hasTrustDialogAccepted".into(), Value::Bool(true));
}

/// Trust `dir` in Claude's config unless it already is.
/// Returns whether the file changed.
pub fn ensure_trusted(dir: &Path) -> std::io::Result<bool> {
    let Some(path) = config_path() else {
        return Ok(false);
    };
    // Claude stores the canonical path (macOS: /private/var/... etc.).
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        // No Claude config yet: Claude will ask once, as usual.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut config: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        // Never rewrite a file we cannot fully understand.
        Err(_) => return Ok(false),
    };
    if is_trusted(&config, &dir) {
        return Ok(false);
    }
    let backup = path.with_extension("json.tower-backup");
    if !backup.exists() {
        std::fs::write(&backup, &text)?;
        // The config can hold MCP server credentials: same access as the original.
        if let Ok(meta) = std::fs::metadata(&path) {
            let _ = std::fs::set_permissions(&backup, meta.permissions());
        }
    }
    add_trust(&mut config, &dir);
    let tmp = path.with_extension("json.tower-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&config)?)?;
    if let Ok(meta) = std::fs::metadata(&path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_per_exact_project_path() {
        let c = json!({"projects": {"/home/me/Tower/TWR-1": {"hasTrustDialogAccepted": true}, "/x": {"hasTrustDialogAccepted": false}}});
        assert!(is_trusted(&c, Path::new("/home/me/Tower/TWR-1")));
        // A flight folder is its own git repo: a trusted parent does not count.
        assert!(!is_trusted(&c, Path::new("/home/me/Tower/TWR-1/sub")));
        assert!(!is_trusted(&c, Path::new("/x")));
        assert!(!is_trusted(&json!({}), Path::new("/a")));
    }

    #[test]
    fn adding_trust_keeps_everything_else() {
        let mut c = json!({"numStartups": 3, "projects": {"/a": {"allowedTools": ["Bash"]}}});
        add_trust(&mut c, Path::new("/a"));
        add_trust(&mut c, Path::new("/b"));
        assert_eq!(c["numStartups"], 3);
        assert_eq!(c["projects"]["/a"]["allowedTools"], json!(["Bash"]));
        assert_eq!(c["projects"]["/a"]["hasTrustDialogAccepted"], true);
        assert_eq!(c["projects"]["/b"]["hasTrustDialogAccepted"], true);
        let mut odd = json!([1, 2]);
        add_trust(&mut odd, Path::new("/c"));
        assert_eq!(odd["projects"]["/c"]["hasTrustDialogAccepted"], true);
    }
}
