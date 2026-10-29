//! The relay that lets the usage popup show Claude's rate limits.
//!
//! Claude Code hands the command configured as its `statusLine` a JSON
//! object on every update, and for Pro/Max accounts that object carries the
//! five-hour and weekly windows (`rate_limits`). Nothing else on disk does.
//! Connecting the relay therefore:
//!
//! 1. saves the user's current `statusLine` setting beside the terminal's
//!    state files,
//! 2. writes a small POSIX `sh` script that stores each payload it receives
//!    and then pipes it into that saved command, so the user's own status
//!    line keeps rendering exactly as before,
//! 3. points `statusLine` in Claude's `settings.json` at the script, after
//!    copying the file to `settings.json.bak`.
//!
//! It is on by default (`agent_ui.section.claude_usage_relay`): the herd
//! scan connects it whenever Claude Code is installed and the relay is not
//! in place, and disconnects it when the setting is turned off. Disconnecting
//! puts the saved setting back and deletes the script. No credentials are
//! read and nothing leaves the machine; the payload is the one Claude Code
//! already gives any status line.
//!
//! Unix only for now.
//! Blocking file I/O throughout: call it off the GUI thread.

use crate::agent_herd::quota;
use anyhow::{bail, Context};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Whether the relay can run on this platform.
pub const SUPPORTED: bool = cfg!(unix);

/// The files the relay reads and writes.
#[derive(Clone, Debug)]
pub struct RelayPaths {
    /// Claude Code's user settings.
    pub settings: PathBuf,
    /// The relay script.
    pub script: PathBuf,
    /// The `statusLine` setting as it was before connecting.
    pub original: PathBuf,
    /// The last payload, read by [`quota::poll_claude_sample`].
    pub sample: PathBuf,
}

impl RelayPaths {
    /// The paths for this user: Claude's config dir honours
    /// `CLAUDE_CONFIG_DIR` as Claude Code does.
    pub fn for_user() -> Option<Self> {
        let claude_dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => dirs_next::home_dir()?.join(".claude"),
        };
        let data = &*config::DATA_DIR;
        Some(Self {
            settings: claude_dir.join("settings.json"),
            script: data.join(config::brand::state_file_name(
                "claude-statusline-relay",
                "sh",
            )),
            original: data.join(config::brand::state_file_name(
                "claude-statusline-original",
                "json",
            )),
            sample: quota::claude_sample_path(data),
        })
    }
}

/// Whether Claude's status line currently goes through the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayStatus {
    /// Not offered on this platform.
    Unsupported,
    Disconnected,
    Connected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectOutcome {
    Connected { had_status_line: bool },
    AlreadyConnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectOutcome {
    /// The saved setting was put back (or removed, if there was none).
    Restored,
    /// The relay was connected but its saved setting was missing, so the
    /// `statusLine` key was removed; `settings.json.bak` still has it.
    RemovedWithoutOriginal,
    NotConnected,
}

fn quote(text: &str) -> String {
    match shlex::try_quote(text) {
        Ok(quoted) => quoted.into_owned(),
        // Only a NUL byte is refused; no path or command can contain one.
        Err(_) => format!("'{}'", text.replace('\'', "'\\''")),
    }
}

/// The `statusLine.command` that runs the relay.
pub fn relay_command(script: &Path) -> String {
    format!("sh {}", quote(&script.to_string_lossy()))
}

/// The relay script's text. `original` is the command to pipe each payload
/// into; without one the script prints the model name, so Claude Code's
/// status row is not left empty.
pub fn relay_script(sample: &Path, original: Option<&str>) -> String {
    let sample = quote(&sample.to_string_lossy());
    let show = match original {
        Some(command) => format!("printf '%s' \"$input\" | sh -c {}\n", quote(command)),
        None => "model=$(printf '%s' \"$input\" | sed -n \
                 's/.*\"display_name\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' | head -n 1)\n\
                 [ -n \"$model\" ] && printf '%s\\n' \"$model\"\n"
            .to_string(),
    };
    format!(
        "#!/bin/sh\n\
         # Installed by TGZTerminal (agent_ui.section.claude_usage_relay).\n\
         # Saves the JSON Claude Code hands its status line, so the terminal can\n\
         # show your 5-hour and weekly limits, then shows the status line you had\n\
         # before. Setting claude_usage_relay = false restores your settings.\n\
         input=$(cat)\n\
         sample={sample}\n\
         tmp=\"$sample.$$\"\n\
         if printf '%s' \"$input\" > \"$tmp\" 2>/dev/null; then\n\
         \x20 mv -f \"$tmp\" \"$sample\" 2>/dev/null || rm -f \"$tmp\"\n\
         fi\n\
         {show}"
    )
}

fn is_connected(settings: &Value, script: &Path) -> bool {
    let script = script.to_string_lossy();
    settings
        .get("statusLine")
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(script.as_ref()))
}

/// The command a saved `statusLine` runs, when it is a command one.
fn original_command(original: Option<&Value>, script: &Path) -> Option<String> {
    let line = original?.as_object()?;
    if line
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("command")
        != "command"
    {
        return None;
    }
    let command = line.get("command")?.as_str()?.trim();
    // Never pipe the relay into itself.
    (!command.is_empty() && !command.contains(script.to_string_lossy().as_ref()))
        .then(|| command.to_string())
}

/// Settings and their raw bytes, or `None` when the file does not exist.
/// A file that does not parse as a JSON object is an error: it is never
/// overwritten with something built from a guess.
fn read_settings(path: &Path) -> anyhow::Result<Option<(Value, Vec<u8>)>> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let value: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    if !value.is_object() {
        bail!("{} is not a JSON object", path.display());
    }
    Ok(Some((value, raw)))
}

/// Write via a temporary file and a rename, so a reader never sees half a
/// file. Keeps the permissions of the file being replaced.
fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tgz-tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    let permissions = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = match (permissions, mode) {
            (_, Some(mode)) => std::fs::Permissions::from_mode(mode),
            (Some(permissions), None) => permissions,
            (None, None) => std::fs::Permissions::from_mode(0o644),
        };
        std::fs::set_permissions(&tmp, permissions).ok();
    }
    #[cfg(not(unix))]
    {
        let _ = (permissions, mode);
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn write_settings(path: &Path, settings: &Value, previous: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(previous) = previous {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        std::fs::write(path.with_file_name(format!("{name}.bak")), previous)
            .context("writing the settings backup")?;
    }
    let mut text = serde_json::to_string_pretty(settings)?;
    text.push('\n');
    write_atomic(path, text.as_bytes(), None)
}

/// The saved setting: `Some(None)` when there was no `statusLine` to save,
/// `None` when nothing was saved.
fn read_original(path: &Path) -> Option<Option<Value>> {
    let saved: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let line = saved.get("statusLine")?;
    Some((!line.is_null()).then(|| line.clone()))
}

fn write_script(paths: &RelayPaths, original: Option<&str>) -> anyhow::Result<()> {
    write_atomic(
        &paths.script,
        relay_script(&paths.sample, original).as_bytes(),
        Some(0o755),
    )
}

/// Route Claude's status line through the relay.
pub fn connect(paths: &RelayPaths) -> anyhow::Result<ConnectOutcome> {
    let (mut settings, previous) = match read_settings(&paths.settings)? {
        Some((settings, raw)) => (settings, Some(raw)),
        None => (Value::Object(Map::new()), None),
    };
    if is_connected(&settings, &paths.script) {
        // The script may have been deleted since; rebuild it.
        let original = read_original(&paths.original).flatten();
        write_script(
            paths,
            original_command(original.as_ref(), &paths.script).as_deref(),
        )?;
        return Ok(ConnectOutcome::AlreadyConnected);
    }

    let original = settings.get("statusLine").cloned();
    let saved = serde_json::json!({ "statusLine": original.clone().unwrap_or(Value::Null) });
    write_atomic(
        &paths.original,
        serde_json::to_string_pretty(&saved)?.as_bytes(),
        Some(0o600),
    )?;
    write_script(
        paths,
        original_command(original.as_ref(), &paths.script).as_deref(),
    )?;

    // Keep `padding` and `refreshInterval`: they shape the same status row.
    let mut line = match &original {
        Some(Value::Object(line)) => line.clone(),
        _ => Map::new(),
    };
    line.insert("type".into(), Value::String("command".into()));
    line.insert(
        "command".into(),
        Value::String(relay_command(&paths.script)),
    );
    settings
        .as_object_mut()
        .expect("read_settings only returns objects")
        .insert("statusLine".into(), Value::Object(line));
    write_settings(&paths.settings, &settings, previous.as_deref())?;
    Ok(ConnectOutcome::Connected {
        had_status_line: original.is_some(),
    })
}

/// Put the saved `statusLine` back and remove the relay's files.
pub fn disconnect(paths: &RelayPaths) -> anyhow::Result<DisconnectOutcome> {
    let outcome = match read_settings(&paths.settings)? {
        Some((mut settings, previous)) if is_connected(&settings, &paths.script) => {
            let saved = read_original(&paths.original);
            let object = settings
                .as_object_mut()
                .expect("read_settings only returns objects");
            let outcome = match saved {
                Some(Some(line)) => {
                    object.insert("statusLine".into(), line);
                    DisconnectOutcome::Restored
                }
                Some(None) => {
                    object.remove("statusLine");
                    DisconnectOutcome::Restored
                }
                None => {
                    object.remove("statusLine");
                    DisconnectOutcome::RemovedWithoutOriginal
                }
            };
            write_settings(&paths.settings, &settings, Some(&previous))?;
            outcome
        }
        _ => DisconnectOutcome::NotConnected,
    };
    for file in [&paths.script, &paths.original, &paths.sample] {
        std::fs::remove_file(file).ok();
    }
    Ok(outcome)
}

/// `settings.json`'s path and mtime, and whether it pointed at the relay then.
static STATUS: LazyLock<Mutex<Option<(PathBuf, Option<SystemTime>, bool)>>> =
    LazyLock::new(Default::default);

/// Whether Claude's status line goes through the relay. Re-reads
/// `settings.json` only when it changed, so it is cheap to ask on every herd
/// scan.
pub fn status(paths: &RelayPaths) -> RelayStatus {
    if !SUPPORTED {
        return RelayStatus::Unsupported;
    }
    let mtime = std::fs::metadata(&paths.settings)
        .and_then(|meta| meta.modified())
        .ok();
    let mut cached = STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let connected = match &*cached {
        Some((path, seen, connected)) if *path == paths.settings && *seen == mtime => *connected,
        _ => {
            let connected = read_settings(&paths.settings)
                .ok()
                .flatten()
                .is_some_and(|(settings, _)| is_connected(&settings, &paths.script));
            *cached = Some((paths.settings.clone(), mtime, connected));
            connected
        }
    };
    if connected {
        RelayStatus::Connected
    } else {
        RelayStatus::Disconnected
    }
}

/// Forget the cached status, so the next [`status`] re-reads the settings
/// (their mtime can stay put within one second of our own write).
pub fn invalidate_status() {
    *STATUS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// The status as the herd scan last saw it, for paint.
static LAST: LazyLock<Mutex<Option<RelayStatus>>> = LazyLock::new(Default::default);

/// Look at the settings again and remember the answer for [`last_status`].
/// Called from the herd scan's worker thread.
pub fn refresh_status(paths: &RelayPaths) -> RelayStatus {
    let status = status(paths);
    *LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(status);
    status
}

/// The status as last seen, without touching the disk; `None` before the
/// first herd scan has looked.
pub fn last_status() -> Option<RelayStatus> {
    if !SUPPORTED {
        return Some(RelayStatus::Unsupported);
    }
    *LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the herd scan should do about the relay: `Some(true)` connect,
/// `Some(false)` disconnect, `None` leave it. `claude_installed` is whether
/// Claude's config directory exists: the relay never creates it for someone
/// who does not use Claude Code.
pub fn relay_change(wanted: bool, status: RelayStatus, claude_installed: bool) -> Option<bool> {
    match status {
        RelayStatus::Unsupported => None,
        RelayStatus::Disconnected if wanted && claude_installed => Some(true),
        RelayStatus::Connected if !wanted => Some(false),
        _ => None,
    }
}

/// The `settings.json` mtime the last automatic change was attempted at.
/// A change is tried once per version of the file, so a tool that keeps
/// rewriting `statusLine` gets wrapped once per rewrite, not in a loop, and
/// a file that cannot be parsed is not retried until it is edited.
static LAST_ATTEMPT: LazyLock<Mutex<Option<(bool, Option<SystemTime>)>>> =
    LazyLock::new(Default::default);

fn toast(title: &str, message: String) {
    wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
        title: title.to_string(),
        message,
        url: None,
        timeout: Some(std::time::Duration::from_secs(8)),
    });
}

/// Connect the relay when `wanted` and disconnect it when not. Called from
/// the herd scan's worker thread on every scan; returns at once unless a
/// change is due. Only a first-time connect is announced: one that finds
/// the relay already in place, as every launch after the first does, is
/// silent.
pub fn ensure(paths: &RelayPaths, wanted: bool) -> RelayStatus {
    let status = refresh_status(paths);
    let claude_installed = paths.settings.parent().is_some_and(Path::is_dir);
    let Some(attach) = relay_change(wanted, status, claude_installed) else {
        return status;
    };
    let mtime = std::fs::metadata(&paths.settings)
        .and_then(|meta| meta.modified())
        .ok();
    {
        let mut last = LAST_ATTEMPT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == Some((attach, mtime)) {
            return status;
        }
        *last = Some((attach, mtime));
    }
    let message = if attach {
        connect(paths).map(|outcome| match outcome {
            ConnectOutcome::Connected {
                had_status_line: true,
            } => Some(
                "Claude's 5-hour and weekly limits now show in the sidebar's \u{3a3} popup. \
                 Your status line still shows as before; previous settings: \
                 settings.json.bak. Turn off: agent_ui.section.claude_usage_relay = false"
                    .to_string(),
            ),
            ConnectOutcome::Connected {
                had_status_line: false,
            } => Some(
                "Claude's 5-hour and weekly limits now show in the sidebar's \u{3a3} popup. \
                 Claude Code shows the model name in its status row. \
                 Turn off: agent_ui.section.claude_usage_relay = false"
                    .to_string(),
            ),
            ConnectOutcome::AlreadyConnected => None,
        })
    } else {
        disconnect(paths).map(|_| None)
    };
    match message {
        Ok(Some(message)) => toast("Claude usage limits connected", message),
        Ok(None) => {}
        Err(err) => log::warn!(
            "claude usage relay: could not {}: {err:#}",
            if attach { "connect" } else { "disconnect" }
        ),
    }
    invalidate_status();
    refresh_status(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> RelayPaths {
        RelayPaths {
            settings: dir.join("claude").join("settings.json"),
            script: dir.join("data").join("relay.sh"),
            original: dir.join("data").join("original.json"),
            sample: dir.join("data").join("sample.json"),
        }
    }

    fn settings(paths: &RelayPaths) -> Value {
        serde_json::from_slice(&std::fs::read(&paths.settings).unwrap()).unwrap()
    }

    #[test]
    fn connect_then_disconnect_restores_the_original_status_line() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        let before = r#"{
  "model": "opus",
  "statusLine": {"type": "command", "command": "sh ~/.claude/statusline-command.sh", "padding": 0},
  "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "echo hi"}]}]}
}"#;
        std::fs::write(&paths.settings, before).unwrap();

        let outcome = connect(&paths).unwrap();
        assert_eq!(
            outcome,
            ConnectOutcome::Connected {
                had_status_line: true
            }
        );
        let connected = settings(&paths);
        assert_eq!(
            connected["statusLine"]["command"],
            relay_command(&paths.script)
        );
        assert_eq!(connected["statusLine"]["padding"], 0);
        assert_eq!(
            connected["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo hi"
        );
        let backup = paths.settings.with_file_name("settings.json.bak");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before);
        let script = std::fs::read_to_string(&paths.script).unwrap();
        assert!(script.contains("| sh -c 'sh ~/.claude/statusline-command.sh'"));
        assert_eq!(status(&paths), RelayStatus::Connected);

        assert_eq!(connect(&paths).unwrap(), ConnectOutcome::AlreadyConnected);

        assert_eq!(disconnect(&paths).unwrap(), DisconnectOutcome::Restored);
        let restored = settings(&paths);
        let original: Value = serde_json::from_str(before).unwrap();
        assert_eq!(restored, original);
        assert!(!paths.script.exists());
        assert!(!paths.original.exists());
        invalidate_status();
        assert_eq!(status(&paths), RelayStatus::Disconnected);
    }

    #[test]
    fn connect_without_settings_or_status_line() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        assert_eq!(
            connect(&paths).unwrap(),
            ConnectOutcome::Connected {
                had_status_line: false
            }
        );
        let script = std::fs::read_to_string(&paths.script).unwrap();
        assert!(script.contains("display_name"));
        assert_eq!(disconnect(&paths).unwrap(), DisconnectOutcome::Restored);
        assert_eq!(settings(&paths), serde_json::json!({}));
    }

    #[test]
    fn unparsable_settings_are_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, "{ not json").unwrap();
        assert!(connect(&paths).is_err());
        assert_eq!(
            std::fs::read_to_string(&paths.settings).unwrap(),
            "{ not json"
        );
        assert!(!paths.script.exists());
    }

    #[test]
    fn disconnect_when_not_connected_leaves_settings_alone() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
        std::fs::write(&paths.settings, r#"{"statusLine":{"command":"x"}}"#).unwrap();
        assert_eq!(disconnect(&paths).unwrap(), DisconnectOutcome::NotConnected);
        assert_eq!(
            std::fs::read_to_string(&paths.settings).unwrap(),
            r#"{"statusLine":{"command":"x"}}"#
        );
    }

    #[test]
    fn the_relay_is_connected_only_for_claude_users_and_removed_when_off() {
        use RelayStatus::*;
        assert_eq!(relay_change(true, Disconnected, true), Some(true));
        assert_eq!(relay_change(true, Disconnected, false), None);
        assert_eq!(relay_change(true, Connected, true), None);
        assert_eq!(relay_change(false, Connected, true), Some(false));
        assert_eq!(relay_change(false, Connected, false), Some(false));
        assert_eq!(relay_change(false, Disconnected, true), None);
        assert_eq!(relay_change(true, Unsupported, true), None);
    }

    #[cfg(unix)]
    #[test]
    fn relay_script_saves_the_payload_and_runs_the_original() {
        let temp = tempfile::tempdir().unwrap();
        let sample = temp.path().join("it's sample.json");
        let script = relay_script(&sample, Some("cat | wc -c | tr -d ' '"));
        let path = temp.path().join("relay.sh");
        std::fs::write(&path, script).unwrap();
        let mut child = std::process::Command::new("sh")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"model":{"display_name":"Opus"}}"#)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "33");
        assert_eq!(
            std::fs::read_to_string(&sample).unwrap(),
            r#"{"model":{"display_name":"Opus"}}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn relay_script_default_prints_the_model() {
        let temp = tempfile::tempdir().unwrap();
        let sample = temp.path().join("sample.json");
        let path = temp.path().join("relay.sh");
        std::fs::write(&path, relay_script(&sample, None)).unwrap();
        let mut child = std::process::Command::new("sh")
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"model": {"id": "x", "display_name": "Opus 5.5"}}"#)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Opus 5.5");
    }
}
