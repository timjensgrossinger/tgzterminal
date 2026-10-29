//! Pane driver: a real interactive agent CLI in a terminal pane.
//!
//! towerd only prepares the launch; the Tower view spawns the pane next to
//! itself and reports the pane id back. Each vendor's CLI takes the brief,
//! extra instructions and Tower's MCP server its own way:
//!
//! | CLI | first prompt | instructions | `tower` MCP server | turn end |
//! |---|---|---|---|---|
//! | Claude Code | `-- <brief>` | `--append-system-prompt` | `--mcp-config` file | hooks (`tower hook`) |
//! | Codex | `-- <brief>` | `-c developer_instructions` | `-c mcp_servers.tower.*` | `-c notify` (`tower notify`) |
//! | OpenCode | `--prompt` | in the prompt | `OPENCODE_CONFIG_CONTENT` | none |
//! | Antigravity (`agy`) | `<brief>` | in the prompt | `.agents/mcp_config.json`, only in folders Tower made | none |
//! | Gemini CLI | `-i` | in the prompt | none (use `gemini --experimental-acp`) | none |
//! | anything else | typed in once it starts | in the prompt | none | none |
//!
//! Without a turn-end signal Tower cannot tell busy from idle, so messages
//! to such an agent are typed in right away (these CLIs queue input).
//! Nothing here touches sign-in: the user's own login is used as is.

use crate::config::RoleCfg;
use crate::paths::Paths;
use serde_json::json;
use std::path::Path;

pub struct PaneLaunch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The CLI reports turn ends (hooks or notify).
    pub turn_signal: bool,
    /// The CLI takes no first prompt on its command line: type this in.
    pub type_brief: Option<String>,
}

/// Where a CLI that reads MCP servers only from a project file may get one:
/// a folder Tower created itself (a flight folder or a worktree). `None`:
/// the user's own checkout, which Tower never writes into.
pub type OwnDir<'a> = Option<&'a Path>;

/// Hook events Tower listens to.
pub const HOOK_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Quote for `sh`: single quotes, with embedded ones closed and escaped.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Clears Claude Code's child-session markers (a terminal started from inside
/// a Claude session would otherwise make agents think they are sub-sessions,
/// which turns off their transcripts), prints OSC 1337 SetUserVar
/// agent.name=<role> so the sidebar shows the role, then becomes the agent. The role name arrives through the environment, so
/// nothing from a role file is ever interpolated into shell syntax.
const SHIM: &str = r#"unset CLAUDECODE CLAUDE_CODE_CHILD_SESSION CLAUDE_CODE_ENTRYPOINT; printf '\033]1337;SetUserVar=%s=%s\007' agent.name "$(printf %s "$TOWER_ROLE" | base64 | tr -d '\n')"; exec "$@""#;

/// Quote a program path for the shell Claude Code runs hook commands in.
pub fn quote_exe(exe: &Path) -> String {
    let s = exe.display().to_string();
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', ""))
    } else {
        sh_quote(&s)
    }
}

pub fn settings_json(exe: &Path, deny: &[String]) -> serde_json::Value {
    let mut hooks = serde_json::Map::new();
    for ev in HOOK_EVENTS.iter() {
        let cmd = format!("{} hook {}", quote_exe(exe), ev);
        let entry = if matches!(*ev, "PreToolUse" | "PostToolUse") {
            json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] }])
        } else {
            json!([{ "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] }])
        };
        hooks.insert(ev.to_string(), entry);
    }
    if deny.is_empty() {
        json!({ "hooks": hooks })
    } else {
        json!({ "hooks": hooks, "permissions": { "deny": deny } })
    }
}

pub fn mcp_json(exe: &Path, agent_id: &str, socket: &Path, token: &str) -> serde_json::Value {
    json!({
        "mcpServers": {
            "tower": {
                "command": exe.display().to_string(),
                "args": ["mcp"],
                "env": {
                    "TOWER_AGENT_ID": agent_id,
                    "TOWER_SOCK": socket.display().to_string(),
                    "TOWER_AGENT_TOKEN": token,
                }
            }
        }
    })
}

/// Prepare a pane launch for `role`'s CLI.
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    paths: &Paths,
    role: &RoleCfg,
    agent_id: &str,
    session_id: &str,
    model: Option<&str>,
    title: &str,
    system: &str,
    brief: &str,
    token: &str,
    extra: &[String],
    deny: &[String],
    own_dir: OwnDir,
) -> std::io::Result<PaneLaunch> {
    use crate::compliance::Vendor;
    let vendor = crate::compliance::vendor_of(role.driver, &role.command);
    if vendor == Vendor::Claude {
        return prepare_claude(
            paths, role, agent_id, session_id, model, title, system, brief, token, extra, deny,
        );
    }
    // Other CLIs have no system-prompt flag worth trusting: the role's
    // instructions open the first prompt instead.
    let prompt = format!("{}\n\n---\n\n{brief}", system.trim());
    let mut argv = shim_prefix();
    argv.extend(role.command.iter().cloned());
    let mut env = base_env(paths, role, agent_id, token);
    let mut added: Vec<String> = Vec::new();
    let mut turn_signal = false;
    let mut type_brief = None;
    match vendor {
        Vendor::Codex => {
            let exe = paths.exe.display().to_string();
            for (k, v) in [
                ("mcp_servers.tower.command", toml_str(&exe)),
                ("mcp_servers.tower.args", "[\"mcp\"]".to_string()),
                // Passed through from the pane's environment, so the token
                // never appears on a command line.
                (
                    "mcp_servers.tower.env_vars",
                    "[\"TOWER_AGENT_ID\", \"TOWER_SOCK\", \"TOWER_AGENT_TOKEN\"]".to_string(),
                ),
                ("notify", format!("[{}, \"notify\"]", toml_str(&exe))),
            ] {
                added.extend(["-c".to_string(), format!("{k}={v}")]);
            }
            added.extend([
                "-c".to_string(),
                format!("developer_instructions={}", toml_str(system.trim())),
            ]);
            for (name, s) in &role.mcp {
                let args: Vec<String> = s.args.iter().map(|a| toml_str(a)).collect();
                let vars: Vec<String> = s.env.keys().map(|k| toml_str(k)).collect();
                for (k, v) in [
                    ("command", toml_str(&s.command)),
                    ("args", format!("[{}]", args.join(", "))),
                    ("env_vars", format!("[{}]", vars.join(", "))),
                ] {
                    added.extend(["-c".to_string(), format!("mcp_servers.{name}.{k}={v}")]);
                }
                // Values only in the environment, never on the command line.
                env.extend(s.env.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            if let Some(m) = model {
                added.extend(["-m".to_string(), m.to_string()]);
            }
            turn_signal = true;
        }
        Vendor::Opencode => {
            env.push((
                "OPENCODE_CONFIG_CONTENT".into(),
                opencode_config(paths, role, agent_id, token).to_string(),
            ));
            if let Some(m) = model {
                added.extend(["-m".to_string(), m.to_string()]);
            }
            added.extend(["--prompt".to_string(), prompt.clone()]);
        }
        Vendor::Antigravity => {
            if let Some(dir) = own_dir {
                write_agy_mcp(dir, paths, role, agent_id, token)?;
            }
        }
        Vendor::Gemini => {
            if let Some(m) = model {
                added.extend(["-m".to_string(), m.to_string()]);
            }
            added.extend(["-i".to_string(), prompt.clone()]);
        }
        _ => type_brief = Some(prompt.clone()),
    }
    if let Some(problem) = crate::compliance::lint_args(&added).first() {
        return Err(std::io::Error::other(format!(
            "refusing to start: Tower's launch arguments {problem}"
        )));
    }
    argv.extend(added);
    argv.extend(extra.iter().cloned());
    match vendor {
        Vendor::Codex => argv.extend(["--".to_string(), prompt]),
        Vendor::Antigravity => argv.push(prompt),
        _ => {}
    }
    let _ = (session_id, title);
    Ok(PaneLaunch {
        argv,
        env,
        turn_signal,
        type_brief,
    })
}

/// Add the role's own `[mcp.<name>]` servers to an `mcpServers` object.
pub fn with_role_servers(mut servers: serde_json::Value, role: &RoleCfg) -> serde_json::Value {
    if let Some(map) = servers.as_object_mut() {
        for (name, s) in &role.mcp {
            map.insert(
                name.clone(),
                json!({"command": s.command, "args": s.args, "env": s.env}),
            );
        }
    }
    servers
}

/// Write a file only this user can read (it may hold tokens).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A TOML basic string, escaped.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn shim_prefix() -> Vec<String> {
    // The shim needs a POSIX shell; on Windows the agent starts directly
    // (no sidebar role name there yet).
    if cfg!(windows) {
        vec![]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            SHIM.to_string(),
            "tower-agent".to_string(),
        ]
    }
}

fn base_env(paths: &Paths, role: &RoleCfg, agent_id: &str, token: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("TOWER_AGENT_ID".into(), agent_id.into()),
        ("TOWER_SOCK".into(), paths.socket.display().to_string()),
        ("TOWER_ROLE".into(), role.name.clone()),
        ("TOWER_AGENT_TOKEN".into(), token.into()),
    ];
    // The pane is opened by the GUI, whose PATH is launchd's when it was
    // started from the Dock; towerd's PATH (see `envpath`) is the one that
    // finds `claude`, `npx` and `node`. A Tower tab in a remote domain gets
    // this machine's PATH too, which is the known limit.
    if let Some(path) = std::env::var_os("PATH") {
        env.push(("PATH".into(), path.to_string_lossy().to_string()));
    }
    env
}

/// `OPENCODE_CONFIG_CONTENT`: merged over the user's config, adds only the
/// `tower` MCP server.
pub fn opencode_config(
    paths: &Paths,
    role: &RoleCfg,
    agent_id: &str,
    token: &str,
) -> serde_json::Value {
    let mut mcp = json!({
        "tower": {
            "type": "local",
            "command": [paths.exe.display().to_string(), "mcp"],
            "enabled": true,
            "environment": {
                "TOWER_AGENT_ID": agent_id,
                "TOWER_SOCK": paths.socket.display().to_string(),
                "TOWER_AGENT_TOKEN": token,
            }
        }
    });
    for (name, s) in &role.mcp {
        let mut command = vec![s.command.clone()];
        command.extend(s.args.iter().cloned());
        mcp[name.as_str()] =
            json!({"type": "local", "command": command, "enabled": true, "environment": s.env});
    }
    json!({ "mcp": mcp })
}

/// Antigravity reads MCP servers only from `.agents/mcp_config.json`. Tower
/// writes one only into a folder it created, keeps it out of git, and never
/// overwrites a file it did not write.
pub fn write_agy_mcp(
    dir: &Path,
    paths: &Paths,
    role: &RoleCfg,
    agent_id: &str,
    token: &str,
) -> std::io::Result<()> {
    let agents = dir.join(".agents");
    let file = agents.join("mcp_config.json");
    if file.exists() {
        let ours = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .is_some_and(|v| v.pointer("/mcpServers/tower").is_some());
        if !ours {
            return Ok(());
        }
    }
    std::fs::create_dir_all(&agents)?;
    let tower = mcp_json(&paths.exe, agent_id, &paths.socket, token);
    let v = json!({"mcpServers": with_role_servers(tower["mcpServers"].clone(), role)});
    write_private(&file, &serde_json::to_vec_pretty(&v)?)?;
    git_exclude(dir, ".agents/mcp_config.json");
    Ok(())
}

/// Add `pattern` to the repo's `.git/info/exclude` (a worktree's `.git` is a
/// file pointing at its git dir). Best effort: no git, no exclude.
fn git_exclude(dir: &Path, pattern: &str) {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if let Ok(text) = std::fs::read_to_string(&dot_git) {
        match text.trim().strip_prefix("gitdir:") {
            Some(p) => dir.join(p.trim()),
            None => return,
        }
    } else {
        return;
    };
    let exclude = git_dir.join("info").join("exclude");
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    if current.lines().any(|l| l.trim() == pattern) {
        return;
    }
    let _ = std::fs::create_dir_all(git_dir.join("info"));
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(pattern);
    next.push('\n');
    let _ = std::fs::write(&exclude, next);
}

#[allow(clippy::too_many_arguments)]
fn prepare_claude(
    paths: &Paths,
    role: &RoleCfg,
    agent_id: &str,
    session_id: &str,
    model: Option<&str>,
    title: &str,
    system: &str,
    brief: &str,
    token: &str,
    extra: &[String],
    deny: &[String],
) -> std::io::Result<PaneLaunch> {
    let dir = paths.agent_dir(agent_id);
    std::fs::create_dir_all(&dir)?;
    let settings = dir.join("settings.json");
    let mcp = dir.join("mcp.json");
    let settings_value = settings_json(&paths.exe, deny);
    // Never ship a settings file that changes how the agent signs in.
    if let Some(problem) = crate::compliance::lint_settings(&settings_value).first() {
        return Err(std::io::Error::other(format!(
            "refusing to start: Tower's agent settings {problem}"
        )));
    }
    write_private(&settings, &serde_json::to_vec_pretty(&settings_value)?)?;
    let tower = mcp_json(&paths.exe, agent_id, &paths.socket, token);
    let servers = with_role_servers(tower["mcpServers"].clone(), role);
    write_private(
        &mcp,
        &serde_json::to_vec_pretty(&json!({"mcpServers": servers}))?,
    )?;

    let mut argv = shim_prefix();
    argv.extend(role.command.iter().cloned());
    argv.extend([
        "--session-id".to_string(),
        session_id.to_string(),
        "-n".to_string(),
        title.to_string(),
        "--append-system-prompt".to_string(),
        system.to_string(),
        "--settings".to_string(),
        settings.display().to_string(),
        "--mcp-config".to_string(),
        mcp.display().to_string(),
        // Tower's own team tools only; everything else keeps Claude's
        // normal permission prompts.
        "--allowedTools".to_string(),
        "mcp__tower".to_string(),
    ]);
    if let Some(m) = model {
        argv.push("--model".to_string());
        argv.push(m.to_string());
    }
    // What Tower adds must not switch the agent's own prompts off; the
    // role's command and `extra` are the user's choice.
    if let Some(problem) = crate::compliance::lint_args(&argv).first() {
        return Err(std::io::Error::other(format!(
            "refusing to start: Tower's launch arguments {problem}"
        )));
    }
    argv.extend(extra.iter().cloned());
    // `--mcp-config` takes several values; `--` ends options so the brief is
    // always the prompt, even when it starts with a dash.
    argv.push("--".to_string());
    argv.push(brief.to_string());

    Ok(PaneLaunch {
        argv,
        env: base_env(paths, role, agent_id, token),
        turn_signal: true,
        type_brief: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn settings_wire_every_hook_to_the_exe() {
        let v = settings_json(Path::new("/x/my tower"), &[]);
        assert!(v.get("permissions").is_none());
        let d = settings_json(Path::new("/x"), &["Bash(*db*)".to_string()]);
        assert_eq!(d["permissions"]["deny"][0], "Bash(*db*)");
        for ev in HOOK_EVENTS.iter() {
            let cmd = v["hooks"][ev][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, format!("'/x/my tower' hook {ev}"));
        }
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
    }

    #[test]
    fn argv_keeps_untrusted_text_out_of_the_shell() {
        let d = std::env::temp_dir().join(format!("tower-pane-{}", crate::paths::random_u64()));
        let paths = Paths::new(d.join("towerd.sock"), Some(d.join("cfg")), None);
        let role =
            crate::config::parse_role("approach", "a.toml".as_ref(), crate::config::SEED_APPROACH)
                .unwrap();
        let evil = "-rf $(rm -rf ~) `x` 'q'";
        let l = prepare(
            &paths,
            &role,
            "approach-1",
            "u-u-i-d",
            Some("opus"),
            "Approach · TWR-1",
            "sys",
            evil,
            "tok",
            &["--effort".to_string(), "low".to_string()],
            &[],
            None,
        )
        .unwrap();
        assert!(l.turn_signal && l.type_brief.is_none());
        assert!(l
            .env
            .contains(&("TOWER_AGENT_TOKEN".to_string(), "tok".to_string())));
        // The shell script is a constant; everything else is positional.
        assert_eq!(l.argv[2], SHIM);
        assert_eq!(l.argv.last().unwrap(), evil);
        assert_eq!(l.argv[l.argv.len() - 2], "--");
        assert!(l
            .argv
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "opus"));
        assert_eq!(
            l.argv[l.argv.len() - 4..l.argv.len() - 2],
            ["--effort", "low"]
        );
        assert!(paths.agent_dir("approach-1").join("settings.json").exists());
        let mcp: serde_json::Value = serde_json::from_slice(
            &std::fs::read(paths.agent_dir("approach-1").join("mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            mcp["mcpServers"]["tower"]["env"]["TOWER_AGENT_ID"],
            "approach-1"
        );
    }

    fn vendor_role(command: &[&str]) -> RoleCfg {
        let mut r =
            crate::config::parse_role("approach", "a.toml".as_ref(), crate::config::SEED_APPROACH)
                .unwrap();
        r.command = command.iter().map(|s| s.to_string()).collect();
        r
    }

    fn launch(command: &[&str], own: OwnDir) -> (PaneLaunch, Paths) {
        let d = std::env::temp_dir().join(format!("tower-pane-{}", crate::paths::random_u64()));
        let paths = Paths::new(d.join("towerd.sock"), Some(d.join("cfg")), None);
        let l = prepare(
            &paths,
            &vendor_role(command),
            "a-1",
            "sid",
            Some("m1"),
            "T",
            "Be careful.",
            "Fix the bug",
            "secret-token",
            &[],
            &[],
            own,
        )
        .unwrap();
        (l, paths)
    }

    #[test]
    fn codex_gets_tools_instructions_and_notify_without_the_token_on_argv() {
        let (l, _) = launch(&["codex"], None);
        let joined = l.argv.join(" ");
        assert!(joined.contains("mcp_servers.tower.args=[\"mcp\"]"));
        assert!(joined.contains("mcp_servers.tower.env_vars="));
        assert!(joined.contains("developer_instructions=\"Be careful.\""));
        assert!(joined.contains("notify=["));
        assert!(
            !joined.contains("secret-token"),
            "token only in the environment"
        );
        assert!(l
            .env
            .contains(&("TOWER_AGENT_TOKEN".into(), "secret-token".into())));
        assert_eq!(l.argv[l.argv.len() - 2], "--");
        assert!(l.argv.last().unwrap().ends_with("Fix the bug"));
        assert!(l.argv.windows(2).any(|w| w[0] == "-m" && w[1] == "m1"));
        assert!(l.turn_signal);
    }

    #[test]
    fn opencode_gets_its_server_through_the_environment() {
        let (l, _) = launch(&["opencode"], None);
        let cfg = l
            .env
            .iter()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, v)| serde_json::from_str::<serde_json::Value>(v).unwrap())
            .unwrap();
        assert_eq!(cfg["mcp"]["tower"]["type"], "local");
        assert_eq!(cfg["mcp"]["tower"]["command"][1], "mcp");
        let i = l.argv.iter().position(|a| a == "--prompt").unwrap();
        assert!(l.argv[i + 1].starts_with("Be careful.") && l.argv[i + 1].ends_with("Fix the bug"));
        assert!(!l.turn_signal);
    }

    #[test]
    fn antigravity_writes_its_mcp_file_only_where_tower_may() {
        let dir = std::env::temp_dir().join(format!("tower-agy-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(dir.join(".git").join("info")).unwrap();
        let (l, _) = launch(&["agy"], Some(&dir));
        assert!(l.argv.last().unwrap().ends_with("Fix the bug"));
        let file = dir.join(".agents/mcp_config.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["tower"]["args"][0], "mcp");
        let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap();
        assert!(exclude.lines().any(|l| l == ".agents/mcp_config.json"));
        // Again: no duplicate exclude line.
        let _ = launch(&["agy"], Some(&dir));
        let exclude = std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches("mcp_config").count(), 1);

        // A file the user wrote is never replaced.
        std::fs::write(&file, r#"{"mcpServers": {"theirs": {}}}"#).unwrap();
        let _ = launch(&["agy"], Some(&dir));
        assert!(std::fs::read_to_string(&file).unwrap().contains("theirs"));

        // Not a folder Tower made: nothing written at all.
        let user =
            std::env::temp_dir().join(format!("tower-agy-user-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&user).unwrap();
        let _ = launch(&["agy"], None);
        assert!(!user.join(".agents").exists());
    }

    #[test]
    fn a_pane_gets_the_daemons_path_and_a_role_server_may_override_it() {
        let path = std::env::var("PATH").unwrap_or_default();
        let (l, _) = launch(&["claude"], None);
        let got: Vec<&str> = l
            .env
            .iter()
            .filter(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(got, vec![path.as_str()], "the daemon's PATH, once");
        // Codex takes role servers' variables in its environment; a PATH
        // there comes later and so wins where the pane is started.
        let d = std::env::temp_dir().join(format!("tower-pathmcp-{}", crate::paths::random_u64()));
        let paths = Paths::new(d.join("towerd.sock"), Some(d.join("cfg")), None);
        let mut role = vendor_role(&["codex"]);
        role.mcp.insert(
            "tool".into(),
            crate::config::McpServerCfg {
                command: "tool-mcp".into(),
                args: vec![],
                env: [("PATH".to_string(), "/custom/bin".to_string())].into(),
            },
        );
        let l = prepare(
            &paths,
            &role,
            "a-2",
            "s",
            None,
            "T",
            "sys",
            "b",
            "tok",
            &[],
            &[],
            None,
        )
        .unwrap();
        let last = l
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str());
        assert_eq!(last, Some("/custom/bin"));
    }

    #[test]
    fn role_servers_reach_every_cli_and_secrets_stay_off_the_command_line() {
        let d = std::env::temp_dir().join(format!("tower-rmcp-{}", crate::paths::random_u64()));
        let paths = Paths::new(d.join("towerd.sock"), Some(d.join("cfg")), None);
        let mut role = vendor_role(&["claude"]);
        role.mcp.insert(
            "github".into(),
            crate::config::McpServerCfg {
                command: "gh-mcp".into(),
                args: vec!["stdio".into()],
                env: [("GH_TOKEN".to_string(), "ghp_secret".to_string())].into(),
            },
        );
        let go = |role: &RoleCfg, own: OwnDir| {
            prepare(
                &paths,
                role,
                "a-9",
                "s",
                None,
                "T",
                "sys",
                "b",
                "tok",
                &[],
                &[],
                own,
            )
            .unwrap()
        };
        // Claude: in its private mcp.json.
        let l = go(&role, None);
        assert!(!l.argv.join(" ").contains("ghp_secret"));
        let file = paths.agent_dir("a-9").join("mcp.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["github"]["command"], "gh-mcp");
        assert!(v["mcpServers"]["tower"].is_object());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "holds tokens");
        }
        // Codex: names on the command line, values in the environment.
        role.command = vec!["codex".into()];
        let l = go(&role, None);
        let joined = l.argv.join(" ");
        assert!(joined.contains("mcp_servers.github.command=\"gh-mcp\""));
        assert!(joined.contains("mcp_servers.github.env_vars=[\"GH_TOKEN\"]"));
        assert!(!joined.contains("ghp_secret"));
        assert!(l.env.contains(&("GH_TOKEN".into(), "ghp_secret".into())));
        // OpenCode: in its config content.
        role.command = vec!["opencode".into()];
        let l = go(&role, None);
        let cfg = &l
            .env
            .iter()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .unwrap()
            .1;
        let cfg: serde_json::Value = serde_json::from_str(cfg).unwrap();
        assert_eq!(cfg["mcp"]["github"]["command"][1], "stdio");
        assert_eq!(
            cfg["mcp"]["github"]["environment"]["GH_TOKEN"],
            "ghp_secret"
        );
    }

    #[test]
    fn gemini_and_unknown_clis() {
        let (g, _) = launch(&["gemini"], None);
        let i = g.argv.iter().position(|a| a == "-i").unwrap();
        assert!(g.argv[i + 1].ends_with("Fix the bug"));
        let (x, _) = launch(&["goose"], None);
        assert!(
            x.argv.last().unwrap().ends_with("goose"),
            "nothing appended"
        );
        assert!(x.type_brief.as_deref().unwrap().ends_with("Fix the bug"));
        assert!(!x.turn_signal);
    }
}
