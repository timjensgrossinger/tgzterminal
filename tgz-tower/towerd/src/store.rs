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
    /// Pane agents whose CLI reports no turn ends.
    #[serde(default)]
    pub no_turn_signal: std::collections::HashSet<String>,
    /// Updates for a busy Claude pane agent, passed after its next tool call.
    #[serde(default)]
    pub fyi: HashMap<String, Vec<String>>,
    /// Ladder rung each agent runs on.
    #[serde(default)]
    pub rungs: HashMap<String, usize>,
    /// Local handoffs waiting for their model to be free: (child flight,
    /// packet id), oldest first.
    #[serde(default)]
    pub local_queue: Vec<(String, String)>,
    /// Local results whose gate commands are running: child flight ->
    /// (sender, message). Re-run after a restart.
    #[serde(default)]
    pub local_checks: HashMap<String, (String, String)>,
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
    /// Vendor-terms note id -> the terms matrix version the user accepted.
    #[serde(default)]
    pub terms_acks: HashMap<String, u32>,
    /// Inbox id -> what Tower would do when the user says "run it".
    #[serde(default)]
    pub pending_actions: HashMap<String, PendingAction>,
    /// SLA cycles already acknowledged (or drafted, or skipped) -> when.
    #[serde(default)]
    pub acks: HashMap<String, u64>,
    /// SLA cycle -> the highest urgency already raised as a Deadline item.
    #[serde(default)]
    pub sla_warned: HashMap<String, u8>,
    /// Ids of comments Tower posted, so following an issue skips them.
    #[serde(default)]
    pub own_comments: Vec<String>,
    /// Flights whose issue has had a reply (stops Tower's own clock).
    #[serde(default)]
    pub answered: std::collections::HashSet<String>,
    /// Scout run flight -> the scout it runs.
    #[serde(default)]
    pub scout_flights: HashMap<String, String>,
    /// Flight id -> when its issue was created on the tracker.
    #[serde(default)]
    pub issue_created: HashMap<String, u64>,
    /// Schedule id -> when it fires and how its runs went.
    #[serde(default)]
    pub schedules: HashMap<String, ScheduleState>,
    /// Flight id -> the schedule that started it.
    #[serde(default)]
    pub flight_schedule: HashMap<String, String>,
    /// Activity counters per local day (`2026-10-24`).
    #[serde(default)]
    pub daily: crate::history::Daily,
    /// `daily` was rebuilt from the flights and the audit log once.
    #[serde(default)]
    pub history_backfilled: bool,
}

/// What Tower remembers about a schedule between runs.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct ScheduleState {
    #[serde(default)]
    pub next_fire: Option<u64>,
    #[serde(default)]
    pub last_fire: Option<u64>,
    /// When a run last landed or was proposed, for `{since}`.
    #[serde(default)]
    pub last_success: Option<u64>,
    /// Latest runs, oldest first, at most `RECENT_RUNS`.
    #[serde(default)]
    pub recent: Vec<tower_proto::RunMark>,
    #[serde(default)]
    pub fail_streak: u32,
    /// Set when Tower paused it; resuming clears it.
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// The `cron`/`every` that `next_fire` was planned from.
    #[serde(default)]
    pub planned_for: String,
}

pub const RECENT_RUNS: usize = 30;

/// Something the policy said to ask about instead of doing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingAction {
    /// Start an agent of `role` on `flight`.
    Launch { flight: String, role: String },
    /// Post an acknowledgement on the flight's issue.
    Ack {
        flight: String,
        text: String,
        #[serde(default)]
        internal: bool,
    },
    /// Ask the triage role which role `flight` needs, then maybe launch.
    Triage {
        flight: String,
        #[serde(default)]
        fallback: Option<String>,
    },
    /// Send text Tower wrote (a handoff result, gate output, an issue
    /// update, another agent's post) to a running agent.
    Message {
        agent: String,
        text: String,
        /// An update to pass along mid-turn rather than a message to act on.
        #[serde(default)]
        fyi: bool,
    },
    /// Run a scout (asked because `scout.run` is not automatic).
    Scout { name: String },
    /// Fire a schedule (asked because `schedule.run` is `confirm`).
    Schedule { id: String },
    /// Resume a schedule Tower paused after failed runs.
    ResumeSchedule { id: String },
    /// Accept a vendor-terms note, then run `then` if given.
    Terms {
        terms: String,
        #[serde(default)]
        then: Option<Box<PendingAction>>,
    },
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
    /// Jira: the watch's SLA field ids; `None` until found, empty when the
    /// project has none.
    #[serde(default)]
    pub sla_fields: Option<Vec<String>>,
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
