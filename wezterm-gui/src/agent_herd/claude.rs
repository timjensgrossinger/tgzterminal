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

/// `entrypoint` values that mean "a human is typing into this".
///
/// Anything else — `sdk-cli`, `sdk-py`, `sdk-ts`, hook children — is a process
/// spawned by a harness. Matching the allowed set rather than excluding the
/// known harnesses means a new harness name defaults to hidden.
const INTERACTIVE_ENTRYPOINTS: &[&str] = &["cli", "vscode", "jetbrains"];

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

    // `kind` is "interactive" even for SDK-spawned sessions, so `entrypoint`
    // is the discriminator: "cli" is a terminal a human types into, while
    // "sdk-cli" and friends are harness/hook children. An absent entrypoint
    // predates the field, so assume interactive rather than hiding the row.
    let interactive = match value.get("entrypoint").and_then(|v| v.as_str()) {
        None => true,
        Some(entrypoint) => INTERACTIVE_ENTRYPOINTS.contains(&entrypoint),
    };

    Some(ClaudeSession {
        pid,
        interactive,
        session_id,
        project_root: super::project_root_for(&cwd),
        cwd,
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        name_is_derived: value
            .get("nameSource")
            .and_then(|v| v.as_str())
            .map(|source| source == "derived")
            .unwrap_or(false),
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
pub(crate) fn process_is_alive(pid: u32) -> bool {
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
pub(crate) fn process_is_alive(_pid: u32) -> bool {
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
    // The last *message*, not the last line: a subagent transcript can pick up
    // trailing bookkeeping too, and reading that instead of the sign-off left
    // the subagent pinned at `Working` on transcript freshness alone.
    let (last_type, stop_reason) = match last_message_line(transcript) {
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

/// How many trailing JSONL records to search for the last real message.
///
/// Claude Code appends bookkeeping records after a turn ends -- `ai-title`,
/// `mode`, `atis-latch`, `last-prompt`, `file-history-snapshot`, and one
/// `attachment` per attached file -- and it appends some of them while the
/// session sits idle at its prompt. Reading only the final line therefore lands
/// on bookkeeping far more often than on the `end_turn` that is a few lines
/// above it, which is what made a finished session look like a working one.
const TURN_SEARCH_LINES: usize = 24;

/// The last line that carries a `message`, skipping bookkeeping records.
pub fn last_message_line(path: &Path) -> Option<String> {
    super::transcript::tail_lines(path, TURN_SEARCH_LINES)
        .into_iter()
        .rev()
        .find(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .is_some_and(|value| value.get("message").is_some())
        })
}

/// Tools that cannot complete without a human touching the keyboard.
///
/// Deliberately not every outstanding `tool_use`: a pending `Bash` looks
/// identical in the transcript whether it is executing or sitting behind a
/// permission prompt, so guessing there would pin working agents at `Blocked`.
/// These two are unambiguous -- neither can ever resolve on its own.
const CLAUDE_HUMAN_GATED_TOOLS: &[&str] = &["ExitPlanMode", "AskUserQuestion"];

/// Whether any `tool_use` block in this message names a human-gated tool.
///
/// Any, not the last: a message can carry several calls, and one of them
/// standing in front of the human is enough to hold the whole turn.
fn has_human_gated_tool_use(message: &serde_json::Value) -> bool {
    message
        .get("content")
        .and_then(|content| content.as_array())
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                    && block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .is_some_and(|name| CLAUDE_HUMAN_GATED_TOOLS.contains(&name))
            })
        })
}

/// What Claude's transcript says about whether the turn is over.
///
/// `end_turn` (and its rarer siblings) is the agent signing off: it has stopped
/// and is waiting for a human. A `tool_use` stop, or a `user` record carrying a
/// `tool_result`, is the middle of a turn. Anything else is not something to
/// guess from.
pub fn turn_state_from_transcript(path: &Path) -> super::TurnState {
    match last_message_line(path) {
        Some(line) => turn_state_from_line(&line),
        None => super::TurnState::Unknown,
    }
}

fn turn_state_from_line(line: &str) -> super::TurnState {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return super::TurnState::Unknown;
    };
    let Some(message) = value.get("message") else {
        return super::TurnState::Unknown;
    };
    match message.get("role").and_then(|v| v.as_str()) {
        Some("assistant") => match message.get("stop_reason").and_then(|v| v.as_str()) {
            Some("end_turn") | Some("stop_sequence") | Some("max_tokens") => {
                super::TurnState::Finished
            }
            // `tool_use` means a call is outstanding -- either a tool that is
            // running, or one that cannot finish until the human answers. Only
            // the latter means the agent is blocked, and no lookahead is needed
            // to tell whether it is still outstanding: the `tool_result` is
            // written as the very next record, so once it exists it *is* the
            // last message line and the `user` arm below claims it.
            Some("tool_use") if has_human_gated_tool_use(message) => {
                super::TurnState::AwaitingHuman
            }
            // A null stop reason means the record was written mid-stream.
            _ => super::TurnState::Working,
        },
        // A tool result being fed back, or a prompt just submitted: either way
        // the agent is about to be, or already is, busy.
        Some("user") => super::TurnState::Working,
        _ => super::TurnState::Unknown,
    }
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

/// The most informative name a session has.
///
/// `sessions/<pid>.json` carries a `name`, but with `nameSource: "derived"` it
/// is only a slug of the working directory plus a hash (`tgzterminal-72`) —
/// which repeats what the project column already shows. Claude's own `ai-title`
/// lives in the transcript instead, so a derived name is replaced by it and only
/// falls back when the transcript has neither a title nor a first prompt.
///
/// A name the user or the SDK actually set is never second-guessed.
fn session_name(
    transcript: Option<&Path>,
    name: Option<String>,
    name_is_derived: bool,
) -> Option<String> {
    if name.is_some() && !name_is_derived {
        return name;
    }
    transcript
        .and_then(super::sessions::claude_transcript_label)
        .or(name)
}

#[cfg(test)]
mod tests {
    use super::super::transcript::{MAX_TAIL_WINDOW, TAIL_WINDOW};
    use super::super::vendor::SessionSource;
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
    fn detector_attaches_transcript_activity_to_live_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let cwd = Path::new("/repo");
        write(
            &temp
                .path()
                .join(".claude/sessions")
                .join(format!("{me}.json")),
            &session_json(me, "busy", None, "/repo"),
        );
        let project = temp
            .path()
            .join(".claude/projects")
            .join(encode_project_path(cwd));
        write(
            &project.join(format!("sess-{me}.jsonl")),
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","id":"tool-1","input":{"command":"cargo check"}}]}}"#,
        );

        let sessions = ClaudeDetector.collect_sessions(temp.path());
        let activity = sessions[0].activity.as_ref().expect("activity");
        assert!(activity.current.is_some());
        assert!(activity
            .current
            .as_ref()
            .unwrap()
            .display_text()
            .contains("Bash"));
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

    /// The regression that made every finished Claude session look busy: the
    /// sign-off is not the last line, because bookkeeping records follow it --
    /// and some of them are written while the session sits idle.
    #[test]
    fn the_turn_boundary_is_found_behind_trailing_bookkeeping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write(
            &path,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\"}]}}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"end_turn\"}}\n\
             {\"type\":\"last-prompt\",\"sessionId\":\"x\"}\n\
             {\"type\":\"ai-title\",\"aiTitle\":\"something\"}\n\
             {\"type\":\"mode\",\"mode\":\"plan\"}\n\
             {\"type\":\"atis-latch\",\"atis\":true}\n",
        );
        assert_eq!(
            turn_state_from_transcript(&path),
            crate::agent_herd::TurnState::Finished,
            "four bookkeeping lines must not hide the end_turn above them"
        );
    }

    /// A question or a plan waiting to be approved is not a turn in progress:
    /// nothing moves until the human answers. Record shapes taken from a real
    /// `~/.claude/projects/<slug>/<session>.jsonl`.
    #[test]
    fn a_pending_human_gated_call_is_awaiting_the_human() {
        let dir = tempfile::tempdir().unwrap();

        for tool in ["AskUserQuestion", "ExitPlanMode"] {
            let path = dir.path().join(format!("{tool}.jsonl"));
            write(
                &path,
                &format!(
                    "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\
                     \"stop_reason\":\"tool_use\",\"content\":[{{\"type\":\"text\"}},\
                     {{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"{tool}\"}}]}}}}\n"
                ),
            );
            assert_eq!(
                turn_state_from_transcript(&path),
                crate::agent_herd::TurnState::AwaitingHuman,
                "a pending {tool} call is the human being the bottleneck"
            );
        }

        // Answered: the `tool_result` is written as the very next record, so it
        // becomes the last message line and the turn is moving again. This is
        // why no lookahead is needed.
        let answered = dir.path().join("answered.jsonl");
        write(
            &answered,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\
             \"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"AskUserQuestion\"}]}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\
             \"tool_use_id\":\"toolu_1\"}]}}\n",
        );
        assert_eq!(
            turn_state_from_transcript(&answered),
            crate::agent_herd::TurnState::Working
        );

        // An ordinary tool looks the same whether it is running or sitting
        // behind a permission prompt, so it stays `Working` and the screen
        // decides.
        let ordinary = dir.path().join("bash.jsonl");
        write(
            &ordinary,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\
             \"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"Bash\"}]}}\n",
        );
        assert_eq!(
            turn_state_from_transcript(&ordinary),
            crate::agent_herd::TurnState::Working
        );
    }

    #[test]
    fn a_turn_mid_tool_call_is_working() {
        let dir = tempfile::tempdir().unwrap();

        let pending = dir.path().join("pending.jsonl");
        write(
            &pending,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\"}}\n",
        );
        assert_eq!(
            turn_state_from_transcript(&pending),
            crate::agent_herd::TurnState::Working
        );

        // A tool result feeding back is still mid-turn.
        let feeding_back = dir.path().join("result.jsonl");
        write(
            &feeding_back,
            "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\"}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\"}]}}\n",
        );
        assert_eq!(
            turn_state_from_transcript(&feeding_back),
            crate::agent_herd::TurnState::Working
        );

        // Nothing readable: say so rather than guess either way.
        let bookkeeping_only = dir.path().join("meta.jsonl");
        write(&bookkeeping_only, "{\"type\":\"mode\",\"mode\":\"plan\"}\n");
        assert_eq!(
            turn_state_from_transcript(&bookkeeping_only),
            crate::agent_herd::TurnState::Unknown
        );
        assert_eq!(
            turn_state_from_transcript(&dir.path().join("absent.jsonl")),
            crate::agent_herd::TurnState::Unknown
        );
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
    fn a_derived_name_is_replaced_by_the_transcripts_own_title() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("sess-1.jsonl");
        write(
            &transcript,
            concat!(
                r#"{"type":"user","cwd":"/repo","message":{"role":"user","content":"hi"}}"#,
                "\n",
                r#"{"type":"ai-title","aiTitle":"Fix sidebar close hover"}"#,
                "\n"
            ),
        );

        // `nameSource: "derived"` means the name is only a slug of the cwd.
        assert_eq!(
            session_name(Some(&transcript), Some("tgzterminal-72".to_string()), true).as_deref(),
            Some("Fix sidebar close hover")
        );
        // A name the user or the SDK set is never second-guessed.
        assert_eq!(
            session_name(Some(&transcript), Some("release-prep".to_string()), false).as_deref(),
            Some("release-prep")
        );
    }

    #[test]
    fn a_derived_name_survives_a_transcript_with_no_title() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("sess-2.jsonl");
        // No `ai-title`, and the only user record is injected context.
        write(
            &transcript,
            concat!(
                r#"{"type":"user","isMeta":true,"cwd":"/repo","#,
                r#""message":{"role":"user","content":"<system>"}}"#,
                "\n"
            ),
        );

        assert_eq!(
            session_name(Some(&transcript), Some("tgzterminal-72".to_string()), true).as_deref(),
            Some("tgzterminal-72")
        );
        assert_eq!(session_name(None, None, false), None);
        assert_eq!(session_name(Some(&transcript), None, false), None);
    }

    #[test]
    fn name_source_is_read_off_the_session_file() {
        let temp = tempfile::tempdir().unwrap();
        let derived = temp.path().join("derived.json");
        write(
            &derived,
            r#"{"pid":1,"sessionId":"sess-1","cwd":"/repo","status":"idle",
                "name":"tgzterminal-72","nameSource":"derived"}"#,
        );
        assert!(parse_session_file(&derived).unwrap().name_is_derived);

        let explicit = temp.path().join("explicit.json");
        write(
            &explicit,
            r#"{"pid":2,"sessionId":"sess-2","cwd":"/repo","status":"idle",
                "name":"release-prep"}"#,
        );
        assert!(!parse_session_file(&explicit).unwrap().name_is_derived);
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
            .map(|s| {
                let activity = activity_for_session(home, &s);
                // Read before the fields below move out of `s`.
                let transcript = session_transcript_path(home, &s.cwd, &s.session_id);
                let turn = transcript
                    .as_deref()
                    .map(turn_state_from_transcript)
                    .unwrap_or_default();
                let name = session_name(transcript.as_deref(), s.name, s.name_is_derived);
                crate::agent_herd::vendor::VendorSession {
                    pid: s.pid,
                    interactive: s.interactive,
                    vendor: crate::agent_herd::vendor::AgentVendor::Claude,
                    session_id: s.session_id,
                    cwd: s.cwd,
                    project_root: s.project_root,
                    name,
                    model: None,
                    status: s.status,
                    // `sessions/<pid>.json` is the agent's own self-report and
                    // carries no way to tell a stale `busy` from a live one, so
                    // the transcript's turn boundary is what can contradict it.
                    turn,
                    blocked_reason: s.blocked_reason,
                    started_at: s.started_at,
                    status_changed_at: s.status_changed_at,
                    subagents: s.subagents,
                    activity,
                    input_tokens: None,
                    output_tokens: None,
                    cost: None,
                }
            })
            .collect()
    }
}

fn activity_for_session(home: &Path, session: &ClaudeSession) -> Option<super::HerdActivity> {
    let transcript = session_transcript_path(home, &session.cwd, &session.session_id)?;
    let project_dir = transcript.parent()?.to_path_buf();
    let mut activity = super::transcript::read_activity(&transcript, 8);
    super::sessions::populate_subagent_tree(
        &mut activity,
        &project_dir,
        &session.session_id,
        &session.subagents,
    );
    Some(activity)
}
