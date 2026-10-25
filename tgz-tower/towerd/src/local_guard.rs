//! Keeping agent CLIs you start yourself out of local-only work.
//!
//! - `tower guard prompt` / `tower guard tool`: Claude Code hooks that
//!   "Connect agents" installs user-wide. In a local-only folder every prompt
//!   is blocked before it reaches the model, and so is a prompt that names a
//!   local-only file (`@path` puts the file into the prompt without a tool
//!   call). Anywhere, a tool call that reaches into a local-only folder or a
//!   sensitive path is denied.
//! - `tower guard dir <folder>`: exits 3 with the reason when the folder is
//!   local-only. The sidebar asks this before it launches an agent there.
//! - `stop_cloud_cli_here`: Codex has no such hook, but it starts Tower's
//!   MCP server before its first prompt; started in a local-only folder, the
//!   server ends it.
//!
//! When the settings cannot be read (a `tower.toml` that does not parse),
//! every check answers "local-only": Tower cannot tell which folders are,
//! so it does not guess "none". The hook command itself also blocks when
//! the guard cannot run at all (see `connect::GUARD_HOOKS`).
//!
//! All of this is best effort against a CLI that wants to get round it (a
//! shell can always hide a path); the hard rule is that Tower never starts a
//! cloud model on local-only work itself.
//!
//! Nothing here talks to towerd: it reads `tower.toml [privacy]` and the
//! repo's `.tower.toml` directly, so it works while the daemon is down.

use crate::privacy::{self, PrivacyCfg};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Exit code of `tower guard dir` for a local-only folder.
pub const LOCAL_ONLY_EXIT: i32 = 3;

/// The settings, or why they cannot be read.
type Settings = Result<PrivacyCfg, String>;

fn settings() -> Settings {
    crate::config::load_privacy(&crate::paths::default_config_dir())
}

/// Why work in `dir` must stay local, if it must.
pub fn dir_reason(dir: &Path, global: &Settings) -> Option<String> {
    let global = match global {
        Ok(g) => g,
        Err(e) => return Some(e.clone()),
    };
    let repo = crate::config::repo_privacy(dir);
    privacy::folder_reason(dir, global, &repo)
}

/// May `cli` (a program name) run in a local-only folder anyway?
pub fn allowed(cli: &str, global: &Settings) -> bool {
    global.as_ref().is_ok_and(|g| {
        g.allow_clis
            .iter()
            .any(|c| c.trim().eq_ignore_ascii_case(cli))
    })
}

fn cwd_of(payload: &Value) -> PathBuf {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand(p: &str, cwd: &Path) -> PathBuf {
    let p = p.trim();
    if p == "~" || p.starts_with("~/") {
        return crate::config::expand_home(p);
    }
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Words of `text` that may name a file: anything with a `/`, starting with
/// `.` or `~`, or a bare name that exists in `cwd` (`cd payroll`).
fn path_words(text: &str, cwd: &Path) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || "\"'`;|&<>()=,".contains(c))
        .map(|w| {
            w.trim_start_matches('@')
                .trim_end_matches([':', '.', '!', '?'])
        })
        .filter(|w| !w.is_empty() && !w.starts_with('-'))
        .filter(|w| {
            w.contains('/') || w.starts_with('.') || w.starts_with('~') || cwd.join(w).exists()
        })
        .map(str::to_string)
        .collect()
}

/// Every string in a JSON value, for MCP tools whose arguments Tower does
/// not know.
fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Paths a tool call may reach: its file arguments, glob patterns, and in
/// a shell command (or an unknown tool's arguments) every word that looks
/// like a path.
pub fn tool_paths(payload: &Value) -> Vec<String> {
    let cwd = cwd_of(payload);
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let mut out = vec![];
    for k in ["file_path", "path", "notebook_path", "pattern", "glob"] {
        if let Some(p) = input.get(k).and_then(Value::as_str) {
            // A glob reaches the folder its fixed part names.
            let fixed = p.split(['*', '?', '[', '{']).next().unwrap_or(p);
            if !fixed.trim().is_empty() {
                out.push(fixed.to_string());
            }
        }
    }
    if let Some(cmd) = input.get("command").and_then(Value::as_str) {
        out.extend(path_words(cmd, &cwd));
    }
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    if tool.starts_with("mcp__") {
        let mut all = vec![];
        strings(&input, &mut all);
        for s in all {
            out.extend(path_words(&s, &cwd));
        }
    }
    out
}

/// The first of `paths` that is in a local-only folder or a sensitive path,
/// with why.
fn first_blocked(paths: &[String], cwd: &Path, global: &Settings) -> Option<String> {
    for p in paths {
        let abs = expand(p, cwd);
        let dir = if abs.is_dir() {
            abs.clone()
        } else {
            abs.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| cwd.to_path_buf())
        };
        if let Some(w) = dir_reason(&dir, global) {
            return Some(format!("{p} is in a local-only folder ({w})"));
        }
        let globs = crate::config::repo_privacy(&dir).globs;
        if privacy::is_sensitive(&abs.display().to_string(), cwd, &globs) {
            return Some(format!("{p} is a sensitive path"));
        }
    }
    None
}

/// `UserPromptSubmit`: block every prompt in a local-only folder, and one
/// that names a local-only file anywhere.
pub fn decide_prompt(payload: &Value, global: &Settings) -> Option<Value> {
    if allowed("claude", global) {
        return None;
    }
    let cwd = cwd_of(payload);
    let why = match dir_reason(&cwd, global) {
        Some(w) => format!("this folder is local-only ({w})"),
        None => {
            let prompt = payload.get("prompt").and_then(Value::as_str).unwrap_or("");
            let named: Vec<String> = prompt
                .split_whitespace()
                .filter(|w| w.starts_with('@'))
                .flat_map(|w| path_words(w, &cwd))
                .collect();
            first_blocked(&named, &cwd, global)?
        }
    };
    Some(json!({
        "decision": "block",
        "reason": format!(
            "Tower: {why}. Nothing was sent. Do this work with Tower's Local role (a model inside your network)."
        ),
    }))
}

/// `PreToolUse`: deny a tool call that reaches into a local-only folder or
/// a sensitive path.
pub fn decide_tool(payload: &Value, global: &Settings) -> Option<Value> {
    if allowed("claude", global) {
        return None;
    }
    let cwd = cwd_of(payload);
    let why = match dir_reason(&cwd, global) {
        Some(w) => format!("this folder is local-only ({w})"),
        None => first_blocked(&tool_paths(payload), &cwd, global)?,
    };
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "Tower: {why}. Only models inside the user's network may see it. Do not try another way to read it; if the task needs it, hand that part to a local model (tower_delegate with local_only) or tell the user."
            )
        }
    }))
}

/// `tower guard prompt|tool|dir <folder> [<cli>]`.
pub fn run(args: &[String]) -> anyhow::Result<()> {
    let global = settings();
    match args.first().map(String::as_str) {
        Some("dir") => {
            let dir = args
                .get(1)
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let cli = args.get(2).map(String::as_str).unwrap_or("");
            if let Some(why) = dir_reason(&dir, &global).filter(|_| !allowed(cli, &global)) {
                println!("{why}");
                std::process::exit(LOCAL_ONLY_EXIT);
            }
            Ok(())
        }
        Some(kind @ ("prompt" | "tool")) => {
            let mut input = String::new();
            let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
            let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
            let out = if kind == "prompt" {
                decide_prompt(&payload, &global)
            } else {
                decide_tool(&payload, &global)
            };
            if let Some(v) = out {
                let mut stdout = std::io::stdout().lock();
                let _ = writeln!(stdout, "{v}");
                let _ = stdout.flush();
            }
            Ok(())
        }
        _ => {
            eprintln!("usage: tower guard prompt|tool|dir [<folder> [<cli>]]");
            std::process::exit(2);
        }
    }
}

/// Is process `pid` still running? Unknown (not Unix) counts as running,
/// so nothing is ever dropped by mistake.
pub fn process_alive(pid: u32) -> bool {
    if !cfg!(unix) || pid == 0 {
        return true;
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

fn parent_pid() -> Option<u32> {
    #[cfg(unix)]
    return Some(std::os::unix::process::parent_id());
    #[cfg(not(unix))]
    return None;
}

/// `(parent pid, command line)` of `pid`, from `ps`.
fn proc_info(pid: u32) -> Option<(u32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=,args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (ppid, args) = line.split_once(char::is_whitespace)?;
    Some((ppid.trim().parse().ok()?, args.trim().to_string()))
}

/// Is this command line Codex? Its binary (`codex`, `codex-aarch64-apple-
/// darwin`) or the npm package's script run by node. Never a shell, never
/// an IDE.
pub fn is_codex(args: &str) -> bool {
    let mut words = args.split_whitespace();
    let prog = words.next().unwrap_or("");
    let name = prog
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prog)
        .to_lowercase();
    if name == "codex" || name.starts_with("codex-") {
        return true;
    }
    (name == "node" || name.starts_with("node"))
        && words
            .next()
            .is_some_and(|script| script.contains("@openai/codex") || script.ends_with("/codex.js"))
}

/// The folder Codex works in: its `-C`/`--cd` argument, else `cwd`.
fn codex_dir(args: &str, cwd: &Path) -> PathBuf {
    let words: Vec<&str> = args.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(v) = w.strip_prefix("--cd=") {
            return expand(v, cwd);
        }
        if (*w == "-C" || *w == "--cd") && i + 1 < words.len() {
            return expand(words[i + 1], cwd);
        }
    }
    cwd.to_path_buf()
}

/// Called by `tower mcp` when it was not launched by Tower: if the CLI that
/// started it is Codex (looked for up to three processes up, since npm and
/// node wrappers sit in between) and works in a local-only folder, end it
/// before its first prompt and say why on its terminal. Claude has its own
/// hook; nothing else connects to Tower. Returns whether it did.
pub fn stop_cloud_cli_here() -> bool {
    let Some(mut pid) = parent_pid() else {
        return false;
    };
    let mut codex = None;
    for _ in 0..3 {
        let Some((ppid, args)) = proc_info(pid) else {
            break;
        };
        if is_codex(&args) {
            codex = Some((pid, args));
            break;
        }
        if ppid <= 1 {
            break;
        }
        pid = ppid;
    }
    let Some((pid, args)) = codex else {
        return false;
    };
    let global = settings();
    if allowed("codex", &global) {
        return false;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let why = dir_reason(&cwd, &global).or_else(|| dir_reason(&codex_dir(&args, &cwd), &global));
    let Some(why) = why else {
        return false;
    };
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    let mut gone = false;
    for _ in 0..20 {
        if !process_alive(pid) {
            gone = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let text = if gone {
        format!("\r\nTower: this folder is local-only ({why}). Codex sends what it reads to OpenAI, so it was stopped before its first prompt. Use Tower's Local role here.\r\n")
    } else {
        format!("\r\nTower: this folder is local-only ({why}), but Codex could not be stopped. Do not use it here; quit it now.\r\n")
    };
    if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        let _ = tty.write_all(text.as_bytes());
    }
    eprintln!("{}", text.trim());
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("tower-guard-{name}-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn global_with(paths: Vec<String>) -> Settings {
        Ok(PrivacyCfg {
            local_only_paths: paths,
            ..Default::default()
        })
    }

    #[test]
    fn prompts_in_or_about_local_only_folders_are_blocked() {
        let d = tmp("prompt");
        let payroll = d.join("payroll");
        std::fs::create_dir_all(&payroll).unwrap();
        std::fs::write(payroll.join("salaries.csv"), "x").unwrap();
        let global = global_with(vec![payroll.display().to_string()]);
        let inside = json!({"cwd": payroll.join("src").display().to_string(), "prompt": "fix it"});
        let out = decide_prompt(&inside, &global).unwrap();
        assert_eq!(out["decision"], "block");
        assert!(out["reason"].as_str().unwrap().contains("local-only"));
        let outside = json!({"cwd": d.display().to_string(), "prompt": "hello"});
        assert!(decide_prompt(&outside, &global).is_none());
        let mention = json!({"cwd": d.display().to_string(), "prompt": "summarise @payroll/salaries.csv please"});
        assert!(
            decide_prompt(&mention, &global).is_some(),
            "@file is in the prompt"
        );
        let opted_out: Settings = Ok(PrivacyCfg {
            allow_clis: vec!["claude".into()],
            ..global.clone().unwrap()
        });
        assert!(decide_prompt(&inside, &opted_out).is_none());
        // Unreadable settings: block rather than guess "nothing is local".
        let broken: Settings = Err("tower.toml does not parse".into());
        assert!(decide_prompt(&outside, &broken).is_some());
    }

    #[test]
    fn tools_cannot_reach_into_local_only_folders_or_sensitive_paths() {
        let d = tmp("tool");
        let payroll = d.join("payroll");
        let repo = d.join("app");
        std::fs::create_dir_all(&payroll).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join(".tower.toml"),
            "[privacy]\nsensitive_paths = [\"secrets/**\"]\n",
        )
        .unwrap();
        let global = global_with(vec![payroll.display().to_string()]);
        let call = |cwd: &Path, tool: &str, input: Value| json!({"cwd": cwd.display().to_string(), "tool_name": tool, "tool_input": input});
        let denied = |v: Option<Value>| {
            v.map(|v| v["hookSpecificOutput"]["permissionDecision"] == "deny")
                .unwrap_or(false)
        };
        let deny = |cwd: &Path, tool: &str, input: Value| {
            denied(decide_tool(&call(cwd, tool, input), &global))
        };
        let p = payroll.display().to_string();
        assert!(deny(
            &repo,
            "Read",
            json!({"file_path": format!("{p}/salaries.csv")})
        ));
        assert!(deny(&repo, "Read", json!({"file_path": "secrets/db.key"})));
        assert!(deny(
            &repo,
            "Bash",
            json!({"command": "cat ./src/../secrets/db.key | head"})
        ));
        assert!(
            deny(&d, "Bash", json!({"command": "cd payroll && cat *"})),
            "bare folder name"
        );
        assert!(deny(
            &repo,
            "Glob",
            json!({"pattern": format!("{p}/**/*.csv")})
        ));
        assert!(deny(
            &repo,
            "mcp__fs__read_file",
            json!({"args": {"target": format!("{p}/x")}})
        ));
        assert!(!deny(&repo, "Read", json!({"file_path": "src/main.rs"})));
        assert!(!deny(&repo, "Bash", json!({"command": "cargo test --all"})));
    }

    #[test]
    fn codex_is_recognised_by_its_command_line_and_nothing_else() {
        assert!(is_codex("/opt/homebrew/bin/codex"));
        assert!(is_codex("codex-aarch64-apple-darwin --cd /x"));
        assert!(is_codex(
            "node /usr/local/lib/node_modules/@openai/codex/bin/codex.js"
        ));
        assert!(!is_codex("-zsh"));
        assert!(!is_codex("/bin/bash -l"));
        assert!(!is_codex("node /usr/lib/gemini-cli/index.js"));
        assert!(!is_codex(
            "/Applications/Antigravity.app/Contents/MacOS/Antigravity"
        ));
        assert!(!is_codex("claude"));
        let cwd = Path::new("/home/me");
        assert_eq!(
            codex_dir("codex -C /w/payroll", cwd),
            PathBuf::from("/w/payroll")
        );
        assert_eq!(
            codex_dir("codex --cd=sub", cwd),
            PathBuf::from("/home/me/sub")
        );
        assert_eq!(codex_dir("codex", cwd), PathBuf::from("/home/me"));
    }

    #[test]
    fn a_process_is_alive_until_it_is_gone() {
        assert!(process_alive(std::process::id()));
        if cfg!(unix) {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            assert!(!process_alive(pid));
        }
    }
}
