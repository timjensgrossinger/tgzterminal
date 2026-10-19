//! Wire types shared by `towerd` and the in-terminal Tower view.
//!
//! This file is also `#[path]`-included into wezterm-gui (edition 2018), so it
//! must stay dependency-free apart from `serde`/`serde_json` and compile under
//! both editions.
//!
//! Framing is newline-delimited JSON: one `ClientMsg` or `ServerMsg` per line.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Where a flight is on the board. Order is the board's column order.
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Arrived, not yet assigned.
    #[default]
    Holding,
    /// Assigned to a role, waiting for an agent (or a clearance) to start.
    Cleared,
    /// An agent is working on it.
    InFlight,
    /// The agent called `complete`; gates are running.
    Gates,
    /// Blocked on a human: a clearance, a question or a failed gate.
    NeedsYou,
    /// Gates passed.
    Landed,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Holding,
        Stage::Cleared,
        Stage::InFlight,
        Stage::Gates,
        Stage::NeedsYou,
        Stage::Landed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Holding => "Holding",
            Stage::Cleared => "Cleared",
            Stage::InFlight => "In flight",
            Stage::Gates => "Gates",
            Stage::NeedsYou => "Needs you",
            Stage::Landed => "Landed",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    Local,
    #[default]
    Cloud,
}

/// A model an agent runs on.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelRef {
    /// What the driver is given: `sonnet`, `opencode-go/kimi-k2.7-code`,
    /// `ollama/qwen3.6:27b`.
    #[serde(default)]
    pub id: String,
    /// Short display name.
    pub label: String,
    pub locality: Locality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl ModelRef {
    pub fn local(label: &str, host: &str) -> Self {
        Self {
            id: label.to_string(),
            label: label.to_string(),
            locality: Locality::Local,
            host: Some(host.to_string()),
        }
    }

    pub fn cloud(label: &str) -> Self {
        Self {
            id: label.to_string(),
            label: label.to_string(),
            locality: Locality::Cloud,
            host: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// A real interactive CLI in a terminal pane (Claude Code).
    #[default]
    Pane,
    /// An Agent Client Protocol agent over stdio (OpenCode, Gemini, Codex, ...).
    Acp,
    /// A plain OpenAI-compatible chat call, no tools.
    Llm,
}

impl DriverKind {
    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Pane => "pane",
            DriverKind::Acp => "ACP",
            DriverKind::Llm => "LLM",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub mission: String,
    /// sRGB.
    pub color: [u8; 3],
    pub driver: DriverKind,
    /// Program the driver launches, for display, e.g. `claude` or `opencode acp`.
    pub program: String,
    /// Preferred models, cheapest first. Escalation walks up this list.
    pub ladder: Vec<ModelRef>,
    /// Release handoffs addressed to this role without a human clearance.
    #[serde(default)]
    pub auto_release: bool,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    /// The role's TOML file, for "Edit role".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Something that will stop this role from working, e.g. a missing model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn default_concurrency() -> u32 {
    1
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Launched, not yet reporting.
    #[default]
    Starting,
    Working,
    /// Idle and waiting for its next message.
    Holding,
    /// Waiting on a human (permission prompt, question).
    NeedsYou,
    Offline,
}

impl AgentStatus {
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Starting => "starting",
            AgentStatus::Working => "working",
            AgentStatus::Holding => "holding",
            AgentStatus::NeedsYou => "needs you",
            AgentStatus::Offline => "offline",
        }
    }

    pub fn is_live(self) -> bool {
        self != AgentStatus::Offline
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    /// You (or Tower on your behalf) to the agent.
    User,
    Agent,
    Tool,
    System,
    Error,
}

/// One line of an agent's conversation, as shown in its drawer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChatEntry {
    pub kind: ChatKind,
    pub text: String,
    pub at_unix: u64,
}

/// Tokens and money spent. Cost is only known for some providers.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0 && self.cost_usd.is_none()
    }

    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cost_usd = match (self.cost_usd, o.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Agent {
    pub id: String,
    pub role_id: String,
    pub status: AgentStatus,
    pub model: ModelRef,
    #[serde(default)]
    pub driver: DriverKind,
    /// The terminal pane a pane-driven agent lives in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flight_id: Option<String>,
    /// One-line current activity, e.g. "Edit src/parser.rs".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// ACP agents only: the conversation. Pane agents talk in their pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript: Vec<ChatEntry>,
    /// Messages waiting for the agent to become idle.
    #[serde(default)]
    pub queued: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub started_unix: u64,
    #[serde(default)]
    pub usage: Usage,
    /// Thinking effort it was started with, when one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandDirection {
    /// To a smaller / cheaper model.
    Down,
    /// To a bigger model after a failure or on request.
    Up,
    /// Same tier, different role.
    Across,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LineageStep {
    pub from_role: String,
    pub to_role: String,
    pub direction: HandDirection,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HandoffState {
    /// Written, not yet released. Editable.
    #[default]
    Draft,
    Released,
    Accepted,
    /// The receiving agent reported back to `return_to`.
    Returned,
    Rejected,
}

impl HandoffState {
    pub fn label(self) -> &'static str {
        match self {
            HandoffState::Draft => "draft",
            HandoffState::Released => "released",
            HandoffState::Accepted => "accepted",
            HandoffState::Returned => "returned",
            HandoffState::Rejected => "rejected",
        }
    }
}

/// A typed, inspectable unit of delegated work.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct HandoffPacket {
    pub id: String,
    pub from_role: String,
    pub to_role: String,
    pub goal: String,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Agent or role id the result goes back to. Empty: back to you.
    pub return_to: String,
    pub state: HandoffState,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateState {
    Pending,
    Running,
    Passed,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct GateResult {
    pub name: String,
    pub state: GateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimelineBody {
    Message {
        from: String,
        text: String,
    },
    ToolCall {
        agent: String,
        tool: String,
        summary: String,
    },
    Handoff {
        packet: HandoffPacket,
    },
    Gate {
        gate: GateResult,
    },
    System {
        text: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TimelineEntry {
    pub at_unix: u64,
    #[serde(flatten)]
    pub body: TimelineBody,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Flight {
    pub id: String,
    /// Short human key: `TWR-12` locally, `TWR-12.1` for a handoff, `PROJ-341` from Jira.
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub brief: String,
    pub stage: Stage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub lineage: Vec<LineageStep>,
    /// Where it came from, e.g. "jira", "local" or "handoff".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub created_unix: u64,
    pub updated_unix: u64,
    #[serde(default)]
    pub gates: Vec<GateResult>,
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
    /// Directory the flight's agents work in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// Set when the flight has its own git worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The flight that handed this one down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Failed gate runs; drives the escalation hint.
    #[serde(default)]
    pub failures: u32,
    /// Everything its agents spent.
    #[serde(default)]
    pub usage: Usage,
    /// Link to the issue it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// How hard the router judged it, and which engine it chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteInfo>,
    /// `code`, `ops`, `design` or `question` (issues in auto-fix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// How the agent ended it: `resolved`, `proposed` or `declined`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Deliverables (documents, prototypes), absolute paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

/// A routing decision: tier from the task text (and triage), then the first
/// available engine of that tier.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct RouteInfo {
    /// `low`, `medium` or `high`.
    pub tier: String,
    /// The lowest tier the task may run at (risk vocabulary, reasoning).
    #[serde(default)]
    pub floor: String,
    #[serde(default)]
    pub score: f32,
    /// `low`, `medium` or `high`.
    #[serde(default)]
    pub effort: String,
    /// Routing engine id, for roles routed by tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The signals that fired.
    #[serde(default)]
    pub reason: String,
    /// What set the tier: `heuristic`, `triage`, `you`, `bandit` or `escalated`.
    #[serde(default)]
    pub by: String,
    /// The learned policy's pick (`tier:engine`), in shadow mode only shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandit: Option<String>,
}

/// One engine in the `[routing]` table, as the Models view shows it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct EngineInfo {
    pub id: String,
    pub tier: String,
    pub driver: DriverKind,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
    pub available: bool,
    /// Why it can't be used right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Arm observations, for the learned policy.
    #[serde(default)]
    pub trained: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// An agent asked to run a tool.
    #[default]
    Permission,
    /// A handoff or a landing waiting for release.
    Clearance,
    /// `ask_human` from an agent.
    Question,
    GateFailure,
}

impl InboxKind {
    pub fn label(self) -> &'static str {
        match self {
            InboxKind::Permission => "Permission",
            InboxKind::Clearance => "Clearance",
            InboxKind::Question => "Question",
            InboxKind::GateFailure => "Gate failed",
        }
    }
}

/// A button on an inbox item.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct InboxOption {
    pub id: String,
    pub label: String,
    /// Rendered as the primary action.
    #[serde(default)]
    pub primary: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct InboxItem {
    pub id: String,
    pub kind: InboxKind,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flight_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub created_unix: u64,
    /// Buttons. Empty: the default Approve / Deny.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<InboxOption>,
    /// Takes a typed answer.
    #[serde(default)]
    pub answerable: bool,
    /// The answer lives in this pane (Claude permission prompts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Up,
    Down,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// "ollama", "lmstudio", "llama.cpp", "vllm", "mlx", "openai-compatible".
    pub kind: String,
    pub health: Health,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FreqMessage {
    pub from: String,
    pub text: String,
    pub at_unix: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Frequency {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(default)]
    pub unread: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<FreqMessage>,
}

/// An intake watch (e.g. a Jira JQL query) and how its polling goes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SourceStatus {
    pub name: String,
    /// "jira".
    pub kind: String,
    /// What it watches, e.g. the JQL.
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_id: Option<String>,
    #[serde(default)]
    pub auto_launch: bool,
    #[serde(default)]
    pub last_poll_unix: u64,
    #[serde(default)]
    pub polling: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Flights created from this watch so far.
    #[serde(default)]
    pub flights: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotOrigin {
    /// Built-in demo data; nothing is real.
    Demo,
    /// Live state from towerd.
    Live,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub origin: SnapshotOrigin,
    pub now_unix: u64,
    pub roles: Vec<Role>,
    pub agents: Vec<Agent>,
    pub flights: Vec<Flight>,
    pub inbox: Vec<InboxItem>,
    pub endpoints: Vec<Endpoint>,
    pub frequencies: Vec<Frequency>,
    /// Where roles/*.toml, models.toml and tower.toml live.
    #[serde(default)]
    pub config_dir: String,
    #[serde(default)]
    pub sources: Vec<SourceStatus>,
    /// Where flights get their own folders (`[workspace] flights_root`).
    #[serde(default)]
    pub flights_root: String,
    /// The role that picks roles for "Auto" flights; empty when none.
    #[serde(default)]
    pub triage_role: String,
    /// The `[routing]` engines, low tier first.
    #[serde(default)]
    pub engines: Vec<EngineInfo>,
    /// `off`, `shadow` or `live`.
    #[serde(default)]
    pub bandit_mode: String,
}

impl Snapshot {
    pub fn role(&self, id: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn flight(&self, id: &str) -> Option<&Flight> {
        self.flights.iter().find(|f| f.id == id)
    }

    pub fn flights_in(&self, stage: Stage) -> impl Iterator<Item = &Flight> {
        self.flights.iter().filter(move |f| f.stage == stage)
    }

    pub fn agents_of_role<'a>(&'a self, role_id: &'a str) -> impl Iterator<Item = &'a Agent> {
        self.agents.iter().filter(move |a| a.role_id == role_id)
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        version: u32,
        token: String,
        /// "gui", "hook", "mcp", "cli".
        client: String,
    },
    GetSnapshot,
    Ping,

    // -- from the Tower view -------------------------------------------------
    CreateFlight {
        title: String,
        #[serde(default)]
        brief: String,
        #[serde(default)]
        role_id: Option<String>,
        /// Defaults to the daemon's working directory.
        #[serde(default)]
        workdir: Option<String>,
        #[serde(default)]
        worktree: bool,
        /// Give the flight a fresh folder of its own under the flights root.
        #[serde(default)]
        new_folder: bool,
        /// No role given: let the triage role pick one.
        #[serde(default)]
        auto_assign: bool,
        /// Start an agent of `role_id` on it right away.
        #[serde(default)]
        launch: bool,
        /// `low` / `medium` / `high` to override the router; `None` = auto.
        #[serde(default)]
        tier: Option<String>,
    },
    /// Launch an agent of a role on a flight, or on a new flight made from `task`.
    LaunchAgent {
        role_id: String,
        #[serde(default)]
        flight_id: Option<String>,
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        workdir: Option<String>,
        /// For a new flight made from `task`: give it its own folder.
        #[serde(default)]
        new_folder: bool,
    },
    StopAgent {
        agent_id: String,
    },
    SendMessage {
        agent_id: String,
        text: String,
    },
    Post {
        frequency_id: String,
        text: String,
    },
    ResolveInbox {
        item_id: String,
        approve: bool,
        #[serde(default)]
        option_id: Option<String>,
        #[serde(default)]
        text: Option<String>,
    },
    ReleaseHandoff {
        flight_id: String,
        handoff_id: String,
    },
    /// Hand a flight (or part of it) to another role, written by you.
    HandDown {
        flight_id: String,
        packet: HandoffPacket,
    },
    Escalate {
        flight_id: String,
    },
    DeleteFlight {
        flight_id: String,
    },
    ProbeEndpoints,
    ReloadConfig,
    /// Poll every intake watch now.
    PollSources,

    // -- from the Tower view, about panes it spawned for the daemon ----------
    PaneSpawned {
        request_id: u64,
        #[serde(default)]
        pane_id: Option<u64>,
        #[serde(default)]
        error: Option<String>,
    },
    PaneExited {
        pane_id: u64,
    },
    LivePanes {
        pane_ids: Vec<u64>,
    },

    // -- from agent-side shims -----------------------------------------------
    /// A Claude Code hook fired (`tower hook <event>`).
    Hook {
        agent_id: String,
        event: String,
        payload: serde_json::Value,
    },
    /// A `tower_*` MCP tool call (`tower mcp`).
    Tool {
        agent_id: String,
        name: String,
        args: serde_json::Value,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        version: u32,
        daemon_pid: u32,
    },
    Snapshot {
        snapshot: Snapshot,
    },
    Error {
        message: String,
    },
    Pong,
    Ack,
    ToolResult {
        ok: bool,
        text: String,
    },

    // -- to the Tower view: terminal work only the GUI can do ----------------
    SpawnPane {
        request_id: u64,
        argv: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        title: String,
    },
    SendToPane {
        pane_id: u64,
        text: String,
        /// Press Enter after pasting.
        submit: bool,
    },
    FocusPane {
        pane_id: u64,
    },
    ClosePane {
        pane_id: u64,
    },
}

/// Serialize one message as a single NDJSON line, including the trailing `\n`.
pub fn encode_line<T: Serialize>(msg: &T) -> serde_json::Result<String> {
    let mut s = serde_json::to_string(msg)?;
    s.push('\n');
    Ok(s)
}

/// Parse one NDJSON line (a trailing `\r\n` / `\n` is ignored).
pub fn decode_line<'a, T: Deserialize<'a>>(line: &'a str) -> serde_json::Result<T> {
    serde_json::from_str(line.trim_end_matches(|c| c == '\n' || c == '\r'))
}

// ---------------------------------------------------------------------------
// Demo data (`tower daemon --demo`, and the GUI before it connects)
// ---------------------------------------------------------------------------

/// A believable team so the UI can be judged before anything is wired up.
pub fn demo_snapshot(now_unix: u64) -> Snapshot {
    let m = |mins: u64| now_unix.saturating_sub(mins * 60);
    let qwen = ModelRef::local("qwen3.8-27b", "studio.lan");
    let sonnet = ModelRef::cloud("sonnet");
    let opus = ModelRef::cloud("opus");
    let kimi = ModelRef::cloud("kimi-k2.7-code");

    let roles = vec![
        Role {
            id: "approach".into(),
            name: "Approach".into(),
            mission: "Owns a flight end to end: plans the change, hands routine work to Ground, \
                      reviews what comes back and calls complete."
                .into(),
            color: [0x7a, 0xa2, 0xf7],
            driver: DriverKind::Pane,
            program: "claude".into(),
            ladder: vec![sonnet.clone(), opus.clone()],
            auto_release: false,
            max_concurrency: 2,
            ..Default::default()
        },
        Role {
            id: "ground".into(),
            name: "Ground".into(),
            mission:
                "Executes scoped handoff packets: mechanical refactors, tests, docs. Never plans, \
                      never pushes."
                    .into(),
            color: [0x9e, 0xce, 0x6a],
            driver: DriverKind::Acp,
            program: "opencode acp".into(),
            ladder: vec![kimi.clone(), sonnet.clone()],
            auto_release: true,
            max_concurrency: 3,
            ..Default::default()
        },
        Role {
            id: "review".into(),
            name: "Review".into(),
            mission:
                "Reads the diff against the packet's acceptance criteria and either clears the \
                      gate or sends it back with findings."
                    .into(),
            color: [0xbb, 0x9a, 0xf7],
            driver: DriverKind::Acp,
            program: "opencode acp".into(),
            ladder: vec![ModelRef::cloud("glm-5"), opus.clone()],
            auto_release: true,
            max_concurrency: 1,
            ..Default::default()
        },
    ];

    let agents = vec![
        Agent {
            id: "approach-1".into(),
            role_id: "approach".into(),
            status: AgentStatus::Working,
            model: sonnet.clone(),
            driver: DriverKind::Pane,
            pane_id: Some(7),
            flight_id: Some("f-12".into()),
            activity: Some("Reviewing Ground's patch for the parser split".into()),
            ..Default::default()
        },
        Agent {
            id: "ground-1".into(),
            role_id: "ground".into(),
            status: AgentStatus::Working,
            model: qwen.clone(),
            driver: DriverKind::Acp,
            flight_id: Some("f-14".into()),
            activity: Some("Running cargo test -p tower-core".into()),
            transcript: vec![
                ChatEntry {
                    kind: ChatKind::User,
                    text: "Port the usage folding tests to tower-core.".into(),
                    at_unix: m(50),
                },
                ChatEntry {
                    kind: ChatKind::Tool,
                    text: "edit core/src/usage.rs".into(),
                    at_unix: m(20),
                },
                ChatEntry {
                    kind: ChatKind::Agent,
                    text: "Ported 14 tests; running them now.".into(),
                    at_unix: m(2),
                },
            ],
            ..Default::default()
        },
        Agent {
            id: "ground-2".into(),
            role_id: "ground".into(),
            status: AgentStatus::NeedsYou,
            model: qwen.clone(),
            driver: DriverKind::Acp,
            flight_id: Some("f-15".into()),
            activity: Some("Wants to run: git stash".into()),
            ..Default::default()
        },
        Agent {
            id: "review-1".into(),
            role_id: "review".into(),
            status: AgentStatus::Holding,
            model: ModelRef::cloud("glm-5"),
            driver: DriverKind::Acp,
            ..Default::default()
        },
    ];

    let packet_parser = HandoffPacket {
        id: "h-31".into(),
        from_role: "approach".into(),
        to_role: "ground".into(),
        goal: "Split parser.rs into lexer.rs and parser.rs without behaviour changes".into(),
        context: vec!["src/parser.rs".into(), "src/lib.rs".into()],
        constraints: vec![
            "No public API changes".into(),
            "Keep every existing test passing".into(),
        ],
        acceptance: vec![
            "cargo test passes".into(),
            "No function body changes beyond moved code".into(),
        ],
        return_to: "approach-1".into(),
        state: HandoffState::Returned,
    };
    let packet_docs = HandoffPacket {
        id: "h-33".into(),
        from_role: "approach".into(),
        to_role: "ground".into(),
        goal: "Write rustdoc for every pub item in sources/jira.rs".into(),
        context: vec!["src/sources/jira.rs".into()],
        constraints: vec!["Docs only, no code changes".into()],
        acceptance: vec!["cargo doc has no missing-docs warnings for the module".into()],
        return_to: "approach-1".into(),
        state: HandoffState::Draft,
    };
    let down = |from: &str, to: &str| LineageStep {
        from_role: from.into(),
        to_role: to.into(),
        direction: HandDirection::Down,
    };

    let flights = vec![
        Flight {
            id: "f-17".into(),
            key: "PROJ-341".into(),
            title: "Login redirect loops when the session cookie is expired".into(),
            stage: Stage::Holding,
            source: Some("jira".into()),
            created_unix: m(4),
            updated_unix: m(4),
            timeline: vec![TimelineEntry {
                at_unix: m(4),
                body: TimelineBody::System {
                    text: "Arrived from Jira watch \"Team backlog\"".into(),
                },
            }],
            ..Default::default()
        },
        Flight {
            id: "f-16".into(),
            key: "TWR-16".into(),
            title: "Add an endpoint health probe for LAN model hosts".into(),
            stage: Stage::Holding,
            source: Some("local".into()),
            created_unix: m(22),
            updated_unix: m(22),
            ..Default::default()
        },
        Flight {
            id: "f-13".into(),
            key: "TWR-13".into(),
            title: "Document the handoff packet format".into(),
            stage: Stage::Cleared,
            role_id: Some("ground".into()),
            model: Some(qwen.clone()),
            route: Some(RouteInfo {
                tier: "low".into(),
                floor: "low".into(),
                score: 0.1,
                effort: "low".into(),
                engine: Some("qwen-local".into()),
                reason: "score=0.10 [base score only] → low".into(),
                by: "heuristic".into(),
                bandit: Some("medium:claude-sonnet".into()),
            }),
            lineage: vec![down("approach", "ground")],
            source: Some("local".into()),
            created_unix: m(40),
            updated_unix: m(9),
            timeline: vec![TimelineEntry {
                at_unix: m(9),
                body: TimelineBody::Handoff {
                    packet: packet_docs.clone(),
                },
            }],
            ..Default::default()
        },
        Flight {
            id: "f-12".into(),
            key: "TWR-12".into(),
            title: "Split the parser module into lexer and parser".into(),
            stage: Stage::InFlight,
            role_id: Some("approach".into()),
            agent_id: Some("approach-1".into()),
            model: Some(sonnet.clone()),
            lineage: vec![
                down("approach", "ground"),
                LineageStep {
                    from_role: "ground".into(),
                    to_role: "approach".into(),
                    direction: HandDirection::Up,
                },
            ],
            source: Some("local".into()),
            created_unix: m(95),
            updated_unix: m(1),
            timeline: vec![
                TimelineEntry {
                    at_unix: m(95),
                    body: TimelineBody::System {
                        text: "Cleared to Approach".into(),
                    },
                },
                TimelineEntry {
                    at_unix: m(80),
                    body: TimelineBody::Message {
                        from: "approach-1".into(),
                        text: "Plan: move tokenizer into lexer.rs, keep parser.rs as the AST builder. \
                               The move is mechanical, handing it to Ground."
                            .into(),
                    },
                },
                TimelineEntry {
                    at_unix: m(78),
                    body: TimelineBody::Handoff {
                        packet: packet_parser,
                    },
                },
                TimelineEntry {
                    at_unix: m(31),
                    body: TimelineBody::ToolCall {
                        agent: "ground-1".into(),
                        tool: "edit".into(),
                        summary: "src/lexer.rs (+412)  src/parser.rs (-398)".into(),
                    },
                },
                TimelineEntry {
                    at_unix: m(12),
                    body: TimelineBody::Message {
                        from: "ground-1".into(),
                        text: "Done. 214 tests pass. One helper (is_ident_start) is now pub(crate) \
                               because both files use it."
                            .into(),
                    },
                },
            ],
            ..Default::default()
        },
        Flight {
            id: "f-14".into(),
            key: "TWR-14".into(),
            title: "Port the usage folding tests to tower-core".into(),
            stage: Stage::InFlight,
            role_id: Some("ground".into()),
            agent_id: Some("ground-1".into()),
            model: Some(qwen.clone()),
            lineage: vec![down("approach", "ground")],
            source: Some("local".into()),
            created_unix: m(50),
            updated_unix: m(2),
            ..Default::default()
        },
        Flight {
            id: "f-11".into(),
            key: "PROJ-338".into(),
            title: "CSV export drops the last row when the file has no trailing newline".into(),
            stage: Stage::Gates,
            role_id: Some("approach".into()),
            agent_id: Some("approach-1".into()),
            model: Some(sonnet.clone()),
            source: Some("jira".into()),
            created_unix: m(180),
            updated_unix: m(6),
            gates: vec![
                GateResult {
                    name: "cargo test".into(),
                    state: GateState::Passed,
                    detail: Some("312 passed".into()),
                },
                GateResult {
                    name: "You".into(),
                    state: GateState::Pending,
                    detail: None,
                },
            ],
            ..Default::default()
        },
        Flight {
            id: "f-15".into(),
            key: "TWR-15".into(),
            title: "Rename towerd socket env var".into(),
            stage: Stage::NeedsYou,
            role_id: Some("ground".into()),
            agent_id: Some("ground-2".into()),
            model: Some(qwen.clone()),
            lineage: vec![down("approach", "ground")],
            source: Some("local".into()),
            created_unix: m(30),
            updated_unix: m(3),
            ..Default::default()
        },
        Flight {
            id: "f-10".into(),
            key: "TWR-10".into(),
            title: "Ladder escalation after two failed gates".into(),
            stage: Stage::Landed,
            role_id: Some("approach".into()),
            model: Some(opus.clone()),
            source: Some("local".into()),
            created_unix: m(600),
            updated_unix: m(240),
            ..Default::default()
        },
    ];

    let inbox = vec![
        InboxItem {
            id: "i-5".into(),
            kind: InboxKind::Permission,
            title: "Ground wants to run a command".into(),
            detail: "git stash  ·  TWR-15".into(),
            flight_id: Some("f-15".into()),
            agent_id: Some("ground-2".into()),
            created_unix: m(3),
            ..Default::default()
        },
        InboxItem {
            id: "i-4".into(),
            kind: InboxKind::Clearance,
            title: "Release handoff to Ground".into(),
            detail: "Write rustdoc for every pub item in sources/jira.rs  ·  TWR-13".into(),
            flight_id: Some("f-13".into()),
            created_unix: m(9),
            ..Default::default()
        },
    ];

    let endpoints = vec![
        Endpoint {
            id: "local".into(),
            name: "localhost".into(),
            base_url: "http://localhost:11434".into(),
            kind: "ollama".into(),
            health: Health::Up,
            latency_ms: Some(2),
            models: vec!["qwen3.6:27b".into()],
        },
        Endpoint {
            id: "gpu-box".into(),
            name: "gpu-box.lan".into(),
            base_url: "http://gpu-box.lan:8000".into(),
            kind: "vllm".into(),
            health: Health::Down,
            latency_ms: None,
            models: vec![],
        },
    ];

    let frequencies = vec![Frequency {
        id: "team".into(),
        name: "Team".into(),
        last_message: Some("approach-1: Ground's split looks right, running the gates".into()),
        unread: 2,
        messages: vec![FreqMessage {
            from: "approach-1".into(),
            text: "Ground's split looks right, running the gates".into(),
            at_unix: m(1),
        }],
    }];

    Snapshot {
        origin: SnapshotOrigin::Demo,
        now_unix,
        roles,
        agents,
        flights,
        inbox,
        endpoints,
        frequencies,
        config_dir: String::new(),
        flights_root: "~/Tower".into(),
        triage_role: "review".into(),
        bandit_mode: "shadow".into(),
        engines: vec![
            EngineInfo {
                id: "qwen-local".into(),
                tier: "low".into(),
                driver: DriverKind::Acp,
                model: "ollama/qwen3.6:27b".into(),
                available: false,
                why: Some("ollama pull qwen3.6:27b".into()),
                ..Default::default()
            },
            EngineInfo {
                id: "claude-sonnet".into(),
                tier: "medium".into(),
                driver: DriverKind::Pane,
                model: "sonnet".into(),
                available: true,
                trained: 12,
                ..Default::default()
            },
            EngineInfo {
                id: "claude-opus".into(),
                tier: "high".into(),
                driver: DriverKind::Pane,
                model: "opus".into(),
                effort: "high".into(),
                available: true,
                trained: 4,
                ..Default::default()
            },
        ],
        sources: vec![SourceStatus {
            name: "Team backlog".into(),
            kind: "jira".into(),
            query: "project = PROJ AND statusCategory = \"To Do\"".into(),
            role_id: Some("approach".into()),
            auto_launch: false,
            last_poll_unix: m(1),
            polling: false,
            error: None,
            flights: 2,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_single_lines() {
        let msgs = vec![
            ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                token: "t".into(),
                client: "gui".into(),
            },
            ClientMsg::GetSnapshot,
            ClientMsg::ResolveInbox {
                item_id: "i-1".into(),
                approve: true,
                option_id: None,
                text: Some("multi\nline".into()),
            },
            ClientMsg::Hook {
                agent_id: "a".into(),
                event: "Stop".into(),
                payload: serde_json::json!({"last_assistant_message": "hi\nthere"}),
            },
        ];
        for msg in msgs {
            let line = encode_line(&msg).unwrap();
            assert!(line.ends_with('\n'));
            assert_eq!(line.matches('\n').count(), 1, "one message per line");
            let back: ClientMsg = decode_line(&line).unwrap();
            assert_eq!(back, msg);
        }
        let spawn = ServerMsg::SpawnPane {
            request_id: 3,
            argv: vec!["claude".into(), "a \"quoted\" prompt".into()],
            cwd: Some("/tmp".into()),
            env: vec![("TOWER_AGENT_ID".into(), "approach-1".into())],
            title: "Approach".into(),
        };
        let back: ServerMsg = decode_line(&encode_line(&spawn).unwrap()).unwrap();
        assert_eq!(back, spawn);
    }

    #[test]
    fn demo_snapshot_round_trips_and_is_consistent() {
        let snap = demo_snapshot(1_000_000);
        let line = encode_line(&ServerMsg::Snapshot {
            snapshot: snap.clone(),
        })
        .unwrap();
        let back: ServerMsg = decode_line(&line).unwrap();
        assert_eq!(
            back,
            ServerMsg::Snapshot {
                snapshot: snap.clone()
            }
        );

        for agent in &snap.agents {
            assert!(
                snap.role(&agent.role_id).is_some(),
                "{} has a role",
                agent.id
            );
        }
        for flight in &snap.flights {
            if let Some(role) = &flight.role_id {
                assert!(snap.role(role).is_some(), "{} role exists", flight.key);
            }
            if let Some(agent) = &flight.agent_id {
                assert!(snap.agent(agent).is_some(), "{} agent exists", flight.key);
            }
        }
        for stage in Stage::ALL.iter() {
            assert!(
                snap.flights_in(*stage).count() > 0,
                "demo fills every column ({:?})",
                stage
            );
        }
    }

    #[test]
    fn unknown_fields_and_missing_new_fields_are_tolerated() {
        let msg: ServerMsg = decode_line(r#"{"type":"pong","extra":1}"#).unwrap();
        assert_eq!(msg, ServerMsg::Pong);
        // A v1-shaped flight (no brief/workdir/...) still parses.
        let f: Flight = serde_json::from_str(
            r#"{"id":"f","key":"K","title":"t","stage":"holding","created_unix":1,"updated_unix":1}"#,
        )
        .unwrap();
        assert_eq!(f.brief, "");
        assert_eq!(f.failures, 0);
    }
}
