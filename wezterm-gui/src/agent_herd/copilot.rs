use crate::agent_herd::claude::process_is_alive;
use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const COPILOT_ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
const COPILOT_WORKING_WINDOW: Duration = Duration::from_secs(2 * 60);

fn copilot_sessions_dir(home: &Path) -> PathBuf {
    home.join(".copilot")
}

/// Root of the `<session_id>/` directories searched by
/// [`collect_state_sessions`], reused by [`session_events_path`] so a
/// transcript lookup by session id agrees with detection.
fn session_state_root(home: &Path) -> PathBuf {
    home.join(".copilot").join("session-state")
}

/// Path to one session's event log. Unlike Codex's rollout files, Copilot
/// names each session directory after its session id, so this is a
/// deterministic join rather than a search.
pub(crate) fn session_events_path(home: &Path, session_id: &str) -> PathBuf {
    session_state_root(home)
        .join(session_id)
        .join("events.jsonl")
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

pub struct CopilotDetector;

impl SessionSource for CopilotDetector {
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Copilot
    }

    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession> {
        let mut sessions = collect_state_sessions(home);
        if !sessions.is_empty() {
            return sessions;
        }

        // Older builds wrote one JSON record directly under ~/.copilot.
        let dir = copilot_sessions_dir(home);
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
                        vendor: AgentVendor::Copilot,
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
                        activity: None,
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

fn collect_state_sessions(home: &Path) -> Vec<VendorSession> {
    let root = session_state_root(home);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(session_id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let workspace = dir.join("workspace.yaml");
        let events = session_events_path(home, session_id);
        let Ok(modified) = std::fs::metadata(&events)
            .or_else(|_| std::fs::metadata(&workspace))
            .and_then(|metadata| metadata.modified())
        else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age > COPILOT_ACTIVE_WINDOW {
            continue;
        }
        let Ok(workspace_text) = std::fs::read_to_string(&workspace) else {
            continue;
        };
        let Ok(metadata) = serde_yaml::from_str::<CopilotWorkspace>(&workspace_text) else {
            continue;
        };
        let Some(cwd) = metadata.cwd.or(metadata.git_root) else {
            continue;
        };
        let activity =
            crate::agent_herd::sessions::activity_from_session_files(&events, &root, session_id);
        sessions.push(VendorSession {
            // Copilot session state has no process id. Herd binding falls back
            // to a unique cwd match against the live pane.
            pid: 0,
            interactive: true,
            vendor: AgentVendor::Copilot,
            session_id: session_id.to_string(),
            cwd,
            project_root: None,
            name: metadata.name,
            model: None,
            status: if age <= COPILOT_WORKING_WINDOW {
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
    sessions
}

#[derive(serde::Deserialize)]
struct CopilotWorkspace {
    cwd: Option<PathBuf>,
    git_root: Option<PathBuf>,
    name: Option<String>,
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
    fn recent_session_state_provides_identity() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".copilot/session-state/session-live");
        write(
            &dir.join("workspace.yaml"),
            "cwd: /repo\nname: Fix sidebar actions\n",
        );
        write(&dir.join("events.jsonl"), "{\"type\":\"session.start\"}\n");

        let sessions = CopilotDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-live");
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert_eq!(sessions[0].name.as_deref(), Some("Fix sidebar actions"));
        assert_eq!(sessions[0].status, HerdStatus::Working);
    }

    #[test]
    fn a_session_whose_process_is_gone_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        // Implausibly high pid: something genuinely dead to reject.
        let dead = 0x7fff_fff0u32;
        write(
            &temp.path().join(".copilot").join("dead.json"),
            &session_json(dead),
        );
        assert!(CopilotDetector.collect_sessions(temp.path()).is_empty());
    }

    #[test]
    fn a_session_whose_process_is_alive_is_returned() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp.path().join(".copilot").join("live.json"),
            &session_json(me),
        );
        let sessions = CopilotDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, me);
    }
}
