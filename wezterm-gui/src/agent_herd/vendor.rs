//! Vendor-neutral agent detection abstraction.
//!
//! Each supported AI agent vendor gets a `SessionSource` implementation
//! that reads vendor-specific session files from disk. The
//! `VendorRegistry` collects sessions from all registered sources and
//! normalises them into a common `VendorSession` shape so the rest of
//! the agent herd logic stays vendor-agnostic.

use crate::agent_herd::{HerdActivity, HerdStatus, HerdSubagent};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Supported AI agent vendors.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentVendor {
    Claude,
    Codex,
    Copilot,
    OpenCode,
    Gemini,
    Cursor,
    Amp,
    Antigravity,
    /// Vendor not in the built-in list; carries the raw adapter id.
    Custom(String),
}

impl AgentVendor {
    /// Human-facing name used in UI labels.
    pub fn label(&self) -> &str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Copilot => "Copilot",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini",
            Self::Cursor => "Cursor",
            Self::Amp => "Amp",
            Self::Antigravity => "Antigravity",
            Self::Custom(id) => id.as_str(),
        }
    }

    /// Monochrome status dot colour. Matches the herdr sidebar palette.
    pub fn dot_color(&self) -> (u8, u8, u8) {
        match self {
            Self::Claude => (78, 205, 196),
            Self::Codex => (98, 114, 164),
            Self::Copilot => (140, 140, 140),
            Self::OpenCode => (255, 121, 198),
            Self::Gemini => (255, 180, 0),
            Self::Cursor => (88, 166, 255),
            Self::Amp => (255, 100, 100),
            Self::Antigravity => (155, 124, 255),
            Self::Custom(_) => (180, 180, 180),
        }
    }

    /// Adapter id this vendor is configured and detected under, i.e. the key
    /// used in `agent_ui.adapters` and in `PaneAgentRow::provider`.
    pub fn adapter_id(&self) -> &str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::OpenCode => "opencode",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Amp => "amp",
            Self::Antigravity => "antigravity",
            Self::Custom(id) => id.as_str(),
        }
    }

    /// Unicode glyph used in the sidebar row.
    pub fn glyph(&self) -> &'static str {
        match self {
            Self::Claude => "◈",
            Self::Codex => "◉",
            Self::Copilot => "◐",
            Self::OpenCode => "◎",
            Self::Gemini => "◆",
            Self::Cursor => "▸",
            Self::Amp => "▶",
            Self::Antigravity => "◇",
            Self::Custom(_) => "●",
        }
    }
}

/// A vendor-normalised session record, analogous to `ClaudeSession` but
/// usable for every vendor.
#[derive(Clone, Debug, PartialEq)]
pub struct VendorSession {
    pub pid: u32,
    pub vendor: AgentVendor,
    pub session_id: String,
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub status: HerdStatus,
    /// What the transcript's turn structure says, where the vendor writes one.
    /// Outranks `status` when it says the turn is over -- see
    /// [`super::TurnState`].
    pub turn: super::TurnState,
    pub blocked_reason: Option<String>,
    pub started_at: Option<SystemTime>,
    pub status_changed_at: Option<SystemTime>,
    pub subagents: Vec<HerdSubagent>,
    /// Recent transcript activity, when this vendor exposes a readable log.
    pub activity: Option<HerdActivity>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost: Option<String>,
    /// Is this a session a human is typing into?
    ///
    /// Vendors spawn agent processes that write the same session files as an
    /// interactive one: SDK harnesses, `-p` one-shots, hook children. They are
    /// not something the user can focus or resume, so the herd hides them
    /// unless `agent_ui.section.show_non_interactive` says otherwise. Vendors
    /// whose store does not distinguish the two report `true`.
    pub interactive: bool,
}

/// Reads session files from a vendor's on-disk store.
pub trait SessionSource: Send + Sync {
    /// The vendor this source handles.
    fn vendor(&self) -> AgentVendor;

    /// Collect sessions from this vendor's storage directory.
    /// `home` is the user's home directory (`dirs_next::home_dir()`).
    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession>;
}

/// Registry of all registered session sources.
pub struct VendorRegistry {
    sources: Vec<Box<dyn SessionSource>>,
}

impl Default for VendorRegistry {
    fn default() -> Self {
        crate::agent_herd::default_registry()
    }
}

impl VendorRegistry {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    pub fn register(&mut self, source: Box<dyn SessionSource>) {
        self.sources.push(source);
    }

    /// Collect sessions from every registered source.
    pub fn collect_all(&self, home: &Path) -> Vec<VendorSession> {
        let mut all = Vec::new();
        for source in &self.sources {
            let vendor = source.vendor();
            let mut sessions = source.collect_sessions(home);
            for session in &mut sessions {
                session.vendor = vendor.clone();
            }
            all.extend(sessions);
        }
        all
    }
}
