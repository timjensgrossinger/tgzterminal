use crate::agent_herd::claude::process_is_alive;
use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CODEX_ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
const CODEX_WORKING_WINDOW: Duration = Duration::from_secs(2 * 60);

fn codex_sessions_dir(home: &Path) -> PathBuf {
    home.join(".codex")
}

/// Root of the nested `YYYY/MM/DD/rollout-*.jsonl` tree searched by
/// [`collect_rollout_sessions`] and reused here so a transcript lookup by
/// session id walks the same tree rather than re-deriving the path.
fn rollout_sessions_root(home: &Path) -> PathBuf {
    home.join(".codex").join("sessions")
}

/// Path to one session's rollout transcript, if any file under the rollout
/// tree has `session_id` in its name. Rollout files are timestamp-prefixed
/// (`rollout-2026-07-03T16-25-16-<session_id>.jsonl`), so there is no
/// deterministic path to construct — this is a genuine search, reusing the
/// same walk `activity_from_session_files` already does for live activity.
pub(crate) fn find_transcript_path(home: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() {
        return None;
    }
    crate::agent_herd::sessions::find_session_artifact(&rollout_sessions_root(home), session_id)
}

fn session_files(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "json"))
        .collect()
}

pub struct CodexDetector;

impl SessionSource for CodexDetector {
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Codex
    }

    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession> {
        let mut sessions = collect_rollout_sessions(home);
        if !sessions.is_empty() {
            return sessions;
        }

        // Older builds wrote one JSON record directly under ~/.codex.
        let dir = codex_sessions_dir(home);
        let files = session_files(&dir);
        sessions = Vec::new();
        for file in files {
            if let Ok(data) = std::fs::read_to_string(&file) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    let pid = match json.get("pid").and_then(|v| v.as_u64()) {
                        Some(pid) if pid <= u32::MAX as u64 => pid as u32,
                        // No usable pid means we cannot verify liveness; skip
                        // this session rather than show a phantom row.
                        _ => continue,
                    };
                    if !process_is_alive(pid) {
                        continue;
                    }
                    let session_id = json
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cwd = json
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dir.clone());
                    let name = json
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let activity = crate::agent_herd::sessions::activity_from_session_files(
                        &file,
                        &dir.join("sessions"),
                        &session_id,
                    );
                    let status = json
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(|s| match s {
                            "busy" | "running" | "thinking" => Some(HerdStatus::Working),
                            "idle" | "done" => Some(HerdStatus::Idle),
                            "waiting" => Some(HerdStatus::Blocked),
                            _ => None,
                        })
                        .unwrap_or(HerdStatus::Unknown);
                    sessions.push(VendorSession {
                        pid,
                        // This store does not distinguish harness-spawned
                        // sessions from interactive ones.
                        interactive: true,
                        vendor: AgentVendor::Codex,
                        session_id,
                        cwd,
                        project_root: None,
                        name,
                        model: None,
                        status,
                        blocked_reason: None,
                        started_at: None,
                        status_changed_at: None,
                        subagents: Vec::new(),
                        activity,
                        input_tokens: None,
                        output_tokens: None,
                        cost: None,
                    });
                }
            }
        }
        sessions
    }
}

fn collect_rollout_sessions(home: &Path) -> Vec<VendorSession> {
    let root = rollout_sessions_root(home);
    let mut dirs = vec![root.clone()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            next.extend(
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir()),
            );
        }
        dirs = next;
    }

    let now = SystemTime::now();
    let mut sessions = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };
            if age > CODEX_ACTIVE_WINDOW {
                continue;
            }
            let Some((session_id, cwd, name)) = read_rollout_details(&path) else {
                continue;
            };
            let activity =
                crate::agent_herd::sessions::activity_from_session_files(&path, &root, &session_id);
            sessions.push(VendorSession {
                // Rollout metadata has no process id. Herd binding falls back
                // to a unique cwd match against the live pane.
                pid: 0,
                interactive: true,
                vendor: AgentVendor::Codex,
                session_id,
                cwd,
                project_root: None,
                name,
                model: None,
                status: if age <= CODEX_WORKING_WINDOW {
                    HerdStatus::Working
                } else {
                    HerdStatus::Idle
                },
                blocked_reason: None,
                started_at: None,
                status_changed_at: Some(modified),
                subagents: Vec::new(),
                activity,
                input_tokens: None,
                output_tokens: None,
                cost: None,
            });
        }
    }
    sessions
}

fn read_rollout_details(path: &Path) -> Option<(String, PathBuf, Option<String>)> {
    let mut session_id = None;
    let mut cwd = None;
    let mut name = None;
    for line in
        crate::agent_herd::transcript::head_lines(path, crate::agent_herd::transcript::HEAD_LINES)
    {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if cwd.is_none() {
            cwd = payload
                .get("cwd")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
        }
        if session_id.is_none() {
            session_id = payload
                .get("session_id")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        if name.is_none()
            && payload.get("type").and_then(|value| value.as_str()) == Some("user_message")
        {
            name = payload
                .get("message")
                .and_then(|value| value.as_str())
                .and_then(crate::agent_herd::transcript::describe_prompt);
        }
        if cwd.is_some() && session_id.is_some() && name.is_some() {
            break;
        }
    }
    Some((session_id?, cwd?, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn session_json(pid: u32) -> String {
        format!(r#"{{"pid":{pid},"session_id":"sess-{pid}","cwd":"/repo","status":"busy"}}"#)
    }

    #[test]
    fn recent_nested_rollout_provides_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join(".codex/sessions/2026/08/16/rollout-live.jsonl");
        write(
            &path,
            &format!(
                "{}\n{}\n",
                r#"{"type":"session_meta","payload":{"session_id":"sess-live","cwd":"/repo"}}"#,
                r#"{"type":"response_item","payload":{"type":"user_message","message":"fix sidebar actions"}}"#
            ),
        );

        let sessions = CodexDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "sess-live");
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert_eq!(sessions[0].name.as_deref(), Some("fix sidebar actions"));
        assert_eq!(sessions[0].status, HerdStatus::Working);
    }

    #[test]
    fn a_session_whose_process_is_gone_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        // Implausibly high pid: something genuinely dead to reject.
        let dead = 0x7fff_fff0u32;
        write(
            &temp.path().join(".codex").join("dead.json"),
            &session_json(dead),
        );
        assert!(CodexDetector.collect_sessions(temp.path()).is_empty());
    }

    #[test]
    fn a_session_whose_process_is_alive_is_returned() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp.path().join(".codex").join("live.json"),
            &session_json(me),
        );
        let sessions = CodexDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, me);
    }

    #[test]
    fn activity_is_read_from_codex_rollout_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp.path().join(".codex").join("live.json"),
            &session_json(me),
        );
        write(
            &temp
                .path()
                .join(".codex/sessions")
                .join(format!("rollout-sess-{me}.jsonl")),
            r#"{"type":"function_call","name":"shell","arguments":{"command":"cargo check"}}"#,
        );

        let sessions = CodexDetector.collect_sessions(temp.path());
        assert!(sessions[0]
            .activity
            .as_ref()
            .and_then(|activity| activity.current.as_ref())
            .is_some());
    }
}
