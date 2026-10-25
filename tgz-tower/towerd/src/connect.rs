//! "Connect agents": Tower's MCP server in the user-wide config of Claude
//! Code (`~/.claude.json` `mcpServers`) and Codex (`~/.codex/config.toml`
//! `[mcp_servers.tower]`), so a CLI you start in any TGZTerminal pane can
//! hand subtasks to local models.
//!
//! Outside TGZTerminal the server lists no tools, so the entry is silent
//! there. Only the one `tower` entry is touched: never a server of yours
//! with that name, never anything else in the file. Each file is backed up
//! once before Tower first changes it and written atomically.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `~/.codex/config.toml`, or `$CODEX_HOME/config.toml`.
pub fn codex_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(dir).join("config.toml"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("config.toml"))
}

/// Is this MCP server entry one Tower wrote (`<tower exe> mcp`)?
fn is_ours(command: Option<&str>, args: &[String]) -> bool {
    let named_tower = command
        .and_then(|c| Path::new(c).file_stem())
        .map(|s| s == "tower")
        .unwrap_or(false);
    named_tower && args == ["mcp"]
}

fn json_args(v: &Value) -> Vec<String> {
    v.get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Claude: is Tower's entry there?
pub fn claude_has(config: &Value) -> bool {
    config
        .pointer("/mcpServers/tower")
        .map(|t| is_ours(t.get("command").and_then(Value::as_str), &json_args(t)))
        .unwrap_or(false)
}

/// Claude: add (`exe` = Some) or remove Tower's entry. Refuses to replace a
/// `tower` server Tower did not write. Returns whether anything changed.
pub fn claude_set(config: &mut Value, exe: Option<&str>) -> Result<bool, String> {
    if let Some(t) = config.pointer("/mcpServers/tower") {
        if !is_ours(t.get("command").and_then(Value::as_str), &json_args(t)) {
            return Err(
                "~/.claude.json already has an MCP server named \"tower\" that Tower did not add"
                    .into(),
            );
        }
    }
    match exe {
        Some(exe) => {
            let want = json!({"type": "stdio", "command": exe, "args": ["mcp"], "env": {}});
            if config.pointer("/mcpServers/tower") == Some(&want) {
                return Ok(false);
            }
            if !config.is_object() {
                return Err("~/.claude.json is not a JSON object".into());
            }
            let obj = config.as_object_mut().expect("object");
            let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
            let servers = servers
                .as_object_mut()
                .ok_or("~/.claude.json: mcpServers is not an object")?;
            servers.insert("tower".into(), want);
            Ok(true)
        }
        None => Ok(config
            .get_mut("mcpServers")
            .and_then(Value::as_object_mut)
            .map(|s| s.remove("tower").is_some())
            .unwrap_or(false)),
    }
}

/// `~/.claude/settings.json`, or `$CLAUDE_CONFIG_DIR/settings.json`.
pub fn claude_settings_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("settings.json"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude").join("settings.json"))
}

/// The guard hooks: (event, matcher, `tower guard` argument).
const GUARD_HOOKS: [(&str, Option<&str>, &str); 2] = [
    ("UserPromptSubmit", None, "prompt"),
    (
        "PreToolUse",
        Some("Read|Edit|Write|MultiEdit|NotebookEdit|Bash|Grep|Glob|mcp__.*"),
        "tool",
    ),
];

/// What a guard hook prints when it cannot run at all. It then exits 2,
/// which blocks: a guard that is gone must not let everything through.
const GUARD_BROKEN: &str = "Tower's local-only guard could not run. Open Tower and click Connect agents again, or Disconnect to remove the guard.";

/// The hook command: the guard, or block with a message when it fails.
fn guard_command(exe: &str, arg: &str) -> String {
    format!(
        "'{}' guard {arg} || {{ echo \"{GUARD_BROKEN}\" >&2; exit 2; }}",
        exe.replace('\'', "'\\''")
    )
}

/// Is this hook command one Tower wrote (`'<…/tower>' guard prompt|tool…`)?
fn is_guard_command(cmd: &str) -> bool {
    let cmd = cmd.trim();
    let Some(rest) = cmd.strip_prefix('\'') else {
        return false;
    };
    let Some(end) = rest.find("' guard ") else {
        return false;
    };
    let prog = &rest[..end];
    let after = &rest[end + "' guard ".len()..];
    (after.starts_with("prompt") || after.starts_with("tool"))
        && Path::new(prog)
            .file_stem()
            .map(|s| s == "tower")
            .unwrap_or(false)
}

/// Where "Connect agents" keeps its own copy of the `tower` binary: what
/// the hooks and MCP entries run, so a rebuild or `cargo clean` of the
/// source tree never leaves them pointing at nothing.
pub fn stable_exe(data_dir: &Path) -> PathBuf {
    data_dir
        .join("bin")
        .join(format!("tower{}", std::env::consts::EXE_SUFFIX))
}

/// Copy `exe` to `stable_exe`, atomically. Returns the copy's path.
pub fn install_exe(exe: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let dest = stable_exe(data_dir);
    if exe == dest {
        return Ok(dest);
    }
    let dir = dest.parent().ok_or("no folder for the tower binary")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".tower-{}", crate::paths::random_u64()));
    std::fs::copy(exe, &tmp).map_err(|e| format!("copying {}: {e}", exe.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(dest)
}

/// Claude: add (`exe` = Some) or remove Tower's guard hooks in
/// `settings.json`, leaving every other hook alone. Returns whether
/// anything changed.
pub fn claude_hooks_set(settings: &mut Value, exe: Option<&str>) -> Result<bool, String> {
    if !settings.is_object() {
        return Err("~/.claude/settings.json is not a JSON object".into());
    }
    let before = settings.clone();
    let obj = settings.as_object_mut().expect("object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or("~/.claude/settings.json: hooks is not an object")?;
    for (event, matcher, arg) in GUARD_HOOKS {
        let list = hooks.entry(event).or_insert_with(|| json!([]));
        let list = list
            .as_array_mut()
            .ok_or_else(|| format!("~/.claude/settings.json: hooks.{event} is not a list"))?;
        // Drop Tower's own entries (and Tower's commands inside shared ones).
        for group in list.iter_mut() {
            if let Some(inner) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                inner.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .map(is_guard_command)
                        .unwrap_or(false)
                });
            }
        }
        list.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(true)
        });
        if let Some(exe) = exe {
            let mut group = json!({"hooks": [{
                "type": "command",
                "command": guard_command(exe, arg),
                "timeout": 10
            }]});
            if let Some(m) = matcher {
                group["matcher"] = json!(m);
            }
            list.push(group);
        }
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
    Ok(*settings != before)
}

/// Claude: are Tower's guard hooks there?
pub fn claude_hooks_present(settings: &Value) -> bool {
    settings
        .pointer("/hooks/UserPromptSubmit")
        .and_then(Value::as_array)
        .map(|groups| {
            groups.iter().any(|g| {
                g.get("hooks").and_then(Value::as_array).is_some_and(|hs| {
                    hs.iter().any(|h| {
                        h.get("command")
                            .and_then(Value::as_str)
                            .is_some_and(is_guard_command)
                    })
                })
            })
        })
        .unwrap_or(false)
}

/// Codex: is Tower's entry there?
pub fn codex_has(doc: &toml_edit::DocumentMut) -> bool {
    doc.get("mcp_servers")
        .and_then(|m| m.get("tower"))
        .map(|t| {
            let args: Vec<String> = t
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            is_ours(t.get("command").and_then(|c| c.as_str()), &args)
        })
        .unwrap_or(false)
}

/// Codex: add or remove Tower's `[mcp_servers.tower]`. Codex hands an MCP
/// server only a few environment variables, so `WEZTERM_PANE` is named.
pub fn codex_set(doc: &mut toml_edit::DocumentMut, exe: Option<&str>) -> Result<bool, String> {
    use toml_edit::{value, Array, Item, Table};
    let exists = doc
        .get("mcp_servers")
        .and_then(|m| m.get("tower"))
        .is_some();
    if exists && !codex_has(doc) {
        return Err(
            "~/.codex/config.toml already has an MCP server named \"tower\" that Tower did not add"
                .into(),
        );
    }
    match exe {
        Some(exe) => {
            if !doc.contains_key("mcp_servers") {
                let mut t = Table::new();
                t.set_implicit(true);
                doc.insert("mcp_servers", Item::Table(t));
            }
            let servers = doc["mcp_servers"]
                .as_table_mut()
                .ok_or("~/.codex/config.toml: mcp_servers is not a table")?;
            let before = servers.get("tower").map(|t| t.to_string());
            let mut t = Table::new();
            t["command"] = value(exe);
            t["args"] = value(Array::from_iter(["mcp"]));
            t["env_vars"] = value(Array::from_iter(["WEZTERM_PANE"]));
            let after = Item::Table(t);
            if before.as_deref() == Some(after.to_string().as_str()) {
                return Ok(false);
            }
            servers.insert("tower", after);
            Ok(true)
        }
        None => Ok(doc
            .get_mut("mcp_servers")
            .and_then(|m| m.as_table_mut())
            .map(|s| s.remove("tower").is_some())
            .unwrap_or(false)),
    }
}

/// Keep a copy of `path` from before Tower first changed it.
fn backup_once(path: &Path, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let backup = path.with_file_name(format!("{name}.tower-backup"));
    if backup.exists() {
        return Ok(());
    }
    // These files can hold MCP server credentials: owner-only from the start.
    write_private(&backup, text)
}

/// Create `path` readable by its owner only (never briefly wider).
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::Write::write_all(&mut f, text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Write `text` over `path` through a temp file with the original's
/// permissions.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.tower-tmp"));
    write_private(&tmp, text)?;
    // Then the original's own permissions, if it allowed more.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Connect (`exe` = Some) or disconnect one CLI's config file. A missing
/// Claude config is left alone (Claude writes its own on first start); a
/// missing Codex config is created. Returns whether the file changed.
pub fn apply_claude(path: &Path, exe: Option<&str>) -> Result<bool, String> {
    // A symlinked config (stow, chezmoi) is written through, not replaced.
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = real.as_path();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut config: Value = serde_json::from_str(&text).map_err(|e| {
        format!(
            "{} is not valid JSON ({e}); not touching it",
            path.display()
        )
    })?;
    if !claude_set(&mut config, exe)? {
        return Ok(false);
    }
    backup_once(path, &text)?;
    let out = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    replace(path, &out)?;
    Ok(true)
}

/// Claude's guard hooks in `settings.json`; created when missing (with
/// its folder) on connect, left alone when missing on disconnect.
pub fn apply_claude_hooks(path: &Path, exe: Option<&str>) -> Result<bool, String> {
    // A symlinked config (stow, chezmoi) is written through, not replaced.
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = real.as_path();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && exe.is_some() => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut settings: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); not touching it",
                path.display()
            )
        })?
    };
    if !claude_hooks_set(&mut settings, exe)? {
        return Ok(false);
    }
    let out = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    if text.is_empty() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        write_private(path, &out)?;
        return Ok(true);
    }
    backup_once(path, &text)?;
    replace(path, &out)?;
    Ok(true)
}

pub fn apply_codex(path: &Path, exe: Option<&str>) -> Result<bool, String> {
    // A symlinked config (stow, chezmoi) is written through, not replaced.
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path = real.as_path();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && exe.is_some() => String::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{} does not parse ({e}); not touching it", path.display()))?;
    if !codex_set(&mut doc, exe)? {
        return Ok(false);
    }
    if text.is_empty() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(path, doc.to_string()).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(true);
    }
    backup_once(path, &text)?;
    replace(path, &doc.to_string())?;
    Ok(true)
}

/// Which CLIs have Tower's entry now.
pub fn status() -> Vec<String> {
    let mut out = vec![];
    let claude = crate::claude_trust::config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|c| claude_has(&c))
        .unwrap_or(false);
    // Connected means the guard hooks too: an older connect without them
    // shows as not connected, so connecting again adds them.
    let guarded = claude_settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|s| claude_hooks_present(&s))
        .unwrap_or(false);
    if claude && guarded {
        out.push("claude".to_string());
    }
    let codex = codex_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .map(|d| codex_has(&d))
        .unwrap_or(false);
    if codex {
        out.push("codex".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/Applications/TGZTerminal.app/Contents/MacOS/tower";

    #[test]
    fn claude_entry_is_added_kept_and_removed_alone() {
        let mut c = json!({"numStartups": 3, "mcpServers": {"github": {"command": "gh-mcp"}}});
        assert!(claude_set(&mut c, Some(EXE)).unwrap());
        assert!(claude_has(&c));
        assert!(!claude_set(&mut c, Some(EXE)).unwrap(), "already there");
        assert_eq!(c["mcpServers"]["github"]["command"], "gh-mcp");
        assert_eq!(c["numStartups"], 3);
        assert!(claude_set(&mut c, None).unwrap());
        assert!(!claude_has(&c));
        assert!(c["mcpServers"]["github"].is_object(), "yours stay");
        assert!(!claude_set(&mut c, None).unwrap());
    }

    #[test]
    fn guard_hooks_are_added_once_and_removed_without_touching_yours() {
        let mut s = json!({"model": "opus", "hooks": {"PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": "~/bin/my-check.sh"}]}
        ]}});
        assert!(claude_hooks_set(&mut s, Some(EXE)).unwrap());
        assert!(claude_hooks_present(&s));
        assert!(
            !claude_hooks_set(&mut s, Some(EXE)).unwrap(),
            "already there"
        );
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            s["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
            guard_command(EXE, "prompt")
        );
        assert!(claude_hooks_set(&mut s, None).unwrap());
        assert!(!claude_hooks_present(&s));
        assert_eq!(
            s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "~/bin/my-check.sh"
        );
        assert!(s["hooks"].get("UserPromptSubmit").is_none());
        assert_eq!(s["model"], "opus");
        assert!(is_guard_command(&guard_command("/a/b c/tower", "tool")));
        assert!(
            is_guard_command("'/a/b c/tower' guard tool"),
            "older entries too"
        );
        assert!(!is_guard_command("'/a/guard' guard tool"));
        assert!(!is_guard_command("tower-cli guard prompt"));
        assert!(guard_command("/x/tower", "prompt").ends_with("exit 2; }"));
    }

    #[test]
    fn a_tower_server_you_wrote_is_never_replaced() {
        let mut c = json!({"mcpServers": {"tower": {"command": "/opt/other/tower-cli", "args": ["serve"]}}});
        assert!(claude_set(&mut c, Some(EXE)).is_err());
        assert!(claude_set(&mut c, None).is_err());
        let mut d: toml_edit::DocumentMut =
            "[mcp_servers.tower]\ncommand = \"x\"\n".parse().unwrap();
        assert!(codex_set(&mut d, Some(EXE)).is_err());
    }

    #[test]
    fn codex_entry_names_the_pane_variable_and_keeps_comments() {
        let mut d: toml_edit::DocumentMut =
            "# mine\nmodel = \"gpt-5\"\n\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n"
                .parse()
                .unwrap();
        assert!(codex_set(&mut d, Some(EXE)).unwrap());
        let out = d.to_string();
        assert!(out.contains("# mine"));
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["mcp_servers"]["tower"]["command"].as_str(), Some(EXE));
        assert_eq!(
            v["mcp_servers"]["tower"]["env_vars"].as_array().unwrap()[0].as_str(),
            Some("WEZTERM_PANE")
        );
        assert!(codex_has(&d));
        assert!(!codex_set(&mut d, Some(EXE)).unwrap(), "already there");
        assert!(codex_set(&mut d, None).unwrap());
        let v: toml::Value = toml::from_str(&d.to_string()).unwrap();
        assert!(v["mcp_servers"].get("tower").is_none());
        assert!(v["mcp_servers"]["github"].is_table());
    }

    #[test]
    fn files_are_backed_up_once_and_a_missing_claude_config_is_left_alone() {
        let dir =
            std::env::temp_dir().join(format!("tower-connect-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let claude = dir.join(".claude.json");
        assert!(!apply_claude(&claude, Some(EXE)).unwrap());
        assert!(!claude.exists());
        std::fs::write(&claude, "{\"numStartups\": 1}").unwrap();
        assert!(apply_claude(&claude, Some(EXE)).unwrap());
        let backup = dir.join(".claude.json.tower-backup");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "{\"numStartups\": 1}"
        );
        assert!(apply_claude(&claude, None).unwrap());
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "{\"numStartups\": 1}",
            "the first backup is kept"
        );
        std::fs::write(&claude, "{not json").unwrap();
        assert!(apply_claude(&claude, Some(EXE)).is_err());

        let codex = dir.join("codex").join("config.toml");
        assert!(apply_codex(&codex, Some(EXE)).unwrap(), "created");
        assert!(codex_has(
            &std::fs::read_to_string(&codex).unwrap().parse().unwrap()
        ));
        assert!(!dir.join("codex").join("config.toml.tower-tmp").exists());
    }

    #[test]
    fn tower_keeps_its_own_copy_of_the_binary() {
        let dir = std::env::temp_dir().join(format!("tower-exe-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("built-tower");
        std::fs::write(&src, b"binary v1").unwrap();
        let data = dir.join("data");
        let copy = install_exe(&src, &data).unwrap();
        assert_eq!(copy, stable_exe(&data));
        std::fs::write(&src, b"binary v2").unwrap();
        install_exe(&src, &data).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"binary v2", "refreshed");
        assert_eq!(install_exe(&copy, &data).unwrap(), copy, "already the copy");
    }
}
