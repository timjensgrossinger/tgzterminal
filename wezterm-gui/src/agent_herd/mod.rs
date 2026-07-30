//! Vendor-neutral "agent herd" model: every AI agent this terminal can see,
//! together with its subagents, its status, and the pane that owns it.
//!
//! Two very different producers feed this model, which is why it deliberately
//! contains no GUI, mux or terminal types:
//!
//! - Filesystem sources (see [`claude`]) run on the overlay thread. They can
//!   see agents this terminal does not own, and they can see subagents, which
//!   run inside their parent process and are therefore invisible to any
//!   process-based detection.
//! - Pane sources run on the GUI thread, because they need the mux. They are
//!   the floor: every vendor gets a row from pane detection even when no
//!   filesystem source knows about it.
//!
//! [`join_sessions_with_panes`] stitches the two together. Everything else
//! here is pure and unit tested.

pub mod claude;

use mux::pane::PaneId;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long after its last write a subagent transcript still counts as active.
const SUBAGENT_ACTIVITY_WINDOW: Duration = Duration::from_secs(10);

/// sRGB color for "this agent is waiting on you".
///
/// Matches the amber the sidebar already uses for waiting badges. Named here so
/// there is one place to change it; `sidebar.rs` still carries two hardcoded
/// copies of the same literal that should adopt this (see the note in
/// `docs/AGENT_QUEUE_PLAN.md` about sourcing it from the palette instead).
pub const ATTENTION_RGB: (u8, u8, u8) = (240, 184, 66);

/// Coarse agent state, shared across vendors.
///
/// Variant order is display and sort order: the thing that needs you comes
/// first, the thing you can ignore comes last. `derive(Ord)` makes that the
/// sort key directly, so don't reorder without checking [`sort_agents`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HerdStatus {
    /// Waiting on the human: permission prompt, elicitation, sandbox request.
    Blocked,
    /// Actively working.
    Working,
    /// Alive and waiting for a prompt.
    Idle,
    /// Finished while you weren't looking, and you haven't looked yet.
    /// Derived by the view, never reported by a source.
    Done,
    Unknown,
}

impl HerdStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Unknown => "unknown",
        }
    }

    /// Status dot. `Done` shares `Working`'s glyph and is distinguished by
    /// being drawn dim, mirroring how the sidebar dims exited panes.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Blocked => "◐",
            Self::Working | Self::Done => "●",
            Self::Idle => "○",
            Self::Unknown => "·",
        }
    }

    /// True when the agent is doing something that Ctrl-C would meaningfully
    /// interrupt. Mirrors the toolbelt's Stop gate.
    pub fn is_interruptible(self) -> bool {
        matches!(self, Self::Working | Self::Blocked)
    }
}

/// Map Claude Code's `~/.claude/sessions/<pid>.json` `status` field.
///
/// The vocabulary is an undocumented internal, so an unrecognised value must
/// never be treated as "fine": a `waitingFor` reason alongside an unknown
/// status still means the agent is blocked on the human, which is the one
/// state we cannot afford to miss.
pub fn status_from_claude(status: &str, waiting_for: Option<&str>) -> HerdStatus {
    match status.trim().to_ascii_lowercase().as_str() {
        "waiting" => HerdStatus::Blocked,
        "busy" | "running" | "thinking" | "compacting" => HerdStatus::Working,
        "idle" => HerdStatus::Idle,
        "done" => HerdStatus::Done,
        _ if waiting_for.is_some() => HerdStatus::Blocked,
        _ => HerdStatus::Unknown,
    }
}

/// Infer a subagent's status from the tail of its transcript.
///
/// `last_type` / `stop_reason` come from the last complete JSONL line;
/// `mtime` is the transcript's last write. The parent's `Task` tool result is
/// deliberately not consulted — it is pinned at `"async_launched"` for the
/// life of the session and never updated on completion.
pub fn subagent_status(
    last_type: Option<&str>,
    stop_reason: Option<&str>,
    mtime: Option<SystemTime>,
    now: SystemTime,
) -> HerdStatus {
    if last_type == Some("assistant") && stop_reason == Some("end_turn") {
        return HerdStatus::Done;
    }
    let recently_written = mtime
        .and_then(|mtime| now.duration_since(mtime).ok())
        .map(|age| age < SUBAGENT_ACTIVITY_WINDOW)
        .unwrap_or(false);
    if recently_written {
        return HerdStatus::Working;
    }
    // Quiet, but never signed off: the transcript was truncated, the agent
    // died, or it is between turns. Claiming either "working" or "done" here
    // would be a guess, so say so.
    HerdStatus::Unknown
}

/// A subagent: a `Task` agent running inside its parent's process.
#[derive(Clone, Debug, PartialEq)]
pub struct HerdSubagent {
    pub agent_id: String,
    /// Subagent type, e.g. `"Explore"`.
    pub agent_type: String,
    pub description: String,
    pub status: HerdStatus,
    /// Nesting depth; 1 for a subagent spawned by the top-level session.
    pub depth: u32,
    pub last_activity: Option<SystemTime>,
}

/// One agent in the herd.
#[derive(Clone, Debug, PartialEq)]
pub struct HerdAgent {
    /// Human-facing name: the session's own name, else the pane title.
    pub name: String,
    /// Adapter id: `"claude"`, `"codex"`, … Empty when wholly unidentified.
    pub provider: String,
    pub status: HerdStatus,
    /// Why it is blocked, when the source tells us.
    pub blocked_reason: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Repo root, used as the grouping key.
    pub project_root: Option<PathBuf>,
    pub git_branch: Option<String>,
    pub pid: Option<u32>,
    /// The pane that owns this agent. `None` means we could not bind it to a
    /// pane, so it cannot be stopped or focused from here.
    pub pane_id: Option<PaneId>,
    pub session_id: Option<String>,
    pub started_at: Option<SystemTime>,
    pub status_changed_at: Option<SystemTime>,
    pub subagents: Vec<HerdSubagent>,
}

impl HerdAgent {
    /// Grouping label: repo directory name, else cwd name, else a placeholder.
    pub fn project_label(&self) -> String {
        self.project_root
            .as_deref()
            .or(self.cwd.as_deref())
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "—".to_string())
    }

    /// Stop is only offered when we know which pane to send Ctrl-C to and
    /// there is something to interrupt.
    pub fn can_stop(&self) -> bool {
        self.pane_id.is_some() && self.status.is_interruptible()
    }
}

/// A Claude session as read from `~/.claude/sessions/<pid>.json`, before it is
/// joined to a pane.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeSession {
    pub pid: u32,
    pub session_id: String,
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub name: Option<String>,
    pub status: HerdStatus,
    pub blocked_reason: Option<String>,
    pub started_at: Option<SystemTime>,
    pub status_changed_at: Option<SystemTime>,
    pub subagents: Vec<HerdSubagent>,
}

/// An agent pane as detected on the GUI thread.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneAgentRow {
    pub pane_id: PaneId,
    /// Adapter id from pane detection, when it identified one.
    pub provider: Option<String>,
    pub title: String,
    pub status: HerdStatus,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    pub git_branch: Option<String>,
    /// Every pid in this pane's foreground process tree, used to bind a
    /// filesystem-discovered session to this pane.
    pub pids: HashSet<u32>,
}

/// Join filesystem-discovered Claude sessions to detected panes.
///
/// Binding is by pid first (exact, via the pane's process tree), then by cwd
/// as a fallback — but only when the cwd match is *unique*. Two agents of the
/// same vendor in the same directory are genuinely ambiguous, and guessing
/// would point Stop at the wrong pane; such a session stays unbound instead.
pub fn join_sessions_with_panes(
    sessions: Vec<ClaudeSession>,
    panes: Vec<PaneAgentRow>,
) -> Vec<HerdAgent> {
    let mut sessions = sessions;
    // Oldest first, so binding is deterministic regardless of readdir order.
    sessions.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.pid.cmp(&b.pid))
    });

    let mut claimed: HashSet<PaneId> = HashSet::new();
    let mut agents = Vec::with_capacity(sessions.len() + panes.len());

    for session in sessions {
        let bound = bind_by_pid(&session, &panes, &claimed)
            .or_else(|| bind_by_cwd(&session, &panes, &claimed));
        if let Some(pane_id) = bound {
            claimed.insert(pane_id);
        }
        let pane = bound.and_then(|id| panes.iter().find(|row| row.pane_id == id));

        agents.push(HerdAgent {
            name: session
                .name
                .clone()
                .or_else(|| pane.map(|p| p.title.clone()))
                .unwrap_or_else(|| session.session_id.clone()),
            provider: "claude".to_string(),
            status: session.status,
            blocked_reason: session.blocked_reason.clone(),
            // The session file carries no model; pane detection sometimes does.
            model: pane.and_then(|p| p.model.clone()),
            project_root: session
                .project_root
                .clone()
                .or_else(|| pane.and_then(|p| p.project_root.clone())),
            cwd: Some(session.cwd.clone()),
            git_branch: pane.and_then(|p| p.git_branch.clone()),
            pid: Some(session.pid),
            pane_id: bound,
            session_id: Some(session.session_id.clone()),
            started_at: session.started_at,
            status_changed_at: session.status_changed_at,
            subagents: session.subagents,
        });
    }

    // Panes no session accounted for: every non-Claude vendor, plus any Claude
    // pane whose session file we could not read.
    for row in panes {
        if claimed.contains(&row.pane_id) {
            continue;
        }
        agents.push(HerdAgent {
            name: row.title.clone(),
            provider: row.provider.clone().unwrap_or_default(),
            status: row.status,
            blocked_reason: None,
            model: row.model.clone(),
            cwd: row.cwd.clone(),
            project_root: row.project_root.clone(),
            git_branch: row.git_branch.clone(),
            pid: None,
            pane_id: Some(row.pane_id),
            session_id: row.session_id.clone(),
            started_at: None,
            status_changed_at: None,
            subagents: Vec::new(),
        });
    }

    agents
}

fn bind_by_pid(
    session: &ClaudeSession,
    panes: &[PaneAgentRow],
    claimed: &HashSet<PaneId>,
) -> Option<PaneId> {
    panes
        .iter()
        .find(|row| !claimed.contains(&row.pane_id) && row.pids.contains(&session.pid))
        .map(|row| row.pane_id)
}

fn bind_by_cwd(
    session: &ClaudeSession,
    panes: &[PaneAgentRow],
    claimed: &HashSet<PaneId>,
) -> Option<PaneId> {
    let mut candidates = panes.iter().filter(|row| {
        !claimed.contains(&row.pane_id)
            && row.cwd.as_deref() == Some(session.cwd.as_path())
            && matches!(row.provider.as_deref(), None | Some("claude"))
    });
    let first = candidates.next()?;
    // Ambiguous: refuse to guess rather than aim Stop at the wrong pane.
    if candidates.next().is_some() {
        return None;
    }
    Some(first.pane_id)
}

/// Which agents the overview shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HerdView {
    /// Only agents belonging to the active pane's project.
    CurrentProject,
    /// Everything, grouped by project.
    AllGrouped,
}

impl HerdView {
    pub fn toggled(self) -> Self {
        match self {
            Self::CurrentProject => Self::AllGrouped,
            Self::AllGrouped => Self::CurrentProject,
        }
    }
}

/// A project's worth of agents.
#[derive(Clone, Debug, PartialEq)]
pub struct HerdGroup {
    pub label: String,
    /// Header is only worth drawing when more than one group is on screen.
    pub show_header: bool,
    pub agents: Vec<HerdAgent>,
}

/// Filter and group agents for display.
///
/// In `CurrentProject` view an agent belongs to the current project when its
/// project root matches, or — for an agent with no discoverable root — when
/// its cwd sits inside the current project.
pub fn group_by_project(
    agents: Vec<HerdAgent>,
    view: HerdView,
    current_project: Option<&Path>,
) -> Vec<HerdGroup> {
    match view {
        HerdView::CurrentProject => {
            let mut agents: Vec<HerdAgent> = match current_project {
                Some(root) => agents
                    .into_iter()
                    .filter(|agent| belongs_to_project(agent, root))
                    .collect(),
                // No project context: showing nothing would look broken, so
                // fall back to the full list.
                None => agents,
            };
            sort_agents(&mut agents);
            let label = current_project
                .and_then(|root| root.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .or_else(|| agents.first().map(|agent| agent.project_label()))
                .unwrap_or_else(|| "—".to_string());
            vec![HerdGroup {
                label,
                show_header: false,
                agents,
            }]
        }
        HerdView::AllGrouped => {
            let mut groups: Vec<HerdGroup> = Vec::new();
            for agent in agents {
                let label = agent.project_label();
                match groups.iter_mut().find(|group| group.label == label) {
                    Some(group) => group.agents.push(agent),
                    None => groups.push(HerdGroup {
                        label,
                        show_header: true,
                        agents: vec![agent],
                    }),
                }
            }
            for group in &mut groups {
                sort_agents(&mut group.agents);
            }
            // Projects that need attention float up; ties by name so the list
            // doesn't reshuffle under the cursor between refreshes.
            groups.sort_by(|a, b| {
                let a_key = a
                    .agents
                    .first()
                    .map(|x| x.status)
                    .unwrap_or(HerdStatus::Unknown);
                let b_key = b
                    .agents
                    .first()
                    .map(|x| x.status)
                    .unwrap_or(HerdStatus::Unknown);
                a_key.cmp(&b_key).then_with(|| a.label.cmp(&b.label))
            });
            groups
        }
    }
}

fn belongs_to_project(agent: &HerdAgent, root: &Path) -> bool {
    if let Some(agent_root) = agent.project_root.as_deref() {
        return agent_root == root;
    }
    agent
        .cwd
        .as_deref()
        .map(|cwd| cwd.starts_with(root))
        .unwrap_or(false)
}

/// Attention first, then stable by name so refreshes don't move rows around.
fn sort_agents(agents: &mut [HerdAgent]) {
    agents.sort_by(|a, b| {
        a.status
            .cmp(&b.status)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.pid.cmp(&b.pid))
    });
}

/// Walk up from `dir` looking for a repo root.
///
/// Handles the `.git`-as-a-file case so linked worktrees resolve to their own
/// checkout rather than the parent repo — worktrees are separate spaces.
///
/// TODO: once the concurrent `sidebar.rs` work lands, share that file's
/// `find_git_branch` / `parse_git_head` instead of keeping two walkers.
pub fn project_root_for(dir: &Path) -> Option<PathBuf> {
    let mut dir = dir;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(pane_id: PaneId, provider: &str, pids: &[u32]) -> PaneAgentRow {
        PaneAgentRow {
            pane_id,
            provider: Some(provider.to_string()),
            title: format!("pane-{pane_id}"),
            status: HerdStatus::Working,
            model: None,
            session_id: None,
            cwd: Some(PathBuf::from("/repo")),
            project_root: Some(PathBuf::from("/repo")),
            git_branch: None,
            pids: pids.iter().copied().collect(),
        }
    }

    fn session(pid: u32, name: &str, cwd: &str) -> ClaudeSession {
        ClaudeSession {
            pid,
            session_id: format!("session-{pid}"),
            cwd: PathBuf::from(cwd),
            project_root: Some(PathBuf::from(cwd)),
            name: Some(name.to_string()),
            status: HerdStatus::Working,
            blocked_reason: None,
            started_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(pid as u64)),
            status_changed_at: None,
            subagents: Vec::new(),
        }
    }

    fn agent(name: &str, status: HerdStatus, root: &str) -> HerdAgent {
        HerdAgent {
            name: name.to_string(),
            provider: "claude".to_string(),
            status,
            blocked_reason: None,
            model: None,
            cwd: Some(PathBuf::from(root)),
            project_root: Some(PathBuf::from(root)),
            git_branch: None,
            pid: None,
            pane_id: Some(1),
            session_id: None,
            started_at: None,
            status_changed_at: None,
            subagents: Vec::new(),
        }
    }

    #[test]
    fn claude_status_vocabulary_maps_to_herd_status() {
        assert_eq!(status_from_claude("busy", None), HerdStatus::Working);
        assert_eq!(status_from_claude("idle", None), HerdStatus::Idle);
        assert_eq!(
            status_from_claude("waiting", Some("permission prompt")),
            HerdStatus::Blocked
        );
        assert_eq!(status_from_claude("thinking", None), HerdStatus::Working);
        assert_eq!(status_from_claude("compacting", None), HerdStatus::Working);
        assert_eq!(status_from_claude("BUSY", None), HerdStatus::Working);
    }

    #[test]
    fn unknown_claude_status_with_a_reason_still_reads_blocked() {
        // The status vocabulary is an undocumented internal; a block reason is
        // the signal we must not drop when the vocabulary drifts.
        assert_eq!(
            status_from_claude("hibernating", Some("input needed")),
            HerdStatus::Blocked
        );
        assert_eq!(status_from_claude("hibernating", None), HerdStatus::Unknown);
        assert_eq!(status_from_claude("", None), HerdStatus::Unknown);
    }

    #[test]
    fn signed_off_subagent_transcript_is_done() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(
            subagent_status(Some("assistant"), Some("end_turn"), None, now),
            HerdStatus::Done
        );
    }

    #[test]
    fn recently_written_subagent_transcript_is_working() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mtime = now - Duration::from_secs(2);
        assert_eq!(
            subagent_status(Some("user"), None, Some(mtime), now),
            HerdStatus::Working
        );
        assert_eq!(
            subagent_status(Some("assistant"), Some("tool_use"), Some(mtime), now),
            HerdStatus::Working
        );
    }

    #[test]
    fn quiet_unsigned_subagent_transcript_is_unknown_not_done() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let stale = now - Duration::from_secs(600);
        assert_eq!(
            subagent_status(Some("user"), None, Some(stale), now),
            HerdStatus::Unknown
        );
        // Unparsable tail must degrade, never panic or claim completion.
        assert_eq!(subagent_status(None, None, None, now), HerdStatus::Unknown);
    }

    #[test]
    fn mtime_in_the_future_does_not_panic() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let future = now + Duration::from_secs(60);
        assert_eq!(
            subagent_status(Some("user"), None, Some(future), now),
            HerdStatus::Unknown
        );
    }

    #[test]
    fn session_binds_to_the_pane_owning_its_pid() {
        let agents = join_sessions_with_panes(
            vec![session(4242, "alpha", "/repo")],
            vec![pane(7, "claude", &[100, 4242, 4243])],
        );
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].pane_id, Some(7));
        assert_eq!(agents[0].name, "alpha");
        assert!(agents[0].can_stop());
    }

    #[test]
    fn session_falls_back_to_a_unique_cwd_match() {
        let mut row = pane(9, "claude", &[]);
        row.cwd = Some(PathBuf::from("/repo"));
        let agents = join_sessions_with_panes(vec![session(1, "alpha", "/repo")], vec![row]);
        assert_eq!(agents[0].pane_id, Some(9));
    }

    #[test]
    fn ambiguous_cwd_match_leaves_the_session_unbound() {
        // Two claude panes in one directory: binding either would risk
        // sending Ctrl-C to the wrong agent.
        let agents = join_sessions_with_panes(
            vec![session(1, "alpha", "/repo")],
            vec![pane(1, "claude", &[]), pane(2, "claude", &[])],
        );
        let alpha = agents.iter().find(|a| a.name == "alpha").unwrap();
        assert_eq!(alpha.pane_id, None);
        assert!(!alpha.can_stop());
        // Both panes still show up on their own.
        assert_eq!(agents.len(), 3);
    }

    #[test]
    fn a_pane_is_never_bound_to_two_sessions() {
        let agents = join_sessions_with_panes(
            vec![session(10, "alpha", "/repo"), session(11, "beta", "/repo")],
            vec![pane(5, "claude", &[10, 11])],
        );
        let bound: Vec<_> = agents.iter().filter_map(|a| a.pane_id).collect();
        assert_eq!(bound, vec![5]);
        // Oldest session wins the pane; the other is left unbound.
        let alpha = agents.iter().find(|a| a.name == "alpha").unwrap();
        assert_eq!(alpha.pane_id, Some(5));
        let beta = agents.iter().find(|a| a.name == "beta").unwrap();
        assert_eq!(beta.pane_id, None);
    }

    #[test]
    fn unmatched_panes_become_rows_so_other_vendors_appear() {
        let agents = join_sessions_with_panes(vec![], vec![pane(3, "codex", &[])]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].provider, "codex");
        assert_eq!(agents[0].pane_id, Some(3));
        assert_eq!(agents[0].pid, None);
    }

    #[test]
    fn a_session_without_a_pane_cannot_be_stopped() {
        let agents = join_sessions_with_panes(vec![session(1, "elsewhere", "/other")], vec![]);
        assert_eq!(agents[0].pane_id, None);
        assert!(!agents[0].can_stop());
    }

    #[test]
    fn groups_sort_attention_first_and_are_stable_by_name() {
        let agents = vec![
            agent("zulu", HerdStatus::Idle, "/repo"),
            agent("alpha", HerdStatus::Idle, "/repo"),
            agent("mike", HerdStatus::Blocked, "/repo"),
            agent("bravo", HerdStatus::Done, "/repo"),
            agent("kilo", HerdStatus::Working, "/repo"),
        ];
        let groups = group_by_project(agents, HerdView::CurrentProject, Some(Path::new("/repo")));
        let names: Vec<&str> = groups[0].agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["mike", "kilo", "alpha", "zulu", "bravo"]);
    }

    #[test]
    fn current_project_view_excludes_other_projects() {
        let agents = vec![
            agent("mine", HerdStatus::Working, "/repo"),
            agent("theirs", HerdStatus::Working, "/elsewhere"),
        ];
        let groups = group_by_project(agents, HerdView::CurrentProject, Some(Path::new("/repo")));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].agents.len(), 1);
        assert_eq!(groups[0].agents[0].name, "mine");
        assert_eq!(groups[0].label, "repo");
        assert!(!groups[0].show_header);
    }

    #[test]
    fn an_agent_without_a_root_belongs_to_a_project_containing_its_cwd() {
        let mut nested = agent("nested", HerdStatus::Working, "/repo");
        nested.project_root = None;
        nested.cwd = Some(PathBuf::from("/repo/crates/inner"));
        let mut outside = agent("outside", HerdStatus::Working, "/repo");
        outside.project_root = None;
        outside.cwd = Some(PathBuf::from("/somewhere/else"));

        let groups = group_by_project(
            vec![nested, outside],
            HerdView::CurrentProject,
            Some(Path::new("/repo")),
        );
        let names: Vec<&str> = groups[0].agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["nested"]);
    }

    #[test]
    fn current_project_view_without_context_shows_everything() {
        let agents = vec![
            agent("mine", HerdStatus::Working, "/repo"),
            agent("theirs", HerdStatus::Working, "/elsewhere"),
        ];
        let groups = group_by_project(agents, HerdView::CurrentProject, None);
        assert_eq!(groups[0].agents.len(), 2);
    }

    #[test]
    fn grouped_view_buckets_by_project_and_floats_attention_up() {
        let agents = vec![
            agent("calm", HerdStatus::Idle, "/aaa"),
            agent("urgent", HerdStatus::Blocked, "/zzz"),
        ];
        let groups = group_by_project(agents, HerdView::AllGrouped, Some(Path::new("/aaa")));
        assert_eq!(groups.len(), 2);
        // Blocked project first, despite sorting last alphabetically.
        assert_eq!(groups[0].label, "zzz");
        assert_eq!(groups[1].label, "aaa");
        assert!(groups[0].show_header);
    }

    #[test]
    fn view_toggles_both_ways() {
        assert_eq!(HerdView::CurrentProject.toggled(), HerdView::AllGrouped);
        assert_eq!(HerdView::AllGrouped.toggled(), HerdView::CurrentProject);
    }

    #[test]
    fn stop_is_refused_for_idle_and_finished_agents() {
        let mut idle = agent("idle", HerdStatus::Idle, "/repo");
        assert!(!idle.can_stop());
        idle.status = HerdStatus::Done;
        assert!(!idle.can_stop());
        idle.status = HerdStatus::Blocked;
        assert!(idle.can_stop());
    }

    #[test]
    fn project_label_falls_back_through_root_then_cwd() {
        let mut agent = agent("x", HerdStatus::Idle, "/repo");
        assert_eq!(agent.project_label(), "repo");
        agent.project_root = None;
        agent.cwd = Some(PathBuf::from("/tmp/scratch"));
        assert_eq!(agent.project_label(), "scratch");
        agent.cwd = None;
        assert_eq!(agent.project_label(), "—");
    }

    #[test]
    fn project_root_walks_up_to_the_repo() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let nested = root.join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(project_root_for(&nested), Some(root));
    }

    #[test]
    fn project_root_handles_a_linked_worktree_gitfile() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        // Linked worktrees carry `.git` as a file, and are their own space.
        std::fs::write(
            worktree.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/wt",
        )
        .unwrap();
        assert_eq!(project_root_for(&worktree), Some(worktree));
    }

    #[test]
    fn project_root_is_none_outside_a_repo() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(project_root_for(temp.path()), None);
    }
}
