//! Pane driver: a real interactive CLI (Claude Code) in a terminal pane.
//!
//! towerd only prepares the launch; the Tower view spawns the pane next to
//! itself and reports the pane id back. Status comes from Claude Code hooks
//! (`tower hook`), team tools from the `tower mcp` server.

use crate::config::RoleCfg;
use crate::paths::Paths;
use serde_json::json;
use std::path::Path;

pub struct PaneLaunch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

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
) -> std::io::Result<PaneLaunch> {
    let dir = paths.agent_dir(agent_id);
    std::fs::create_dir_all(&dir)?;
    let settings = dir.join("settings.json");
    let mcp = dir.join("mcp.json");
    std::fs::write(
        &settings,
        serde_json::to_vec_pretty(&settings_json(&paths.exe, deny))?,
    )?;
    std::fs::write(
        &mcp,
        serde_json::to_vec_pretty(&mcp_json(&paths.exe, agent_id, &paths.socket, token))?,
    )?;

    // The shim needs a POSIX shell; on Windows the agent starts directly
    // (no sidebar role name there yet).
    let mut argv = if cfg!(windows) {
        vec![]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            SHIM.to_string(),
            "tower-agent".to_string(),
        ]
    };
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
    argv.extend(extra.iter().cloned());
    // `--mcp-config` takes several values; `--` ends options so the brief is
    // always the prompt, even when it starts with a dash.
    argv.push("--".to_string());
    argv.push(brief.to_string());

    Ok(PaneLaunch {
        argv,
        env: vec![
            ("TOWER_AGENT_ID".into(), agent_id.into()),
            ("TOWER_SOCK".into(), paths.socket.display().to_string()),
            ("TOWER_ROLE".into(), role.name.clone()),
            ("TOWER_AGENT_TOKEN".into(), token.into()),
        ],
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
        )
        .unwrap();
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
}
