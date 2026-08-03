use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use std::path::{Path, PathBuf};

fn codex_sessions_dir(home: &Path) -> PathBuf {
    home.join(".codex")
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
        let dir = codex_sessions_dir(home);
        let files = session_files(&dir);
        let mut sessions = Vec::new();
        for file in files {
            if let Ok(data) = std::fs::read_to_string(&file) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    let pid = json.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);
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
                        pid: pid.unwrap_or(0),
                        vendor: AgentVendor::Codex,
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
