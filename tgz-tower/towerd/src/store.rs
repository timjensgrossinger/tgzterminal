//! Persisted state: one JSON file, written atomically.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tower_proto::{Agent, Flight, Frequency, InboxItem};

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Counters {
    pub flight: u32,
    pub agent: u32,
    pub inbox: u32,
    pub handoff: u32,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Persisted {
    pub flights: Vec<Flight>,
    pub agents: Vec<Agent>,
    pub inbox: Vec<InboxItem>,
    pub frequencies: Vec<Frequency>,
    pub counters: Counters,
    /// Messages waiting for an agent to become idle.
    #[serde(default)]
    pub queues: HashMap<String, Vec<String>>,
    /// Ladder rung each agent runs on.
    #[serde(default)]
    pub rungs: HashMap<String, usize>,
    /// Claude session id -> agent id, to re-attach hooks after a restart.
    #[serde(default)]
    pub sessions: HashMap<String, String>,
    /// Intake watch name -> its polling state.
    #[serde(default)]
    pub sources: HashMap<String, SourceState>,
    /// Flight id -> the intake watch it came from.
    #[serde(default)]
    pub flight_source: HashMap<String, String>,
    /// Inbox id -> the Jira comment it would post.
    #[serde(default)]
    pub pending_comments: HashMap<String, String>,
    /// Per-agent token -> agent id, for the agents' hook/MCP shims.
    #[serde(default)]
    pub agent_tokens: HashMap<String, String>,
    /// "Auto" flights waiting for a worktree that should launch once triaged.
    #[serde(default)]
    pub auto_launch: std::collections::HashSet<String>,
    /// The learned routing policy.
    #[serde(default)]
    pub bandit: tower_core::bandit::Bandit,
    /// Flight id -> routing decisions not yet scored by an outcome.
    #[serde(default)]
    pub decisions: HashMap<String, Vec<Decision>>,
    /// Local git repos found under `[projects] roots`.
    #[serde(default)]
    pub repos: Vec<crate::projects::RepoInfo>,
    #[serde(default)]
    pub repos_scanned: u64,
    /// `jira:ACME` / `github:owner/repo` -> project path you chose.
    #[serde(default)]
    pub learned_projects: HashMap<String, String>,
    /// Inbox id -> (flight, project choices) for "Which project?" questions.
    #[serde(default)]
    pub pending_projects: HashMap<String, (String, Vec<String>)>,
    /// Inbox id -> flight, for "Open a draft PR?" clearances.
    #[serde(default)]
    pub pending_prs: HashMap<String, String>,
    /// Flights in auto-fix mode.
    #[serde(default)]
    pub autofix: std::collections::HashSet<String>,
    /// Flight id -> the issue it came from.
    #[serde(default)]
    pub flight_issue: HashMap<String, IssueRef>,
    /// Auto-fix flights waiting for the first project scan.
    #[serde(default)]
    pub awaiting_scan: std::collections::HashSet<String>,
    /// Flight id -> its draft PR/MR.
    #[serde(default)]
    pub pr_urls: HashMap<String, String>,
    /// Inbox id -> (agent, question, option labels) for tower_decide.
    #[serde(default)]
    pub pending_decisions: HashMap<String, (String, String, Vec<String>)>,
    /// Flight id -> what Tower has seen of its issue (comments, text, status).
    #[serde(default)]
    pub issue_seen: HashMap<String, IssueSeen>,
    /// Inbox id -> (flight, update text) for "resume with this update".
    #[serde(default)]
    pub pending_updates: HashMap<String, (String, String)>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct IssueSeen {
    pub comments: Vec<String>,
    pub summary: String,
    pub description_hash: u64,
    pub status: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct IssueRef {
    /// Jira project key, `owner/repo` or `group/project`.
    pub project: String,
    #[serde(default)]
    pub number: Option<u64>,
}

/// One engine choice, kept until its flight lands or is given up.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Decision {
    /// `tier:engine` that ran.
    pub arm: String,
    pub features: Vec<f64>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct SourceState {
    /// Issue keys already turned into flights (newest last).
    pub seen: Vec<String>,
    pub last_poll: u64,
    pub error: Option<String>,
    pub flights: u32,
}

pub fn load(path: &Path) -> Persisted {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            // Keep the unreadable file for inspection rather than losing it.
            let bak = path.with_extension("json.bad");
            let _ = std::fs::rename(path, &bak);
            eprintln!(
                "tower: state unreadable ({err}); moved to {}",
                bak.display()
            );
            Persisted::default()
        }),
        Err(_) => Persisted::default(),
    }
}

pub fn save(path: &Path, state: &Persisted) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(state)?;
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_garbage() {
        let d = std::env::temp_dir().join(format!("tower-store-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("state.json");
        assert_eq!(
            load(&p),
            Persisted::default(),
            "missing file is empty state"
        );
        let mut s = Persisted::default();
        s.counters.flight = 7;
        s.queues.insert("a".into(), vec!["hi".into()]);
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        std::fs::write(&p, "{not json").unwrap();
        assert_eq!(load(&p), Persisted::default());
        assert!(d.join("state.json.bad").exists());
    }
}
