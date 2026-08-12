use crate::agent_herd::claude::process_is_alive;
use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use std::path::{Path, PathBuf};

fn opencode_sessions_dir(home: &Path) -> PathBuf {
    home.join(".config").join("opencode")
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

pub struct OpenCodeDetector;

impl SessionSource for OpenCodeDetector {
    fn vendor(&self) -> AgentVendor {
        AgentVendor::OpenCode
    }

    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession> {
        let dir = opencode_sessions_dir(home);
        let files = session_files(&dir);
        let mut sessions = Vec::new();
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
                        vendor: AgentVendor::OpenCode,
                        session_id,
                        cwd,
                        project_root: None,
                        name,
                        status,
                        blocked_reason: None,
                        started_at: None,
                        status_changed_at: None,
                        subagents: Vec::new(),
                    });
                }
            }
        }
        sessions
    }
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
    fn a_session_whose_process_is_gone_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        // Implausibly high pid: something genuinely dead to reject.
        let dead = 0x7fff_fff0u32;
        write(
            &temp
                .path()
                .join(".config")
                .join("opencode")
                .join("dead.json"),
            &session_json(dead),
        );
        assert!(OpenCodeDetector.collect_sessions(temp.path()).is_empty());
    }

    #[test]
    fn a_session_whose_process_is_alive_is_returned() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp
                .path()
                .join(".config")
                .join("opencode")
                .join("live.json"),
            &session_json(me),
        );
        let sessions = OpenCodeDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, me);
    }
}
