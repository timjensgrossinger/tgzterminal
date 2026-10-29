//! Pure Tower logic: the rules for how flights, handoffs and ladders move.
//!
//! No IO, no clocks, no randomness: every function takes the state it needs and
//! returns the next state or a decision, so all of it is unit-testable.

pub mod bandit;
pub mod cron;
pub mod fanout;
pub mod router;
pub mod work;

use tower_proto::{
    AgentStatus, GateResult, GateState, HandDirection, HandoffPacket, HandoffState, ModelRef, Role,
    Stage,
};

/// Why a transition was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    InvalidTransition {
        from: &'static str,
        to: &'static str,
    },
    MissingField(&'static str),
    NeedsClearance,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::InvalidTransition { from, to } => write!(f, "cannot go from {from} to {to}"),
            Refusal::MissingField(name) => write!(f, "handoff packet is missing {name}"),
            Refusal::NeedsClearance => write!(f, "this handoff needs a human clearance"),
        }
    }
}

impl std::error::Error for Refusal {}

// ---------------------------------------------------------------------------
// Handoffs
// ---------------------------------------------------------------------------

/// What can happen to a handoff packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffAction {
    /// Release a draft. `cleared_by_human` is true when a person pressed Release.
    Release { cleared_by_human: bool },
    /// The receiving agent picked it up.
    Accept,
    /// The receiving agent reported back.
    Return,
    /// A human or the receiver refused it.
    Reject,
}

/// A packet must be self-contained before it can leave Draft: a smaller model
/// gets nothing but the packet, so missing pieces are a bug in the packet.
pub fn validate_packet(packet: &HandoffPacket) -> Result<(), Refusal> {
    if packet.goal.trim().is_empty() {
        return Err(Refusal::MissingField("a goal"));
    }
    if packet.acceptance.iter().all(|a| a.trim().is_empty()) {
        return Err(Refusal::MissingField("acceptance criteria"));
    }
    if packet.return_to.trim().is_empty() {
        return Err(Refusal::MissingField("return_to"));
    }
    Ok(())
}

/// Apply `action` to a packet addressed to `target`.
pub fn advance_handoff(
    packet: &HandoffPacket,
    target: &Role,
    action: HandoffAction,
) -> Result<HandoffState, Refusal> {
    use HandoffState::*;
    let refuse = |to: &'static str| Refusal::InvalidTransition {
        from: packet.state.label(),
        to,
    };
    match (packet.state, action) {
        (Draft, HandoffAction::Release { cleared_by_human }) => {
            validate_packet(packet)?;
            if !cleared_by_human && !target.auto_release {
                return Err(Refusal::NeedsClearance);
            }
            Ok(Released)
        }
        (Released, HandoffAction::Accept) => Ok(Accepted),
        (Accepted, HandoffAction::Return) => Ok(Returned),
        (Draft | Released | Accepted, HandoffAction::Reject) => Ok(Rejected),
        (_, HandoffAction::Release { .. }) => Err(refuse("released")),
        (_, HandoffAction::Accept) => Err(refuse("accepted")),
        (_, HandoffAction::Return) => Err(refuse("returned")),
        (_, HandoffAction::Reject) => Err(refuse("rejected")),
    }
}

/// Classify a handoff by where the target's cheapest model sits relative to
/// the sender's, for the lineage arrows on a strip.
pub fn hand_direction(from: &Role, to: &Role) -> HandDirection {
    match (from.ladder.first(), to.ladder.first()) {
        (Some(a), Some(b)) => match (model_rank(a), model_rank(b)) {
            (x, y) if y < x => HandDirection::Down,
            (x, y) if y > x => HandDirection::Up,
            _ => HandDirection::Across,
        },
        _ => HandDirection::Across,
    }
}

/// Rough cost rank: local models below every cloud model; cloud models ranked
/// by well-known tier words. Only used for display and default routing.
pub fn model_rank(model: &ModelRef) -> u8 {
    use tower_proto::Locality;
    if model.locality == Locality::Local {
        return 0;
    }
    let l = model.label.to_ascii_lowercase();
    if l.contains("haiku") || l.contains("flash") || l.contains("mini") {
        1
    } else if l.contains("opus") || l.contains("pro") || l.contains("max") {
        3
    } else {
        2
    }
}

// ---------------------------------------------------------------------------
// Gates and stages
// ---------------------------------------------------------------------------

/// Overall verdict of a flight's gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// No gates configured: a flight can never land on an agent's say-so alone.
    Missing,
    Running,
    Passed,
    Failed,
}

pub fn gate_verdict(gates: &[GateResult]) -> GateVerdict {
    if gates.is_empty() {
        return GateVerdict::Missing;
    }
    if gates.iter().any(|g| g.state == GateState::Failed) {
        return GateVerdict::Failed;
    }
    if gates.iter().all(|g| g.state == GateState::Passed) {
        return GateVerdict::Passed;
    }
    GateVerdict::Running
}

/// Where a flight in `Gates` goes next.
pub fn stage_after_gates(gates: &[GateResult]) -> Stage {
    match gate_verdict(gates) {
        GateVerdict::Passed => Stage::Landed,
        GateVerdict::Failed | GateVerdict::Missing => Stage::NeedsYou,
        GateVerdict::Running => Stage::Gates,
    }
}

// ---------------------------------------------------------------------------
// Ladder
// ---------------------------------------------------------------------------

/// Which ladder rung a flight should run on after `failures` failed gate runs.
/// Climbs one rung every `failures_per_rung` failures and stops at the top.
pub fn ladder_rung(ladder_len: usize, failures: u32, failures_per_rung: u32) -> Option<usize> {
    if ladder_len == 0 {
        return None;
    }
    let per = failures_per_rung.max(1);
    Some(((failures / per) as usize).min(ladder_len - 1))
}

// ---------------------------------------------------------------------------
// Message delivery
// ---------------------------------------------------------------------------

/// Whether a message for an agent can be delivered now. Typing into an agent
/// mid-turn corrupts its prompt, so anything but an idle agent queues.
pub fn can_deliver(status: AgentStatus) -> bool {
    matches!(status, AgentStatus::Holding)
}

/// `@name` mentions in a message, lowercased and de-duplicated, in order.
/// A name is letters, digits, `-`, `_` and `.`; a trailing `.` is punctuation.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut prev: Option<char> = None;
    while let Some((i, c)) = chars.next() {
        let starts = c == '@' && !prev.map(|p| p.is_alphanumeric()).unwrap_or(false);
        prev = Some(c);
        if !starts {
            continue;
        }
        let rest = &text[i + 1..];
        let len = rest
            .char_indices()
            .find(|(_, ch)| !(ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.')))
            .map(|(j, _)| j)
            .unwrap_or(rest.len());
        let name = rest[..len].trim_end_matches('.').to_lowercase();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Key for the `n`th handoff out of `parent`: `TWR-12` -> `TWR-12.1`.
pub fn child_key(parent: &str, n: u32) -> String {
    format!("{parent}.{n}")
}

/// The rung an explicit Escalate moves to, or `None` at the top.
pub fn escalated_rung(ladder_len: usize, current: usize) -> Option<usize> {
    let next = current + 1;
    (next < ladder_len).then_some(next)
}

/// One line describing a Claude Code tool call for the timeline: the tool and
/// its most telling argument (a path, a command, a pattern), shortened.
pub fn tool_summary(tool: &str, input: &serde_json::Value) -> String {
    let pick = [
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|k| input.get(*k).and_then(|v| v.as_str()));
    match pick {
        Some(arg) => {
            let one_line = arg.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{tool} {}", shorten(&one_line, 90))
        }
        None => tool.to_string(),
    }
}

/// Is a permission request about one of Tower's own team tools? Agents name
/// MCP tools `mcp__<server>__<tool>` (Claude) or `<server>_<tool>` (OpenCode).
pub fn is_tower_tool(title: &str) -> bool {
    let t = title.trim().to_ascii_lowercase();
    let tool = t.split_whitespace().next().unwrap_or("");
    tool.starts_with("mcp__tower__tower_") || tool.starts_with("tower_tower_")
}

/// `s` cut to at most `max` chars, with an ellipsis when cut.
pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A flight title from free text: its first line, shortened.
pub fn title_from_task(task: &str) -> String {
    let first = task
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Untitled flight");
    shorten(first, 80)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_proto::{DriverKind, ModelRef};

    #[test]
    fn mentions_are_parsed_conservatively() {
        assert_eq!(
            mentions("@ground please, and @Review."),
            vec!["ground", "review"]
        );
        assert_eq!(mentions("mail me at tim@example.com"), Vec::<String>::new());
        assert_eq!(
            mentions("@ground-local @ground-local @approach-1"),
            vec!["ground-local", "approach-1"]
        );
        assert!(mentions("@ alone").is_empty());
    }

    #[test]
    fn tower_tools_are_recognised_but_not_lookalikes() {
        assert!(is_tower_tool("mcp__tower__tower_return"));
        assert!(is_tower_tool("tower_tower_complete"));
        assert!(!is_tower_tool("mcp__evil__tower_return"));
        assert!(!is_tower_tool("bash: echo mcp__tower__tower_return"));
        assert!(!is_tower_tool("tower_return"));
    }

    #[test]
    fn keys_rungs_and_titles() {
        assert_eq!(child_key("TWR-12", 2), "TWR-12.2");
        assert_eq!(escalated_rung(2, 0), Some(1));
        assert_eq!(escalated_rung(2, 1), None);
        assert_eq!(
            title_from_task("\n  Fix the login loop\nmore detail"),
            "Fix the login loop"
        );
        assert_eq!(title_from_task("   "), "Untitled flight");
        assert_eq!(shorten("abcdef", 4), "abc…");
    }

    #[test]
    fn tool_summaries_pick_the_telling_argument() {
        let v = serde_json::json!({"file_path": "/src/main.rs", "content": "x"});
        assert_eq!(tool_summary("Write", &v), "Write /src/main.rs");
        let v = serde_json::json!({"command": "cargo   test\n -p core"});
        assert_eq!(tool_summary("Bash", &v), "Bash cargo test -p core");
        assert_eq!(
            tool_summary("TodoWrite", &serde_json::json!({"todos": []})),
            "TodoWrite"
        );
    }

    fn role(id: &str, auto_release: bool, ladder: Vec<ModelRef>) -> Role {
        Role {
            id: id.into(),
            name: id.into(),
            mission: String::new(),
            color: [0, 0, 0],
            driver: DriverKind::Acp,
            program: "x".into(),
            ladder,
            auto_release,
            max_concurrency: 1,
            ..Default::default()
        }
    }

    fn packet(state: HandoffState) -> HandoffPacket {
        HandoffPacket {
            id: "h".into(),
            from_role: "a".into(),
            to_role: "g".into(),
            goal: "do it".into(),
            context: vec![],
            constraints: vec![],
            acceptance: vec!["tests pass".into()],
            return_to: "a-1".into(),
            state,
            model: None,
            local_only: false,
        }
    }

    #[test]
    fn draft_needs_clearance_unless_target_auto_releases() {
        let manual = role("g", false, vec![]);
        let auto = role("g", true, vec![]);
        let p = packet(HandoffState::Draft);
        let agent_release = HandoffAction::Release {
            cleared_by_human: false,
        };
        assert_eq!(
            advance_handoff(&p, &manual, agent_release),
            Err(Refusal::NeedsClearance)
        );
        assert_eq!(
            advance_handoff(&p, &auto, agent_release),
            Ok(HandoffState::Released)
        );
        assert_eq!(
            advance_handoff(
                &p,
                &manual,
                HandoffAction::Release {
                    cleared_by_human: true
                }
            ),
            Ok(HandoffState::Released)
        );
    }

    #[test]
    fn incomplete_packets_cannot_leave_draft() {
        let target = role("g", true, vec![]);
        let mut p = packet(HandoffState::Draft);
        p.acceptance = vec!["  ".into()];
        let release = HandoffAction::Release {
            cleared_by_human: true,
        };
        assert_eq!(
            advance_handoff(&p, &target, release),
            Err(Refusal::MissingField("acceptance criteria"))
        );
        p.acceptance = vec!["ok".into()];
        p.goal = String::new();
        assert_eq!(
            advance_handoff(&p, &target, release),
            Err(Refusal::MissingField("a goal"))
        );
    }

    #[test]
    fn handoff_lifecycle_is_linear() {
        let target = role("g", true, vec![]);
        let p = packet(HandoffState::Released);
        assert_eq!(
            advance_handoff(&p, &target, HandoffAction::Accept),
            Ok(HandoffState::Accepted)
        );
        let p = packet(HandoffState::Accepted);
        assert_eq!(
            advance_handoff(&p, &target, HandoffAction::Return),
            Ok(HandoffState::Returned)
        );
        let p = packet(HandoffState::Returned);
        assert!(advance_handoff(&p, &target, HandoffAction::Reject).is_err());
        let p = packet(HandoffState::Draft);
        assert!(advance_handoff(&p, &target, HandoffAction::Return).is_err());
    }

    #[test]
    fn gates_decide_landing_and_empty_gates_never_land() {
        let g = |state| GateResult {
            name: "g".into(),
            state,
            detail: None,
        };
        assert_eq!(stage_after_gates(&[]), Stage::NeedsYou);
        assert_eq!(
            stage_after_gates(&[g(GateState::Passed), g(GateState::Running)]),
            Stage::Gates
        );
        assert_eq!(
            stage_after_gates(&[g(GateState::Passed), g(GateState::Failed)]),
            Stage::NeedsYou
        );
        assert_eq!(
            stage_after_gates(&[g(GateState::Passed), g(GateState::Passed)]),
            Stage::Landed
        );
    }

    #[test]
    fn ladder_climbs_and_saturates() {
        assert_eq!(ladder_rung(0, 5, 2), None);
        assert_eq!(ladder_rung(3, 0, 2), Some(0));
        assert_eq!(ladder_rung(3, 1, 2), Some(0));
        assert_eq!(ladder_rung(3, 2, 2), Some(1));
        assert_eq!(ladder_rung(3, 9, 2), Some(2));
        assert_eq!(
            ladder_rung(3, 1, 0),
            Some(1),
            "zero per-rung means every failure"
        );
    }

    #[test]
    fn direction_follows_model_rank() {
        let approach = role("a", false, vec![ModelRef::cloud("sonnet")]);
        let ground = role("g", true, vec![ModelRef::local("qwen", "lan")]);
        let big = role("b", false, vec![ModelRef::cloud("opus")]);
        assert_eq!(hand_direction(&approach, &ground), HandDirection::Down);
        assert_eq!(hand_direction(&ground, &approach), HandDirection::Up);
        assert_eq!(hand_direction(&approach, &big), HandDirection::Up);
        assert_eq!(hand_direction(&approach, &approach), HandDirection::Across);
    }

    #[test]
    fn only_idle_agents_take_messages() {
        assert!(can_deliver(AgentStatus::Holding));
        assert!(!can_deliver(AgentStatus::Working));
        assert!(!can_deliver(AgentStatus::NeedsYou));
        assert!(!can_deliver(AgentStatus::Offline));
    }
}
