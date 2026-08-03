//! Claude Code source for the agent herd.
//!
//! Two on-disk surfaces are read, both of them **undocumented internals** of
//! Claude Code (observed on 2.1.220). Every field is treated as optional and
//! every parse failure degrades a single row rather than failing the view;
//! pane detection remains the floor if this whole module comes back empty.
//!
//! 1. `~/.claude/sessions/<pid>.json` — one file per live session, maintained
//!    by the session itself. Carries `status` (`busy` / `waiting` / `idle`),
//!    `waitingFor` (the block reason), `cwd`, and a human `name`. This is the
//!    only first-party live status signal available without installing hooks.
//!
//! 2. `~/.claude/projects/<encoded-cwd>/<sessionId>/subagents/agent-<id>.jsonl`
//!    plus a sibling `.meta.json`. Subagents run inside their parent's
//!    process, so this is the *only* way to see them at all.
//!
//! All of this runs on the overlay thread. Nothing here may touch the mux.

use super::{status_from_claude, subagent_status, ClaudeSession, HerdSubagent};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Why a project's log directory could not be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectDirError {
    Missing,
    /// Resolved outside the projects root — a symlinked project directory
    /// trying to escape. Refused.
    OutsideProjects,
    NotDirectory,
}

/// Collect every live Claude session, newest state first.
///
/// `home` is injectable so this is testable against a fixture tree.
pub fn collect_sessions(home: &Path, include_subagents: bool) -> Vec<ClaudeSession> {
    let sessions_dir = home.join(".claude").join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        match parse_session_file(&path) {
            Some(session) if process_is_alive(session.pid) => {
                let mut session = session;
                if include_subagents {
                    session.subagents = collect_subagents(home, &session.cwd, &session.session_id);
                }
                sessions.push(session);
            }
            // A file whose process is gone is a stale leftover; a file we
            // cannot parse tells us nothing. Either way, skip quietly — this
            // is a best-effort read of someone else's private format.
            _ => continue,
        }
    }
    sessions
}

/// Parse one `sessions/<pid>.json`. Returns `None` if the file is missing the
/// fields we cannot work without (`pid`, `sessionId`, `cwd`).
fn parse_session_file(path: &Path) -> Option<ClaudeSession> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;

    let pid = value.get("pid")?.as_u64()?;
    // A pid that doesn't fit u32 isn't a pid we can signal or match.
    if pid > u32::MAX as u64 {
        return None;
    }
    let pid = pid as u32;
    let session_id = value.get("sessionId")?.as_str()?.to_string();
    let cwd = PathBuf::from(value.get("cwd")?.as_str()?);

    let status_text = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let blocked_reason = value
        .get("waitingFor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let status = status_from_claude(status_text, blocked_reason.as_deref());

    Some(ClaudeSession {
        pid,
        session_id,
        project_root: super::project_root_for(&cwd),
        cwd,
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        status,
        blocked_reason,
        started_at: epoch_millis(value.get("startedAt")),
        status_changed_at: epoch_millis(value.get("statusUpdatedAt"))
            .or_else(|| epoch_millis(value.get("updatedAt"))),
        subagents: Vec::new(),
    })
}

fn epoch_millis(value: Option<&serde_json::Value>) -> Option<SystemTime> {
    let millis = value?.as_u64()?;
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(millis))
}

/// Is this pid still running?
///
/// `EPERM` counts as alive: the process exists, we just don't own it.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let pid = pid as i32;
    // Signal 0 performs error checking only; it never delivers a signal.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    // Claude Code's session registry is a unix-only surface today; without a
    // liveness check we would show stale sessions, so report none.
    false
}

/// Encode a working directory the way Claude Code names its project folders:
/// separators, colons **and dots** all become dashes.
///
/// The dot is easy to miss and load-bearing: a home directory like
/// `/Users/first.last` encodes to `-Users-first-last`, and a reader that keeps
/// the dot silently resolves nothing for every such user.
/// `sidebar.rs::encode_claude_project_path` still has that bug — dedupe onto
/// this function when the concurrent sidebar work lands.
pub fn encode_project_path(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '.' => '-',
            _ => ch,
        })
        .collect()
}

/// Resolve a project's log directory, refusing anything that escapes the
/// projects root.
///
/// The canonicalize-then-`starts_with` pair is the load-bearing part: without
/// it, a symlink placed at the encoded project name would let an attacker
/// point this reader at an arbitrary directory.
pub fn resolve_project_dir(home: &Path, cwd: &Path) -> Result<PathBuf, ProjectDirError> {
    let projects_root = home.join(".claude").join("projects");
    let project_dir = projects_root.join(encode_project_path(cwd));
    let root = projects_root
        .canonicalize()
        .map_err(|_| ProjectDirError::Missing)?;
    let path = project_dir
        .canonicalize()
        .map_err(|_| ProjectDirError::Missing)?;
    if !path.starts_with(&root) {
        return Err(ProjectDirError::OutsideProjects);
    }
    if !path.is_dir() {
        return Err(ProjectDirError::NotDirectory);
    }
    Ok(path)
}

/// Collect the subagents of one session.
pub fn collect_subagents(home: &Path, cwd: &Path, session_id: &str) -> Vec<HerdSubagent> {
    let Ok(project_dir) = resolve_project_dir(home, cwd) else {
        return Vec::new();
    };
    let subagents_dir = project_dir.join(session_id).join("subagents");
    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return Vec::new();
    };

    let now = SystemTime::now();
    let mut subagents = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Drive off the transcript, not the meta file: a transcript with no
        // meta is still a subagent worth showing.
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(agent_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let agent_id = agent_id.strip_prefix("agent-").unwrap_or(agent_id);
        subagents.push(read_subagent(&path, agent_id, now));
    }

    // Deepest last, then by description, so nesting reads top-down and the
    // order doesn't shuffle between refreshes.
    subagents.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.description.cmp(&b.description))
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });
    subagents
}

fn read_subagent(transcript: &Path, agent_id: &str, now: SystemTime) -> HerdSubagent {
    let meta = transcript.with_extension("meta.json");
    let meta = std::fs::read_to_string(&meta)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());

    let agent_type = meta
        .as_ref()
        .and_then(|m| m.get("agentType"))
        .and_then(|v| v.as_str())
        .unwrap_or("agent")
        .to_string();
    let description = meta
        .as_ref()
        .and_then(|m| m.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let depth = meta
        .as_ref()
        .and_then(|m| m.get("spawnDepth"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    let mtime = std::fs::metadata(transcript)
        .and_then(|meta| meta.modified())
        .ok();
    let (last_type, stop_reason) = match last_complete_line(transcript) {
        Some(line) => parse_tail_line(&line),
        None => (None, None),
    };

    HerdSubagent {
        agent_id: agent_id.to_string(),
        agent_type,
        description,
        status: subagent_status(last_type.as_deref(), stop_reason.as_deref(), mtime, now),
        depth,
        last_activity: mtime,
    }
}

/// Extract `type` and `message.stop_reason` from one JSONL line.
fn parse_tail_line(line: &str) -> (Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return (None, None);
    };
    let entry_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let stop_reason = value
        .get("message")
        .and_then(|m| m.get("stop_reason"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (entry_type, stop_reason)
}

/// Read the last line of a possibly-huge JSONL file.
///
/// Thin wrapper over [`super::transcript::tail_lines`], which owns the bounded
/// tail-reading machinery this and the activity reader both need.
pub fn last_complete_line(path: &Path) -> Option<String> {
    super::transcript::tail_lines(path, 1).pop()
}

/// Path to a session's own transcript.
///
/// It sits beside the `<sessionId>/` directory that holds the subagent
/// transcripts, not inside it.
pub fn session_transcript_path(home: &Path, cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let project_dir = resolve_project_dir(home, cwd).ok()?;
    let path = project_dir.join(format!("{session_id}.jsonl"));
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::super::transcript::{MAX_TAIL_WINDOW, TAIL_WINDOW};
    use super::super::HerdStatus;
    use super::*;
    use std::time::Duration;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn session_json(pid: u32, status: &str, waiting_for: Option<&str>, cwd: &str) -> String {
        let waiting = waiting_for
            .map(|w| format!(r#","waitingFor":"{w}""#))
            .unwrap_or_default();
        format!(
            r#"{{"pid":{pid},"sessionId":"sess-{pid}","cwd":"{cwd}","status":"{status}",
                "name":"agent-{pid}","startedAt":1785343234713,"statusUpdatedAt":1785344232946{waiting}}}"#
        )
    }

    #[test]
    fn encodes_project_paths_like_claude_does() {
        assert_eq!(
            encode_project_path(Path::new("/Users/me/Documents/repo")),
            "-Users-me-Documents-repo"
        );
        assert_eq!(
            encode_project_path(Path::new("C:\\src\\repo")),
            "C--src-repo"
        );
    }

    #[test]
    fn dots_in_the_path_become_dashes() {
        // Verified against a real tree: a home of /Users/tim.grossinger with a
        // dotted username, and a dotfile directory, both collapse to dashes.
        assert_eq!(
            encode_project_path(Path::new("/Users/tim.grossinger/Documents/tgzterminal")),
            "-Users-tim-grossinger-Documents-tgzterminal"
        );
        assert_eq!(
            encode_project_path(Path::new("/Users/tim.grossinger/.local/lib/TGs-router")),
            "-Users-tim-grossinger--local-lib-TGs-router"
        );
    }

    #[test]
    fn reads_a_live_session_with_its_block_reason() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp
                .path()
                .join(".claude/sessions")
                .join(format!("{me}.json")),
            &session_json(me, "waiting", Some("permission prompt"), "/repo"),
        );

        let sessions = collect_sessions(temp.path(), false);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, HerdStatus::Blocked);
        assert_eq!(
            sessions[0].blocked_reason.as_deref(),
            Some("permission prompt")
        );
        assert_eq!(
            sessions[0].name.as_deref(),
            Some(&format!("agent-{me}")[..])
        );
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert!(sessions[0].started_at.is_some());
    }

    #[test]
    fn a_session_whose_process_is_gone_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        // pid 1 exists but isn't ours; use an implausibly high pid instead so
        // the liveness check has something genuinely dead to reject.
        let dead = 0x7fff_fff0u32;
        write(
            &temp.path().join(".claude/sessions/dead.json"),
            &session_json(dead, "busy", None, "/repo"),
        );
        assert!(collect_sessions(temp.path(), false).is_empty());
    }

    #[test]
    fn malformed_and_irrelevant_files_are_skipped_not_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".claude/sessions");
        write(&dir.join("garbage.json"), "{not json at all");
        write(&dir.join("empty.json"), "");
        write(
            &dir.join("no-pid.json"),
            r#"{"sessionId":"x","cwd":"/repo"}"#,
        );
        write(&dir.join("notes.txt"), "ignored");

        assert!(collect_sessions(temp.path(), false).is_empty());
    }

    #[test]
    fn a_missing_sessions_directory_yields_nothing() {
        let temp = tempfile::tempdir().unwrap();
        assert!(collect_sessions(temp.path(), false).is_empty());
    }

    #[test]
    fn resolves_a_project_directory_under_the_projects_root() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/repo/here");
        let project = temp
            .path()
            .join(".claude/projects")
            .join(encode_project_path(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        assert_eq!(
            resolve_project_dir(temp.path(), &cwd).unwrap(),
            project.canonicalize().unwrap()
        );
    }

    #[test]
    fn a_missing_project_directory_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".claude/projects")).unwrap();
        assert_eq!(
            resolve_project_dir(temp.path(), Path::new("/nope")),
            Err(ProjectDirError::Missing)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_escaping_the_projects_root_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join(".claude/projects");
        std::fs::create_dir_all(&projects).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let cwd = PathBuf::from("/repo");
        std::os::unix::fs::symlink(&outside, projects.join(encode_project_path(&cwd))).unwrap();

        assert_eq!(
            resolve_project_dir(temp.path(), &cwd),
            Err(ProjectDirError::OutsideProjects)
        );
    }

    #[test]
    fn subagents_come_back_with_identity_and_status() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/repo");
        let subagents = temp
            .path()
            .join(".claude/projects")
            .join(encode_project_path(&cwd))
            .join("sess-1")
            .join("subagents");

        write(
            &subagents.join("agent-aaa.meta.json"),
            r#"{"agentType":"Explore","description":"map the sidebar","spawnDepth":1}"#,
        );
        write(
            &subagents.join("agent-aaa.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}\n",
        );

        let found = collect_subagents(temp.path(), &cwd, "sess-1");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_id, "aaa");
        assert_eq!(found[0].agent_type, "Explore");
        assert_eq!(found[0].description, "map the sidebar");
        assert_eq!(found[0].depth, 1);
        assert_eq!(found[0].status, HerdStatus::Done);
    }

    #[test]
    fn a_transcript_without_meta_still_appears() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/repo");
        let subagents = temp
            .path()
            .join(".claude/projects")
            .join(encode_project_path(&cwd))
            .join("sess-1")
            .join("subagents");
        write(&subagents.join("agent-bbb.jsonl"), "{\"type\":\"user\"}\n");

        let found = collect_subagents(temp.path(), &cwd, "sess-1");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_type, "agent");
        assert_eq!(found[0].description, "");
        // Just written, so it reads as active.
        assert_eq!(found[0].status, HerdStatus::Working);
    }

    #[test]
    fn tail_returns_the_last_complete_line() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        write(&path, "{\"a\":1}\n{\"a\":2}\n{\"a\":3}\n");
        assert_eq!(last_complete_line(&path).as_deref(), Some(r#"{"a":3}"#));
    }

    #[test]
    fn tail_ignores_a_trailing_partial_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        // No trailing newline: the writer is mid-append.
        write(&path, "{\"a\":1}\n{\"a\":2}\n{\"a\":3");
        // We hand back the fragment; parse_tail_line degrades it to Unknown
        // rather than pretending it parsed.
        let line = last_complete_line(&path).unwrap();
        assert_eq!(parse_tail_line(&line), (None, None));
    }

    #[test]
    fn tail_skips_the_leading_partial_line_of_a_large_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("big.jsonl");
        let filler = "x".repeat(2_000);
        let mut contents = String::new();
        // Enough lines to push the start of the window past the start of the
        // file, so the leading-partial-line trim is actually exercised.
        for i in 0..((TAIL_WINDOW / 1_000) + 8) {
            contents.push_str(&format!("{{\"n\":{i},\"pad\":\"{filler}\"}}\n"));
        }
        contents.push_str("{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"end_turn\"}}\n");
        write(&path, &contents);

        assert!(std::fs::metadata(&path).unwrap().len() > TAIL_WINDOW);
        let line = last_complete_line(&path).unwrap();
        // A complete, parsable line despite the window starting mid-file.
        assert_eq!(
            parse_tail_line(&line),
            (Some("assistant".to_string()), Some("end_turn".to_string()))
        );
    }

    #[test]
    fn tail_window_grows_past_a_final_line_bigger_than_the_first_window() {
        // This is the real-world shape that a fixed window got wrong: a
        // subagent's closing message is one JSONL line of a few hundred KB, so
        // the initial window contains no newline at all.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("long-tail.jsonl");
        let report = "z".repeat((TAIL_WINDOW as usize) * 3);
        write(
            &path,
            &format!(
                "{{\"type\":\"user\"}}\n{{\"type\":\"assistant\",\"pad\":\"{report}\",\"message\":{{\"stop_reason\":\"end_turn\"}}}}\n"
            ),
        );

        let line = last_complete_line(&path).unwrap();
        assert_eq!(
            parse_tail_line(&line),
            (Some("assistant".to_string()), Some("end_turn".to_string()))
        );
    }

    #[test]
    fn tail_of_a_line_longer_than_the_ceiling_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("pathological.jsonl");
        // One line past the ceiling: we stop growing rather than read an
        // unbounded file into memory, and report nothing rather than a
        // fragment that would parse as a dead agent.
        write(&path, &"y".repeat(MAX_TAIL_WINDOW as usize + 64));
        assert_eq!(last_complete_line(&path), None);
    }

    #[test]
    fn tail_of_a_whole_small_file_without_a_trailing_newline_is_returned() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("small.jsonl");
        write(&path, r#"{"type":"assistant"}"#);
        assert_eq!(
            last_complete_line(&path).as_deref(),
            Some(r#"{"type":"assistant"}"#)
        );
    }

    #[test]
    fn tail_of_an_empty_file_is_none() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("empty.jsonl");
        write(&path, "");
        assert_eq!(last_complete_line(&path), None);
    }

    #[test]
    fn tail_line_parses_type_and_stop_reason() {
        let (ty, stop) = parse_tail_line(
            r#"{"type":"assistant","isSidechain":true,"message":{"stop_reason":"end_turn"}}"#,
        );
        assert_eq!(ty.as_deref(), Some("assistant"));
        assert_eq!(stop.as_deref(), Some("end_turn"));

        let (ty, stop) = parse_tail_line(r#"{"type":"user"}"#);
        assert_eq!(ty.as_deref(), Some("user"));
        assert_eq!(stop, None);
    }

    #[test]
    fn epoch_millis_survives_absurd_values() {
        assert_eq!(epoch_millis(None), None);
        assert_eq!(epoch_millis(Some(&serde_json::json!("nope"))), None);
        assert_eq!(
            epoch_millis(Some(&serde_json::json!(1_000))),
            Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_000))
        );
    }

    /// Debug aid, not a CI test: dumps what this reader sees in the real home
    /// directory. Run with
    /// `cargo test -p wezterm-gui agent_herd -- --ignored --nocapture`
    /// when Claude Code's on-disk format seems to have drifted.
    #[test]
    #[ignore = "reads the real ~/.claude; output depends on what is running"]
    fn explain_real_home() {
        let Some(home) = dirs_next::home_dir() else {
            return;
        };
        for session in collect_sessions(&home, true) {
            println!(
                "{} pid={} {} {:?} cwd={}",
                session.name.as_deref().unwrap_or("<unnamed>"),
                session.pid,
                session.status.label(),
                session.blocked_reason,
                session.cwd.display()
            );
            for sub in &session.subagents {
                println!(
                    "    {} {} — {}",
                    sub.status.label(),
                    sub.agent_type,
                    sub.description
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn liveness_check_recognises_this_process_and_rejects_nonsense() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(0));
        assert!(!process_is_alive(0x7fff_fff0));
    }
}

/// Detector that reads Claude sessions from the filesystem.
pub struct ClaudeDetector;

impl crate::agent_herd::vendor::SessionSource for ClaudeDetector {
    fn vendor(&self) -> crate::agent_herd::vendor::AgentVendor {
        crate::agent_herd::vendor::AgentVendor::Claude
    }

    fn collect_sessions(
        &self,
        home: &std::path::Path,
    ) -> Vec<crate::agent_herd::vendor::VendorSession> {
        let claude_sessions = collect_sessions(home, true);
        claude_sessions
            .into_iter()
            .map(|s| crate::agent_herd::vendor::VendorSession {
                pid: s.pid,
                vendor: crate::agent_herd::vendor::AgentVendor::Claude,
                session_id: s.session_id,
                cwd: s.cwd,
                project_root: s.project_root,
                name: s.name,
                status: s.status,
                blocked_reason: s.blocked_reason,
                started_at: s.started_at,
                status_changed_at: s.status_changed_at,
                subagents: s.subagents,
            })
            .collect()
    }
}
