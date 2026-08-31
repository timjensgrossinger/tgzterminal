use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const AGY_ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
const AGY_WORKING_WINDOW: Duration = Duration::from_secs(2 * 60);

/// Root of the antigravity-cli store under `$HOME`.
fn antigravity_root(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli")
}

/// Path to one conversation's transcript. Antigravity names each
/// conversation's log directory after its own id, so this is a deterministic
/// join, matching what [`SessionSource::collect_sessions`] already builds.
pub(crate) fn transcript_path(home: &Path, session_id: &str) -> PathBuf {
    antigravity_root(home)
        .join("brain")
        .join(session_id)
        .join(".system_generated/logs/transcript.jsonl")
}

#[derive(serde::Deserialize)]
struct HistoryEntry {
    display: Option<String>,
    timestamp: Option<i64>,
    workspace: Option<PathBuf>,
    #[serde(rename = "conversationId")]
    conversation_id: Option<String>,
}

pub struct AntigravityDetector;

impl SessionSource for AntigravityDetector {
    fn vendor(&self) -> AgentVendor {
        AgentVendor::Antigravity
    }

    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession> {
        let root = antigravity_root(home);
        let last_path = root.join("cache/last_conversations.json");
        let Ok(last_text) = std::fs::read_to_string(last_path) else {
            return Vec::new();
        };
        let Ok(last) = serde_json::from_str::<HashMap<PathBuf, String>>(&last_text) else {
            return Vec::new();
        };

        let mut history_by_id: HashMap<String, HistoryEntry> = HashMap::new();
        // Newer agy builds sometimes omit `conversationId` from a history
        // entry — including, in practice, the newest line for a fresh
        // conversation. Keep the newest such entry per workspace so the
        // `last_conversations.json` id can still be matched by cwd.
        let mut fallback_by_workspace: HashMap<PathBuf, HistoryEntry> = HashMap::new();
        let history_path = root.join("history.jsonl");
        let Ok(history) = std::fs::read_to_string(history_path) else {
            return Vec::new();
        };
        for line in history.lines() {
            let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) else {
                continue;
            };
            match entry.conversation_id.clone() {
                Some(id) => {
                    let replace = history_by_id
                        .get(&id)
                        .and_then(|old| old.timestamp)
                        .unwrap_or_default()
                        <= entry.timestamp.unwrap_or_default();
                    if replace {
                        history_by_id.insert(id, entry);
                    }
                }
                None => {
                    let Some(workspace) = entry.workspace.clone() else {
                        continue;
                    };
                    let replace = fallback_by_workspace
                        .get(&workspace)
                        .and_then(|old| old.timestamp)
                        .unwrap_or_default()
                        <= entry.timestamp.unwrap_or_default();
                    if replace {
                        fallback_by_workspace.insert(workspace, entry);
                    }
                }
            }
        }

        let now = SystemTime::now();
        last.into_iter()
            .filter_map(|(cwd, session_id)| {
                let entry = history_by_id
                    .get(&session_id)
                    .or_else(|| fallback_by_workspace.get(&cwd))?;
                let updated_at = epoch_millis(entry.timestamp?)?;
                let age = now.duration_since(updated_at).ok()?;
                if age > AGY_ACTIVE_WINDOW {
                    return None;
                }
                let cwd = entry.workspace.clone().unwrap_or(cwd);
                if cwd.as_os_str().is_empty() {
                    return None;
                }
                let transcript = transcript_path(home, &session_id);
                let activity = transcript
                    .exists()
                    .then(|| {
                        crate::agent_herd::sessions::activity_from_session_files(
                            &transcript,
                            &root,
                            &session_id,
                        )
                    })
                    .flatten();
                Some(VendorSession {
                    // agy history has no process id. Herd binding falls back to
                    // a unique cwd match against the live pane.
                    pid: 0,
                    interactive: true,
                    vendor: AgentVendor::Antigravity,
                    // This store exposes no turn boundary; freshness is all it has.
                    turn: crate::agent_herd::TurnState::Unknown,
                    session_id,
                    cwd,
                    project_root: None,
                    name: entry
                        .display
                        .as_deref()
                        .filter(|display| !display.trim().is_empty())
                        .map(|display| crate::agent_herd::transcript::trim_to_words(display, 10)),
                    model: None,
                    status: if age <= AGY_WORKING_WINDOW {
                        HerdStatus::Working
                    } else {
                        HerdStatus::Idle
                    },
                    blocked_reason: None,
                    started_at: None,
                    status_changed_at: Some(updated_at),
                    subagents: Vec::new(),
                    activity,
                    input_tokens: None,
                    output_tokens: None,
                    cost: None,
                })
            })
            .collect()
    }
}

fn epoch_millis(value: i64) -> Option<SystemTime> {
    let millis = u64::try_from(value).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn recent_agy_conversation_provides_identity() {
        let temp = tempfile::tempdir().unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let id = "conversation-live";
        write(
            &temp
                .path()
                .join(".gemini/antigravity-cli/cache/last_conversations.json"),
            &format!(r#"{{"/repo":"{id}"}}"#),
        );
        write(
            &temp.path().join(".gemini/antigravity-cli/history.jsonl"),
            &format!(
                r#"{{"display":"Fix sidebar actions","timestamp":{now},"workspace":"/repo","conversationId":"{id}"}}"#
            ),
        );

        let sessions = AntigravityDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, id);
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert_eq!(sessions[0].name.as_deref(), Some("Fix sidebar actions"));
        assert_eq!(sessions[0].status, HerdStatus::Working);
    }

    #[test]
    fn history_entry_without_conversation_id_falls_back_to_workspace() {
        // Newer agy builds sometimes omit `conversationId` — including for the
        // newest line of a fresh conversation. The `last_conversations.json` id
        // must still resolve through a workspace match, or the session vanishes.
        let temp = tempfile::tempdir().unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let id = "conversation-live";
        write(
            &temp
                .path()
                .join(".gemini/antigravity-cli/cache/last_conversations.json"),
            &format!(r#"{{"/repo":"{id}"}}"#),
        );
        write(
            &temp.path().join(".gemini/antigravity-cli/history.jsonl"),
            // No entry carries the conversation id at all.
            &format!(r#"{{"display":"hello","timestamp":{now},"workspace":"/repo"}}"#),
        );

        let sessions = AntigravityDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, id);
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert_eq!(sessions[0].name.as_deref(), Some("hello"));
    }

    #[test]
    fn workspace_fallback_loses_to_id_match() {
        // When both an id match and a workspace fallback exist, the id match
        // must win even if its entry is older.
        let temp = tempfile::tempdir().unwrap();
        let now: u128 = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let id = "conversation-live";
        write(
            &temp
                .path()
                .join(".gemini/antigravity-cli/cache/last_conversations.json"),
            &format!(r#"{{"/repo":"{id}"}}"#),
        );
        write(
            &temp.path().join(".gemini/antigravity-cli/history.jsonl"),
            &format!(
                r#"{{"display":"older but identified","timestamp":{now},"workspace":"/other","conversationId":"{id}"}}
{{"display":"newest but anonymous","timestamp":{},"workspace":"/repo"}}"#,
                now + 1000
            ),
        );

        let sessions = AntigravityDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name.as_deref(), Some("older but identified"));
        assert_eq!(sessions[0].cwd, PathBuf::from("/other"));
    }
}
