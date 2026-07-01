use crate::quad::TripleLayerQuadAllocator;
use crate::tabbar::TabBarItem;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::render::RenderScreenLineParams;
use crate::termwindow::{AgentCopyAction, AgentToolbeltAction, UIItem, UIItemType};
use config::{
    AgentAdapterConfig, AgentTelemetryField, AgentToolbeltPosition, SidebarPosition,
    SidebarTabDensity, SidebarTabMetadata, SidebarTabTitleSource, TabBarColors,
};
use mux::pane::{CachePolicy, Pane};
use mux::renderable::RenderableDimensions;
use mux::tab::PositionedPane;
use mux::Mux;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::surface::{Line, SEQ_ZERO};
use window::color::LinearRgba;
use window::{MousePress, RectF};

const INSET: f32 = 8.;
const GAP: f32 = 4.;
const PAD_X: f32 = 10.;
const ACTIVE_RAIL_W: f32 = 3.;
const ACTIVE_TEXT_GAP: f32 = 7.;
const ACTION_ICON_W: f32 = 16.;
const ACTION_ICON_GAP: f32 = 8.;
const RADIUS: f32 = 7.;
const CLOSE_ZONE_W: f32 = 34.;
const SIDEBAR_SCROLLBAR_GUTTER_W: f32 = 30.;
const SIDEBAR_SCROLLBAR_W: f32 = 10.;
const SIDEBAR_SCROLLBAR_INSET_Y: f32 = 12.;
const RESIZE_GRIP_W: usize = 6;
const AUTO_HIDE_HOVER_SLOP: isize = 0;
const AUTO_HIDE_RETAIN_SLOP: isize = 48;
const AUTO_HIDE_COLLAPSE_DELAY_MS: u64 = 0;
const AUTO_HIDE_RESIZE_GRIP_W: usize = 8;
const MIN_AUTO_HIDE_RAIL_W: usize = 48;
const AGENT_TOOLBELT_H: f32 = 32.;
const AGENT_TOOLBELT_GAP: f32 = 6.;
const AGENT_TOOLBELT_BUTTON_W: f32 = 72.;
const AGENT_TOOLBELT_MAX_W: f32 = 520.;
const AGENT_COPY_MENU_W: f32 = 216.;
const AGENT_COPY_MENU_ROW_H: f32 = 28.;

fn lerp_rgba(a: LinearRgba, b: LinearRgba, t: f32) -> LinearRgba {
    LinearRgba(
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
        a.3,
    )
}

fn opaque(color: LinearRgba) -> LinearRgba {
    LinearRgba(color.0, color.1, color.2, 1.0)
}

fn contrast_label_color(bg: LinearRgba) -> LinearRgba {
    if bg.relative_luminance() > 0.46 {
        LinearRgba(0.03, 0.03, 0.035, 1.0)
    } else {
        LinearRgba(1.0, 1.0, 1.0, 1.0)
    }
}

fn line_to_string(line: &Line) -> String {
    line.visible_cells()
        .map(|cell| cell.str().to_string())
        .collect::<String>()
}

fn is_agent_user_prompt_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("> ")
        || trimmed.starts_with("› ")
        || trimmed.starts_with("❯ ")
        || trimmed.starts_with("$ ")
        || trimmed.starts_with("# ")
}

fn is_agent_prompt_or_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty()
        || is_agent_user_prompt_line(line)
        || trimmed.contains("ctx:")
        || trimmed.contains("auto mode")
        || trimmed.contains("shift+tab")
        || trimmed.contains("for agents")
}

fn compact_label(value: &str, fallback: &str) -> String {
    let label: String = value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect();
    if label.is_empty() {
        fallback.to_string()
    } else {
        label
    }
}

fn compact_tab_symbol(
    title: &str,
    tab_idx: usize,
    agent_kind: Option<&AgentKind>,
    command: Option<&str>,
    pane_title: Option<&str>,
) -> String {
    if let Some(kind) = agent_kind {
        return match kind {
            AgentKind::Claude => "Cl".to_string(),
            AgentKind::Codex => "Cx".to_string(),
            AgentKind::Gemini => "G".to_string(),
            AgentKind::OpenCode => "Oc".to_string(),
            AgentKind::Copilot => "Cp".to_string(),
            AgentKind::Cursor => "Cu".to_string(),
            AgentKind::Amp => "A".to_string(),
            AgentKind::Unknown(value) => compact_label(value, "Ag"),
        };
    }

    let lower_title = title.to_ascii_lowercase();
    let lower_pane_title = pane_title.map(str::to_ascii_lowercase).unwrap_or_default();
    if lower_title.contains("worktree") || lower_pane_title.contains("worktree") {
        return "Wt".to_string();
    }

    match command
        .map(|cmd| basename(cmd).to_ascii_lowercase())
        .as_deref()
    {
        Some("claude" | "claude-code" | "claude_code") => "Cl".to_string(),
        Some("codex" | "openai-codex" | "openai_codex") => "Cx".to_string(),
        Some("gemini" | "gemini-cli" | "gemini_cli") => "G".to_string(),
        Some("opencode" | "open-code" | "open_code") => "Oc".to_string(),
        Some("copilot" | "gh-copilot" | "github-copilot") => "Cp".to_string(),
        Some("cursor") => "Cu".to_string(),
        Some("amp") => "A".to_string(),
        Some("bash" | "fish" | "nu" | "powershell" | "pwsh" | "sh" | "zsh") => "$".to_string(),
        Some("vi" | "vim" | "nvim") => "Vi".to_string(),
        Some("emacs") => "Em".to_string(),
        Some("git" | "gh" | "lazygit") => "Gt".to_string(),
        Some("ssh" | "mosh") => "S".to_string(),
        Some("python" | "python3" | "pytest" | "uv") => "Py".to_string(),
        Some("node" | "npm" | "pnpm" | "bun" | "yarn") => "JS".to_string(),
        Some("cargo" | "rustc" | "rust-analyzer") => "Rs".to_string(),
        Some("docker" | "podman") => "Dk".to_string(),
        Some("make" | "cmake" | "ninja") => "Mk".to_string(),
        Some("htop" | "top" | "btop") => "Tp".to_string(),
        Some("less" | "man") => "Pg".to_string(),
        _ => {
            let title = title.trim();
            let title = title
                .strip_prefix(&format!("{}:", tab_idx + 1))
                .map(str::trim_start)
                .unwrap_or(title);
            title
                .chars()
                .find(|c| c.is_alphanumeric())
                .map(|c| c.to_uppercase().collect::<String>())
                .filter(|symbol| !symbol.is_empty())
                .unwrap_or_else(|| ((tab_idx + 1) % 10).to_string())
        }
    }
}

fn compact_tab_color(
    title: &str,
    tab_idx: usize,
    agent_kind: Option<&AgentKind>,
    command: Option<&str>,
    pane_title: Option<&str>,
) -> LinearRgba {
    if let Some(kind) = agent_kind {
        return match kind {
            AgentKind::Claude => LinearRgba(0.86, 0.48, 0.32, 1.0),
            AgentKind::Codex => LinearRgba(0.24, 0.64, 0.48, 1.0),
            AgentKind::Gemini => LinearRgba(0.28, 0.52, 0.92, 1.0),
            AgentKind::OpenCode => LinearRgba(0.22, 0.66, 0.70, 1.0),
            AgentKind::Copilot => LinearRgba(0.34, 0.66, 0.38, 1.0),
            AgentKind::Cursor => LinearRgba(0.44, 0.42, 0.82, 1.0),
            AgentKind::Amp => LinearRgba(0.74, 0.36, 0.68, 1.0),
            AgentKind::Unknown(_) => LinearRgba(0.58, 0.50, 0.82, 1.0),
        };
    }

    let lower_title = title.to_ascii_lowercase();
    let lower_pane_title = pane_title.map(str::to_ascii_lowercase).unwrap_or_default();
    if lower_title.contains("worktree") || lower_pane_title.contains("worktree") {
        return LinearRgba(0.50, 0.58, 0.42, 1.0);
    }

    match command
        .map(|cmd| basename(cmd).to_ascii_lowercase())
        .as_deref()
    {
        Some("bash" | "fish" | "nu" | "powershell" | "pwsh" | "sh" | "zsh") => {
            LinearRgba(0.45, 0.48, 0.52, 1.0)
        }
        Some("vi" | "vim" | "nvim" | "emacs") => LinearRgba(0.34, 0.58, 0.42, 1.0),
        Some("git" | "gh" | "lazygit") => LinearRgba(0.86, 0.36, 0.26, 1.0),
        Some("ssh" | "mosh") => LinearRgba(0.76, 0.60, 0.28, 1.0),
        Some("python" | "python3" | "pytest" | "uv") => LinearRgba(0.30, 0.54, 0.76, 1.0),
        Some("node" | "npm" | "pnpm" | "bun" | "yarn") => LinearRgba(0.42, 0.66, 0.34, 1.0),
        Some("cargo" | "rustc" | "rust-analyzer") => LinearRgba(0.72, 0.42, 0.28, 1.0),
        Some("docker" | "podman") => LinearRgba(0.22, 0.50, 0.78, 1.0),
        Some("make" | "cmake" | "ninja") => LinearRgba(0.62, 0.52, 0.42, 1.0),
        _ => compact_tab_fallback_color(title, tab_idx),
    }
}

fn compact_tab_fallback_color(title: &str, tab_idx: usize) -> LinearRgba {
    const COLORS: [LinearRgba; 10] = [
        LinearRgba(0.86, 0.24, 0.28, 1.0),
        LinearRgba(0.93, 0.48, 0.18, 1.0),
        LinearRgba(0.86, 0.68, 0.22, 1.0),
        LinearRgba(0.30, 0.66, 0.38, 1.0),
        LinearRgba(0.18, 0.62, 0.58, 1.0),
        LinearRgba(0.20, 0.50, 0.78, 1.0),
        LinearRgba(0.45, 0.38, 0.82, 1.0),
        LinearRgba(0.72, 0.34, 0.72, 1.0),
        LinearRgba(0.74, 0.38, 0.48, 1.0),
        LinearRgba(0.48, 0.52, 0.58, 1.0),
    ];
    let hash = title.bytes().fold(tab_idx as usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    COLORS[hash % COLORS.len()]
}

fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(value)
        .to_string()
}

fn title_case_value(value: &str, command_like: bool) -> Option<String> {
    let raw = if command_like {
        value
            .split_whitespace()
            .next()
            .map(basename)
            .unwrap_or_else(|| basename(value))
    } else {
        value.trim().to_string()
    };
    let raw = raw.trim();
    if raw.is_empty() || is_generic_shell_title(raw, None) {
        return None;
    }

    let mut label = String::new();
    for word in raw
        .split(['-', '_', '.', ' '])
        .filter(|word| !word.is_empty())
    {
        if !label.is_empty() {
            label.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            label.extend(first.to_uppercase());
            label.push_str(chars.as_str());
        }
    }

    if label.is_empty() {
        None
    } else {
        Some(label)
    }
}

fn title_case_command(value: &str) -> Option<String> {
    title_case_value(value, true)
}

fn title_case_label(value: &str) -> Option<String> {
    title_case_value(value, false)
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn is_generic_shell_title(title: &str, command: Option<&str>) -> bool {
    let title = title.trim();
    if title.is_empty() {
        return true;
    }

    let title_lower = title.to_ascii_lowercase();
    let generic_titles = [
        "bash",
        "cmd",
        "cmd.exe",
        "fish",
        "nu",
        "powershell",
        "pwsh",
        "sh",
        "tgzterminal",
        "wezterm",
        "wezterm-gui",
        "wsl",
        "zsh",
    ];
    if generic_titles.contains(&title_lower.as_str()) {
        return true;
    }

    command
        .map(|command| title_lower == command.trim().to_ascii_lowercase())
        .unwrap_or(false)
}

fn live_tab_title(tab_title: &str, fallback: &str, command: Option<&str>) -> Option<String> {
    let title = tab_title.trim().trim_start_matches('*').trim();
    if title.is_empty() || title == fallback || is_generic_shell_title(title, command) {
        None
    } else {
        Some(title.to_string())
    }
}

fn looks_like_version_fragment(title: &str) -> bool {
    let title = title.trim();
    !title.is_empty()
        && !title.chars().any(|ch| ch.is_alphabetic())
        && title
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | ' ' | ':' | '/'))
}

fn pane_app_title(pane: &Arc<dyn Pane>, command: Option<&str>) -> Option<String> {
    let vars = pane.copy_user_vars();
    for key in ["agent.title", "agent.name", "agent.kind"] {
        if let Some(value) = user_var(&vars, key) {
            if let Some(title) = title_case_label(value) {
                return Some(title);
            }
        }
    }
    for key in ["WEZTERM_PROG", "PROG"] {
        if let Some(value) = user_var(&vars, key) {
            if let Some(title) = title_case_command(value) {
                return Some(title);
            }
        }
    }

    command.and_then(title_case_command)
}

fn is_worktree_pane(pane: &Arc<dyn Pane>) -> bool {
    pane.copy_user_vars().contains_key("tgzterminal.worktree")
        || pane.get_title().trim() == "Worktree"
}

fn clean_live_title(title: &str, fallback: &str, command: Option<&str>) -> Option<String> {
    let title = live_tab_title(title, fallback, command)?;
    let mut title = title.trim().to_string();
    if let Some(version_idx) = title.find(" v") {
        let next_is_digit = title[version_idx + 2..]
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit());
        if next_is_digit {
            title.truncate(version_idx);
        }
    }

    let title = title.trim();
    if title.is_empty()
        || is_generic_shell_title(title, command)
        || looks_like_version_fragment(title)
    {
        None
    } else {
        Some(title.to_string())
    }
}

fn pane_working_dir(pane: &Arc<dyn Pane>) -> Option<PathBuf> {
    pane.get_current_working_dir(CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
}

fn find_git_branch(mut dir: &Path) -> Option<String> {
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
            return parse_git_head(&head);
        }
        if git.is_file() {
            let indirection = std::fs::read_to_string(&git).ok()?;
            let Some(git_dir) = indirection.strip_prefix("gitdir:").map(str::trim) else {
                return None;
            };
            let git_dir = if Path::new(git_dir).is_absolute() {
                PathBuf::from(git_dir)
            } else {
                dir.join(git_dir)
            };
            let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
            return parse_git_head(&head);
        }
        dir = dir.parent()?;
    }
}

fn parse_git_head(head: &str) -> Option<String> {
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(|branch| branch.to_string())
        .or_else(|| {
            if head.is_empty() {
                None
            } else {
                Some(head.chars().take(8).collect())
            }
        })
}

fn user_var<'a>(vars: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    vars.get(key)
        .or_else(|| vars.get(&key.replace('.', "_")))
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentKind {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Copilot,
    Cursor,
    Amp,
    Unknown(String),
}

impl AgentKind {
    fn from_hint(hint: &str) -> Option<Self> {
        let lower = basename(hint).to_ascii_lowercase();
        match lower.as_str() {
            "claude" | "claude-code" | "claude_code" => Some(Self::Claude),
            "codex" | "openai-codex" | "openai_codex" => Some(Self::Codex),
            "gemini" | "gemini-cli" | "gemini_cli" => Some(Self::Gemini),
            "opencode" | "open-code" | "open_code" => Some(Self::OpenCode),
            "copilot" | "gh-copilot" | "github-copilot" => Some(Self::Copilot),
            "cursor" => Some(Self::Cursor),
            "amp" => Some(Self::Amp),
            _ => None,
        }
    }

    fn from_user_var(hint: &str) -> Self {
        Self::from_hint(hint).unwrap_or_else(|| Self::Unknown(hint.trim().to_string()))
    }

    fn label(&self) -> &str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini",
            Self::OpenCode => "OpenCode",
            Self::Copilot => "Copilot",
            Self::Cursor => "Cursor",
            Self::Amp => "Amp",
            Self::Unknown(value) => value.as_str(),
        }
    }

    fn config_key(&self) -> Option<&'static str> {
        match self {
            Self::Claude => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Gemini => Some("gemini"),
            Self::OpenCode => Some("opencode"),
            Self::Copilot => Some("copilot"),
            Self::Cursor => Some("cursor"),
            Self::Amp => Some("amp"),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentStatus {
    Unknown,
    Idle,
    Running,
    WaitingForInput,
    Streaming,
    Exited,
}

impl AgentStatus {
    fn from_hint(hint: Option<&str>) -> Self {
        let Some(hint) = hint else {
            return Self::Unknown;
        };
        match hint.trim().to_ascii_lowercase().as_str() {
            "idle" => Self::Idle,
            "running" | "busy" | "working" => Self::Running,
            "waiting" | "waiting_for_input" | "waiting-for-input" | "input" => {
                Self::WaitingForInput
            }
            "streaming" | "responding" => Self::Streaming,
            "exited" | "exit" | "dead" => Self::Exited,
            _ => Self::Unknown,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::WaitingForInput => "waiting",
            Self::Streaming => "streaming",
            Self::Exited => "exited",
        }
    }
}

#[derive(Clone, Debug, Default)]
struct AgentActions {
    interrupt: bool,
    copy_summary: bool,
    attach: bool,
    resume: bool,
    open_logs: bool,
}

#[derive(Clone, Debug)]
struct AgentPaneState {
    kind: AgentKind,
    status: AgentStatus,
    model: Option<String>,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost: Option<String>,
    actions: AgentActions,
}

trait AgentAdapter {
    fn supported_actions(&self, vars: &HashMap<String, String>) -> AgentActions;
}

struct PassiveAgentAdapter;

impl AgentAdapter for PassiveAgentAdapter {
    fn supported_actions(&self, _vars: &HashMap<String, String>) -> AgentActions {
        AgentActions {
            interrupt: true,
            copy_summary: true,
            attach: false,
            resume: false,
            open_logs: false,
        }
    }
}

fn parse_u64_var(vars: &HashMap<String, String>, key: &str) -> Option<u64> {
    user_var(vars, key).and_then(|value| value.replace(',', "").parse::<u64>().ok())
}

fn has_agent_metadata_evidence(vars: &HashMap<String, String>) -> bool {
    [
        "agent.model",
        "agent.status",
        "agent.session_id",
        "agent.session",
        "agent.input_tokens",
        "agent.output_tokens",
        "agent.total_tokens",
        "agent.cost",
        "agent.estimated_cost",
    ]
    .iter()
    .any(|key| user_var(vars, key).is_some())
}

fn title_agent_hint(title: &str) -> Option<AgentKind> {
    let lower = title.to_ascii_lowercase();
    for marker in [
        "claude code",
        "claude",
        "codex",
        "gemini",
        "opencode",
        "open code",
        "copilot",
        "cursor",
        "amp",
    ] {
        if lower.contains(marker) {
            return AgentKind::from_hint(marker);
        }
    }
    None
}

impl crate::TermWindow {
    pub fn sidebar_is_active(&self) -> bool {
        self.show_tab_bar && self.config.sidebar_enabled
    }

    pub fn sidebar_width(&self) -> usize {
        if !self.sidebar_is_active() {
            0
        } else if self.config.sidebar_auto_hide && !self.sidebar_auto_hide_open {
            self.sidebar_collapsed_width()
        } else {
            self.sidebar_expanded_width()
        }
    }

    fn sidebar_expanded_width(&self) -> usize {
        self.sidebar_drag_width
            .unwrap_or(self.config.sidebar_width_px)
            .max(self.sidebar_collapsed_width())
    }

    pub fn sidebar_collapsed_width(&self) -> usize {
        if self.config.sidebar_auto_hide {
            self.config
                .sidebar_collapsed_width_px
                .max(MIN_AUTO_HIDE_RAIL_W)
        } else {
            self.config.sidebar_collapsed_width_px
        }
    }

    pub fn sidebar_reserved_width(&self) -> usize {
        if !self.sidebar_is_active() {
            0
        } else if let Some((item, _)) = self.dragging.as_ref() {
            match item.item_type {
                UIItemType::SidebarResize { start_width } => start_width,
                _ => self.sidebar_width(),
            }
        } else if self.config.sidebar_auto_hide {
            self.sidebar_collapsed_width()
        } else {
            self.sidebar_width()
        }
    }

    pub(crate) fn update_sidebar_auto_hide_state(&mut self) -> bool {
        let was_open = self.sidebar_auto_hide_open;
        let expanded = self.sidebar_expanded_width();
        let now_open = self.sidebar_auto_hide_should_open(expanded, was_open);
        if now_open {
            if !was_open {
                self.sidebar_auto_hide_open = true;
                self.quad_generation += 1;
            }
            self.sidebar_auto_hide_close_after = None;
            return !was_open;
        }

        if was_open {
            return self.schedule_sidebar_auto_hide_close();
        }

        self.sidebar_auto_hide_close_after = None;
        false
    }

    pub(crate) fn schedule_sidebar_auto_hide_close(&mut self) -> bool {
        if !self.sidebar_is_active()
            || !self.config.sidebar_auto_hide
            || !self.sidebar_auto_hide_open
        {
            self.sidebar_auto_hide_close_after = None;
            return false;
        }

        let now = Instant::now();
        if AUTO_HIDE_COLLAPSE_DELAY_MS == 0 {
            self.sidebar_auto_hide_open = false;
            self.sidebar_auto_hide_close_after = None;
            self.quad_generation += 1;
            return true;
        }

        if let Some(close_after) = self.sidebar_auto_hide_close_after {
            if now >= close_after {
                self.sidebar_auto_hide_open = false;
                self.sidebar_auto_hide_close_after = None;
                self.quad_generation += 1;
                return true;
            }
            *self.has_animation.borrow_mut() = Some(close_after);
            return false;
        }

        let close_after = now + Duration::from_millis(AUTO_HIDE_COLLAPSE_DELAY_MS);
        self.sidebar_auto_hide_close_after = Some(close_after);
        *self.has_animation.borrow_mut() = Some(close_after);
        true
    }

    fn settle_sidebar_auto_hide_close(&mut self) {
        if !self.sidebar_is_active() || !self.config.sidebar_auto_hide {
            self.sidebar_auto_hide_close_after = None;
            return;
        }

        let Some(close_after) = self.sidebar_auto_hide_close_after else {
            return;
        };

        if Instant::now() >= close_after {
            if self.sidebar_auto_hide_open {
                self.sidebar_auto_hide_open = false;
                self.quad_generation += 1;
            }
            self.sidebar_auto_hide_close_after = None;
        } else {
            *self.has_animation.borrow_mut() = Some(close_after);
        }
    }

    fn sidebar_auto_hide_should_open(&self, expanded: usize, was_open: bool) -> bool {
        if !self.sidebar_is_active() || !self.config.sidebar_auto_hide {
            return false;
        }

        if let Some((item, _)) = self.dragging.as_ref() {
            if matches!(
                item.item_type,
                UIItemType::SidebarResize { .. }
                    | UIItemType::SidebarScrollThumb
                    | UIItemType::SidebarTab { .. }
            ) {
                return true;
            }
        }

        let Some(event) = &self.current_mouse_event else {
            return false;
        };

        let border = self.get_os_border();
        let x = event.coords.x;
        let y = event.coords.y;
        let top = border.top.get() as isize;
        let bottom = self
            .dimensions
            .pixel_height
            .saturating_sub(border.bottom.get()) as isize;
        if y < top || y >= bottom {
            return false;
        }

        let collapsed = self.sidebar_collapsed_width() as isize;
        let expanded = expanded as isize;
        let reveal_width = collapsed + AUTO_HIDE_HOVER_SLOP;
        let retain_width = if was_open {
            expanded + AUTO_HIDE_RETAIN_SLOP
        } else {
            reveal_width
        };
        match self.config.sidebar_position {
            SidebarPosition::Left => {
                let left = border.left.get() as isize;
                x >= left && x <= left + retain_width
            }
            SidebarPosition::Right => {
                let right = self
                    .dimensions
                    .pixel_width
                    .saturating_sub(border.right.get()) as isize;
                x <= right && x >= right - retain_width
            }
        }
    }

    fn sidebar_metadata_rows_enabled(&self) -> bool {
        self.config.sidebar_tab_hover_details
            && matches!(
                self.config.sidebar_tab_density,
                SidebarTabDensity::Comfortable
            )
    }

    fn agent_adapter_enabled(&self, kind: &AgentKind) -> bool {
        self.agent_adapter_config(kind)
            .map(|adapter| adapter.enabled)
            .unwrap_or(true)
    }

    fn agent_adapter_config(&self, kind: &AgentKind) -> Option<&AgentAdapterConfig> {
        let adapters = &self.config.agent_ui.adapters;
        match kind.config_key() {
            Some("claude") => Some(&adapters.claude),
            Some("codex") => Some(&adapters.codex),
            Some("gemini") => Some(&adapters.gemini),
            Some("opencode") => Some(&adapters.opencode),
            Some("copilot") => Some(&adapters.copilot),
            Some("cursor") => Some(&adapters.cursor),
            Some("amp") => Some(&adapters.amp),
            Some(_) | None => None,
        }
    }

    fn configured_agent_match(&self, process: Option<&str>, title: &str) -> Option<AgentKind> {
        let process = process.map(|process| basename(process).to_ascii_lowercase());
        let title = title.to_ascii_lowercase();
        let adapters = &self.config.agent_ui.adapters;
        let configured = [
            (&adapters.claude, AgentKind::Claude),
            (&adapters.codex, AgentKind::Codex),
            (&adapters.gemini, AgentKind::Gemini),
            (&adapters.opencode, AgentKind::OpenCode),
            (&adapters.copilot, AgentKind::Copilot),
            (&adapters.cursor, AgentKind::Cursor),
            (&adapters.amp, AgentKind::Amp),
        ];

        for (adapter, kind) in configured {
            if !adapter.enabled {
                continue;
            }
            if let Some(process) = &process {
                if adapter
                    .process_names
                    .iter()
                    .any(|name| basename(name).eq_ignore_ascii_case(process))
                {
                    return Some(kind);
                }
            }
            if adapter.title_patterns.iter().any(|pattern| {
                let pattern = pattern.trim().to_ascii_lowercase();
                !pattern.is_empty() && title.contains(&pattern)
            }) {
                return Some(kind);
            }
        }

        None
    }

    fn agent_supported_actions(
        &self,
        _kind: &AgentKind,
        vars: &HashMap<String, String>,
    ) -> AgentActions {
        PassiveAgentAdapter.supported_actions(vars)
    }

    fn detect_agent_pane(&self, pane: &Arc<dyn Pane>) -> Option<AgentPaneState> {
        if !self.config.agent_ui.enabled {
            return None;
        }

        let vars = pane.copy_user_vars();
        let explicit_kind = user_var(&vars, "agent.kind").map(AgentKind::from_user_var);
        let foreground_process = pane.get_foreground_process_name(CachePolicy::AllowStale);
        let pane_title = pane.get_title();
        let process_kind = if self.config.agent_ui.detect_processes {
            foreground_process.as_deref().and_then(AgentKind::from_hint)
        } else {
            None
        };
        let title_kind = if self.config.agent_ui.detect_processes {
            title_agent_hint(&pane_title)
        } else {
            None
        };
        let configured_kind = if self.config.agent_ui.detect_processes {
            self.configured_agent_match(foreground_process.as_deref(), &pane_title)
        } else {
            None
        };
        let metadata_kind = if has_agent_metadata_evidence(&vars) {
            Some(AgentKind::Unknown("Agent".to_string()))
        } else {
            None
        };

        let kind = explicit_kind
            .or(process_kind)
            .or(title_kind)
            .or(configured_kind)
            .or(metadata_kind)?;
        if !self.agent_adapter_enabled(&kind) {
            return None;
        }

        let status = AgentStatus::from_hint(user_var(&vars, "agent.status"));
        let cwd = pane_working_dir(pane);
        let actions = self.agent_supported_actions(&kind, &vars);
        Some(AgentPaneState {
            kind,
            status,
            model: user_var(&vars, "agent.model").map(ToString::to_string),
            session_id: user_var(&vars, "agent.session_id")
                .or_else(|| user_var(&vars, "agent.session"))
                .map(ToString::to_string),
            cwd,
            input_tokens: parse_u64_var(&vars, "agent.input_tokens"),
            output_tokens: parse_u64_var(&vars, "agent.output_tokens"),
            cost: user_var(&vars, "agent.cost")
                .or_else(|| user_var(&vars, "agent.estimated_cost"))
                .map(ToString::to_string),
            actions,
        })
    }

    pub(crate) fn agent_pane_summary(&self, pane: &Arc<dyn Pane>) -> String {
        let title = pane.get_title();
        let Some(agent) = self.detect_agent_pane(pane) else {
            return format!("Pane {}: {}", pane.pane_id(), title);
        };

        let mut lines = vec![
            format!("Agent: {}", agent.kind.label()),
            format!("Pane: {}", pane.pane_id()),
        ];
        if !title.trim().is_empty() {
            lines.push(format!("Title: {}", title.trim()));
        }
        lines.push(format!("Status: {}", agent.status.label()));
        if let Some(model) = agent.model {
            lines.push(format!("Model: {model}"));
        }
        if let Some(session_id) = agent.session_id {
            lines.push(format!("Session: {session_id}"));
        }
        if let Some(cwd) = agent.cwd {
            lines.push(format!("CWD: {}", cwd.display()));
        }
        if let Some(tokens) = agent.input_tokens {
            lines.push(format!("Input tokens: {tokens}"));
        }
        if let Some(tokens) = agent.output_tokens {
            lines.push(format!("Output tokens: {tokens}"));
        }
        if let Some(cost) = agent.cost {
            lines.push(format!("Cost: {cost}"));
        }
        lines.join("\n")
    }

    pub(crate) fn agent_pane_conversation_text(&self, pane: &Arc<dyn Pane>) -> String {
        let dims = pane.get_dimensions();
        let end = dims.physical_top + dims.viewport_rows as isize;
        let max_rows = 5000;
        let start = dims.scrollback_top.max(end.saturating_sub(max_rows));
        let mut lines = Vec::new();

        for logical in pane.get_logical_lines(start..end) {
            let text = line_to_string(&logical.logical).trim_end().to_string();
            lines.push(text);
        }

        while lines
            .last()
            .map(|line| line.trim().is_empty())
            .unwrap_or(false)
        {
            lines.pop();
        }

        lines.join("\n")
    }

    pub(crate) fn agent_pane_last_message_text(&self, pane: &Arc<dyn Pane>) -> String {
        let conversation = self.agent_pane_conversation_text(pane);
        if conversation.trim().is_empty() {
            return self.agent_pane_summary(pane);
        }

        let mut lines: Vec<&str> = conversation.lines().collect();
        while lines
            .last()
            .map(|line| is_agent_prompt_or_status_line(line))
            .unwrap_or(false)
        {
            lines.pop();
        }

        let Some(last_content_idx) = lines.iter().rposition(|line| !line.trim().is_empty()) else {
            return conversation;
        };
        lines.truncate(last_content_idx + 1);

        let start_idx = lines
            .iter()
            .enumerate()
            .rev()
            .skip(1)
            .find_map(|(idx, line)| {
                if is_agent_user_prompt_line(line) {
                    Some(idx + 1)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                lines
                    .iter()
                    .enumerate()
                    .rev()
                    .skip(1)
                    .find_map(|(idx, line)| {
                        if line.trim().is_empty() {
                            Some(idx + 1)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0)
            });

        let message = lines[start_idx..].join("\n").trim().to_string();
        if message.is_empty() {
            conversation
        } else {
            message
        }
    }

    fn sidebar_row_height(&self) -> usize {
        let cell = self.render_metrics.cell_size.height as usize;
        match self.config.sidebar_tab_density {
            SidebarTabDensity::Comfortable if self.sidebar_metadata_rows_enabled() => {
                (cell * 2 + 8).max(44)
            }
            SidebarTabDensity::Comfortable => (cell + 10).max(34),
            SidebarTabDensity::Compact => (cell + 6).max(28),
        }
    }

    pub(crate) fn sidebar_search_matches(&self, query: &str) -> Vec<usize> {
        self.tab_bar
            .items()
            .iter()
            .filter_map(|entry| match entry.item {
                TabBarItem::Tab { tab_idx, .. } => {
                    let (title, metadata) = self.sidebar_tab_labels(tab_idx, &entry.title);
                    Some((tab_idx, title, metadata))
                }
                _ => None,
            })
            .filter(|(_, title, metadata)| self.sidebar_query_matches(title, metadata, query))
            .map(|(tab_idx, _, _)| tab_idx)
            .collect()
    }

    pub(crate) fn activate_first_sidebar_search_match(&mut self) {
        let Some(search) = &self.sidebar_search else {
            return;
        };
        if let Some(idx) = self.sidebar_search_matches(&search.query).first().copied() {
            self.activate_tab(idx as isize).ok();
        }
    }

    fn sidebar_tab_count(&self) -> usize {
        let query = self
            .sidebar_search
            .as_ref()
            .map(|state| state.query.as_str());
        self.tab_bar
            .items()
            .iter()
            .filter(|entry| match entry.item {
                TabBarItem::Tab { tab_idx, .. } => match query {
                    Some(query) if !query.is_empty() => {
                        let (title, metadata) = self.sidebar_tab_labels(tab_idx, &entry.title);
                        self.sidebar_query_matches(&title, &metadata, query)
                    }
                    _ => true,
                },
                _ => false,
            })
            .count()
    }

    fn sidebar_query_matches(&self, title: &str, metadata: &[String], query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let query = query.to_lowercase();
        title.to_lowercase().contains(&query)
            || metadata
                .iter()
                .any(|item| item.to_lowercase().contains(&query))
    }

    fn sidebar_tab_row_capacity(&self) -> usize {
        let border = self.get_os_border();
        let width = self.sidebar_width();
        if width == 0 {
            return 0;
        }

        let top = border.top.get() as f32;
        let height = (self.dimensions.pixel_height as f32
            - border.top.get() as f32
            - border.bottom.get() as f32)
            .max(0.);
        let row_height = self.sidebar_row_height() as f32;
        let mut list_top = top + INSET;
        if width > 96 {
            list_top += row_height + GAP;
        }
        let bottom_button_rows = if self.sidebar_width() > 180 { 2. } else { 1. };
        let new_tab_y = top + height - INSET - row_height;
        let list_height =
            (new_tab_y - GAP - (bottom_button_rows - 1.) * (row_height + GAP) - list_top).max(0.);
        ((list_height + GAP) / (row_height + GAP)).floor() as usize
    }

    pub(crate) fn scroll_sidebar_tabs(&mut self, wheel_delta: isize) -> bool {
        let total = self.sidebar_tab_count();
        let visible = self.sidebar_tab_row_capacity();
        let max_offset = total.saturating_sub(visible);
        if max_offset == 0 {
            let changed = self.sidebar_scroll_offset != 0;
            self.sidebar_scroll_offset = 0;
            return changed;
        }

        let step = wheel_delta.unsigned_abs().max(1);
        let next = if wheel_delta > 0 {
            self.sidebar_scroll_offset.saturating_sub(step)
        } else {
            self.sidebar_scroll_offset
                .saturating_add(step)
                .min(max_offset)
        };
        if next == self.sidebar_scroll_offset {
            false
        } else {
            self.sidebar_scroll_offset = next;
            true
        }
    }

    fn sidebar_scroll_track_bounds(&self) -> Option<(f32, f32, usize, usize)> {
        if !self.config.sidebar_scroll_bar {
            return None;
        }

        let total = self.sidebar_tab_count();
        let visible = self.sidebar_tab_row_capacity();
        if total <= visible || visible == 0 {
            return None;
        }

        let border = self.get_os_border();
        let top = border.top.get() as f32;
        let height = (self.dimensions.pixel_height as f32
            - border.top.get() as f32
            - border.bottom.get() as f32)
            .max(0.);
        let row_height = self.sidebar_row_height() as f32;
        let mut list_top = top + INSET;
        if self.sidebar_width() > 96 {
            list_top += row_height + GAP;
        }
        let bottom_button_rows = if self.sidebar_width() > 180 { 2. } else { 1. };
        let new_tab_y = top + height - INSET - row_height;
        let list_height =
            (new_tab_y - GAP - (bottom_button_rows - 1.) * (row_height + GAP) - list_top).max(0.);
        let list_top = list_top + SIDEBAR_SCROLLBAR_INSET_Y;
        let list_height = list_height - SIDEBAR_SCROLLBAR_INSET_Y * 2.;
        if list_height <= 0. {
            None
        } else {
            Some((list_top, list_height, visible, total))
        }
    }

    fn sidebar_scroll_thumb_bounds(&self) -> Option<(f32, f32, usize)> {
        let (track_y, track_h, visible, total) = self.sidebar_scroll_track_bounds()?;
        let max_offset = total.saturating_sub(visible);
        let thumb_h = (track_h * visible as f32 / total as f32)
            .max(self.sidebar_row_height() as f32 * 0.75)
            .min(track_h);
        let scroll_range = (track_h - thumb_h).max(0.);
        let thumb_y = if max_offset == 0 {
            track_y
        } else {
            track_y + scroll_range * self.sidebar_scroll_offset as f32 / max_offset as f32
        };
        Some((thumb_y, thumb_h, max_offset))
    }

    pub(crate) fn scroll_sidebar_tabs_page_toward(&mut self, y: isize) -> bool {
        let Some((thumb_y, thumb_h, _)) = self.sidebar_scroll_thumb_bounds() else {
            return false;
        };
        if (y as f32) < thumb_y {
            self.scroll_sidebar_tabs(self.sidebar_tab_row_capacity() as isize)
        } else if (y as f32) > thumb_y + thumb_h {
            self.scroll_sidebar_tabs(-(self.sidebar_tab_row_capacity() as isize))
        } else {
            false
        }
    }

    pub(crate) fn scroll_sidebar_thumb_to(&mut self, y: isize) -> bool {
        let Some((track_y, track_h, visible, total)) = self.sidebar_scroll_track_bounds() else {
            return false;
        };
        let max_offset = total.saturating_sub(visible);
        if max_offset == 0 {
            return false;
        }
        let thumb_h = (track_h * visible as f32 / total as f32)
            .max(self.sidebar_row_height() as f32 * 0.75)
            .min(track_h);
        let scroll_range = (track_h - thumb_h).max(0.);
        if scroll_range <= 0. {
            return false;
        }
        let thumb_top = (y as f32 - thumb_h * 0.5).clamp(track_y, track_y + scroll_range);
        let next = (((thumb_top - track_y) / scroll_range) * max_offset as f32).round() as usize;
        if next == self.sidebar_scroll_offset {
            false
        } else {
            self.sidebar_scroll_offset = next;
            true
        }
    }

    fn sidebar_tab_labels(&self, tab_idx: usize, fallback_title: &Line) -> (String, Vec<String>) {
        let fallback = line_to_string(fallback_title).trim().to_string();
        let Some(tab) = Mux::get()
            .get_window(self.mux_window_id)
            .and_then(|window| window.get_by_idx(tab_idx).cloned())
        else {
            return (fallback, Vec::new());
        };
        let active_pane = tab.get_active_pane();
        let panes = tab.iter_panes_ignoring_zoom();
        let pane = active_pane
            .as_ref()
            .filter(|pane| !is_worktree_pane(pane))
            .cloned()
            .or_else(|| {
                panes
                    .iter()
                    .find(|pos| !is_worktree_pane(&pos.pane))
                    .map(|pos| pos.pane.clone())
            })
            .or(active_pane);
        let cwd = pane.as_ref().and_then(pane_working_dir);
        let git_branch = cwd.as_deref().and_then(find_git_branch);
        let command = pane
            .as_ref()
            .and_then(|pane| pane.get_foreground_process_name(CachePolicy::AllowStale))
            .map(|name| basename(&name));
        let tab_title = tab.get_title();
        let pane_title = pane.as_ref().map(|pane| pane.get_title());
        let app_title = pane
            .as_ref()
            .and_then(|pane| pane_app_title(pane, command.as_deref()));

        let title = match self.config.sidebar_tab_title_source {
            SidebarTabTitleSource::Title => {
                if tab_title.is_empty() {
                    fallback.clone()
                } else {
                    tab_title
                }
            }
            SidebarTabTitleSource::Command => command
                .clone()
                .filter(|cmd| !cmd.is_empty())
                .unwrap_or_else(|| fallback.clone()),
            SidebarTabTitleSource::WorkingDirectory => pane_title
                .as_deref()
                .and_then(|title| clean_live_title(title, &fallback, command.as_deref()))
                .or_else(|| clean_live_title(&tab_title, &fallback, command.as_deref()))
                .or(app_title)
                .or_else(|| cwd.as_deref().map(path_label).filter(|cwd| !cwd.is_empty()))
                .unwrap_or_else(|| fallback.clone()),
            SidebarTabTitleSource::GitBranch => git_branch
                .clone()
                .filter(|branch| !branch.is_empty())
                .unwrap_or_else(|| fallback.clone()),
        };

        let mut metadata = Vec::new();
        for field in &self.config.sidebar_tab_metadata {
            match field {
                SidebarTabMetadata::GitBranch => {
                    if let Some(branch) = &git_branch {
                        metadata.push(branch.clone());
                    }
                }
                SidebarTabMetadata::WorkingDirectory => {
                    if let Some(cwd) = &cwd {
                        metadata.push(path_label(cwd));
                    }
                }
            }
        }
        if self.config.agent_telemetry.enabled || self.config.agent_ui.enabled {
            if let Some(pane) = &pane {
                if let Some(agent) = self.detect_agent_pane(pane) {
                    metadata.extend(self.sidebar_agent_metadata(&agent));
                }
            }
        }
        metadata.retain(|item| item != &title);
        metadata.dedup();

        (title, metadata)
    }

    fn sidebar_agent_for_tab_idx(&self, tab_idx: usize) -> Option<AgentPaneState> {
        if !self.config.agent_ui.enabled || !self.config.agent_ui.show_sidebar_badges {
            return None;
        }
        let pane = self.sidebar_primary_pane_for_tab_idx(tab_idx)?;
        self.detect_agent_pane(&pane)
    }

    fn sidebar_primary_pane_for_tab_idx(&self, tab_idx: usize) -> Option<Arc<dyn Pane>> {
        let tab = Mux::get()
            .get_window(self.mux_window_id)
            .and_then(|window| window.get_by_idx(tab_idx).cloned())?;
        let active_pane = tab.get_active_pane();
        let panes = tab.iter_panes_ignoring_zoom();
        active_pane
            .as_ref()
            .filter(|pane| !is_worktree_pane(pane))
            .cloned()
            .or_else(|| {
                panes
                    .iter()
                    .find(|pos| !is_worktree_pane(&pos.pane))
                    .map(|pos| pos.pane.clone())
            })
            .or(active_pane)
    }

    fn sidebar_compact_tab_icon(&self, tab_idx: usize, title: &str) -> (String, LinearRgba) {
        let pane = self.sidebar_primary_pane_for_tab_idx(tab_idx);
        let agent = pane.as_ref().and_then(|pane| self.detect_agent_pane(pane));
        let command = pane
            .as_ref()
            .and_then(|pane| pane.get_foreground_process_name(CachePolicy::AllowStale))
            .map(|name| basename(&name));
        let pane_title = pane.as_ref().map(|pane| pane.get_title());
        let symbol = compact_tab_symbol(
            title,
            tab_idx,
            agent.as_ref().map(|agent| &agent.kind),
            command.as_deref(),
            pane_title.as_deref(),
        );
        let color = compact_tab_color(
            title,
            tab_idx,
            agent.as_ref().map(|agent| &agent.kind),
            command.as_deref(),
            pane_title.as_deref(),
        );
        (symbol, color)
    }

    fn sidebar_agent_metadata(&self, agent: &AgentPaneState) -> Vec<String> {
        let mut items = Vec::new();
        let fields = if self.config.agent_telemetry.enabled {
            self.config.agent_telemetry.fields.clone()
        } else {
            vec![
                AgentTelemetryField::Kind,
                AgentTelemetryField::Model,
                AgentTelemetryField::Status,
            ]
        };
        for field in &fields {
            match field {
                AgentTelemetryField::Kind => {
                    items.push(agent.kind.label().to_string());
                }
                AgentTelemetryField::Model => {
                    if let Some(value) = &agent.model {
                        items.push(value.clone());
                    }
                }
                AgentTelemetryField::Status => {
                    if agent.status != AgentStatus::Unknown {
                        items.push(agent.status.label().to_string());
                    }
                }
                AgentTelemetryField::InputTokens => {
                    if let Some(value) = agent.input_tokens {
                        items.push(format!("in {value}"));
                    }
                }
                AgentTelemetryField::OutputTokens => {
                    if let Some(value) = agent.output_tokens {
                        items.push(format!("out {value}"));
                    }
                }
                AgentTelemetryField::TotalTokens => {
                    if let (Some(input), Some(output)) = (agent.input_tokens, agent.output_tokens) {
                        items.push(format!("tokens {}", input + output));
                    }
                }
                AgentTelemetryField::EstimatedCost => {
                    if let Some(value) = &agent.cost {
                        items.push(format!("cost {value}"));
                    }
                }
            }
        }
        items
    }

    pub fn paint_agent_toolbelt(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        pos: &PositionedPane,
    ) -> anyhow::Result<()> {
        if !self.config.agent_ui.enabled || !self.config.agent_ui.show_pane_toolbelt {
            return Ok(());
        }
        let Some(agent) = self.detect_agent_pane(&pos.pane) else {
            return Ok(());
        };

        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;
        let cell_w_f = self.render_metrics.cell_size.width as f32;
        let cell_h_f = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar && !self.sidebar_is_active() {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.
        } else {
            tab_bar_height
        };
        let border = self.get_os_border();
        let pane_x = border.left.get() as f32 + padding_left + pos.left as f32 * cell_w_f;
        let pane_y =
            border.top.get() as f32 + top_bar_height + padding_top + pos.top as f32 * cell_h_f;
        let pane_w = pos.pixel_width as f32;
        let pane_h = pos.pixel_height as f32;
        if pane_w < 140. || pane_h < AGENT_TOOLBELT_H + AGENT_TOOLBELT_GAP * 2. {
            return Ok(());
        }

        let mut buttons: Vec<(&str, AgentToolbeltAction)> = Vec::new();
        if agent.actions.interrupt
            && matches!(agent.status, AgentStatus::Running | AgentStatus::Streaming)
        {
            buttons.push(("Stop", AgentToolbeltAction::Interrupt));
        }
        if agent.actions.copy_summary {
            buttons.push(("Copy", AgentToolbeltAction::CopyMenu));
        }
        if agent.actions.attach {
            buttons.push(("Attach", AgentToolbeltAction::Attach));
        }
        if agent.actions.resume {
            buttons.push(("Resume", AgentToolbeltAction::Resume));
        }
        if agent.actions.open_logs {
            buttons.push(("Logs", AgentToolbeltAction::OpenLogs));
        }
        if buttons.is_empty() {
            return Ok(());
        }

        let status = if agent.status == AgentStatus::Unknown {
            String::new()
        } else {
            format!(" {}", agent.status.label())
        };
        let label = match &agent.model {
            Some(model) => format!("{} agent {}{}", agent.kind.label(), model, status),
            None => format!("{} agent{}", agent.kind.label(), status),
        };

        let button_area = buttons.len() as f32 * AGENT_TOOLBELT_BUTTON_W
            + buttons.len().saturating_sub(1) as f32 * AGENT_TOOLBELT_GAP;
        let desired_w = (190. + button_area + PAD_X * 2.).min(AGENT_TOOLBELT_MAX_W);
        let tool_w = desired_w.min((pane_w - AGENT_TOOLBELT_GAP * 2.).max(1.));
        let tool_x = pane_x + pane_w - tool_w - AGENT_TOOLBELT_GAP;
        let tool_y = match self.config.agent_ui.toolbelt_position {
            AgentToolbeltPosition::Top => pane_y + AGENT_TOOLBELT_GAP,
            AgentToolbeltPosition::Bottom => {
                pane_y + pane_h - AGENT_TOOLBELT_H - AGENT_TOOLBELT_GAP
            }
        };

        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_else(TabBarColors::default);
        let active_tab = colors.active_tab();
        let inactive_tab = colors.inactive_tab();
        let fg = active_tab.fg_color.to_linear();
        let bg = opaque(inactive_tab.bg_color.to_linear());
        let hover_bg = lerp_rgba(bg, fg, 0.18);
        let pressed_bg = lerp_rgba(bg, fg, 0.28);
        let accent = active_tab.fg_color.to_linear();

        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(tool_x, tool_y, tool_w, AGENT_TOOLBELT_H),
            RADIUS,
            bg,
        )?;
        let dot_size = 7.;
        self.sidebar_pill_fill(
            layers,
            1,
            euclid::rect(
                tool_x + PAD_X,
                tool_y + (AGENT_TOOLBELT_H - dot_size) * 0.5,
                dot_size,
                dot_size,
            ),
            dot_size * 0.5,
            accent,
        )?;

        let palette = self.palette().clone();
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let render_text = |this: &mut Self,
                           layers: &mut TripleLayerQuadAllocator,
                           text: &str,
                           x: f32,
                           y: f32,
                           pixel_width: f32,
                           fg: LinearRgba,
                           default_bg: LinearRgba,
                           bold: bool|
         -> anyhow::Result<()> {
            let cols = (pixel_width / cell_width as f32).max(1.) as usize;
            let mut attrs = CellAttributes::default();
            attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
            if bold {
                attrs.set_intensity(Intensity::Bold);
            }
            let mut line = Line::from_text(text, &attrs, 1, None);
            line.resize(cols.max(1), SEQ_ZERO);
            this.render_screen_line(
                RenderScreenLineParams {
                    top_pixel_y: y,
                    left_pixel_x: x,
                    pixel_width,
                    stable_line_idx: None,
                    line: &line,
                    selection: 0..0,
                    cursor: &Default::default(),
                    palette: &palette,
                    dims: &RenderableDimensions {
                        cols,
                        physical_top: 0,
                        scrollback_rows: 0,
                        scrollback_top: 0,
                        viewport_rows: 1,
                        dpi: this.terminal_size.dpi,
                        pixel_height: cell_height as usize,
                        pixel_width: pixel_width as usize,
                        reverse_video: false,
                    },
                    config: &this.config,
                    cursor_border_color: LinearRgba::default(),
                    foreground: fg,
                    pane: None,
                    is_active: true,
                    selection_fg: LinearRgba::default(),
                    selection_bg: LinearRgba::default(),
                    cursor_fg: LinearRgba::default(),
                    cursor_bg: LinearRgba::default(),
                    cursor_is_default_color: true,
                    white_space,
                    filled_box,
                    window_is_transparent: true,
                    default_bg,
                    style: None,
                    font: None,
                    use_pixel_positioning: this.config.experimental_pixel_positioning,
                    render_metrics: this.render_metrics,
                    shape_key: None,
                    password_input: false,
                },
                layers,
            )
            .map(|_| ())
        };

        let button_start_x = tool_x + tool_w - PAD_X - button_area;
        let label_x = tool_x + PAD_X + dot_size + AGENT_TOOLBELT_GAP;
        let label_w = (button_start_x - AGENT_TOOLBELT_GAP - label_x).max(cell_width as f32);
        render_text(
            self,
            layers,
            &label,
            label_x,
            tool_y + (AGENT_TOOLBELT_H - cell_h_f) * 0.5,
            label_w,
            fg,
            bg,
            false,
        )?;

        let hovered_item = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let left_pressed = self.current_mouse_buttons.contains(&MousePress::Left);
        let mut button_x = button_start_x;
        for (button_label, action) in buttons {
            let item_type = UIItemType::AgentToolbeltButton {
                pane_id: pos.pane.pane_id(),
                action: action.clone(),
            };
            let hovered = hovered_item.as_ref() == Some(&item_type);
            let pressed =
                hovered && left_pressed && self.pressed_ui_item.as_ref() == Some(&item_type);
            let button_bg = if pressed {
                pressed_bg
            } else if hovered {
                hover_bg
            } else {
                lerp_rgba(bg, fg, 0.08)
            };
            let offset = if pressed { 1. } else { 0. };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(
                    button_x,
                    tool_y + 5. + offset,
                    AGENT_TOOLBELT_BUTTON_W,
                    AGENT_TOOLBELT_H - 10.,
                ),
                5.,
                button_bg,
            )?;
            let button_fg = contrast_label_color(button_bg);
            render_text(
                self,
                layers,
                button_label,
                button_x + 8.,
                tool_y + offset + (AGENT_TOOLBELT_H - cell_h_f) * 0.5,
                AGENT_TOOLBELT_BUTTON_W - 16.,
                button_fg,
                button_bg,
                true,
            )?;
            self.ui_items.push(UIItem {
                x: button_x as usize,
                y: tool_y as usize,
                width: AGENT_TOOLBELT_BUTTON_W as usize,
                height: AGENT_TOOLBELT_H as usize,
                item_type,
            });
            button_x += AGENT_TOOLBELT_BUTTON_W + AGENT_TOOLBELT_GAP;
        }

        Ok(())
    }

    pub fn paint_agent_copy_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.agent_copy_menu.clone() else {
            return Ok(());
        };
        if Mux::get().get_pane(menu.pane_id).is_none() {
            self.agent_copy_menu = None;
            return Ok(());
        }

        let items = [
            ("Copy conversation", AgentCopyAction::Conversation),
            ("Copy last message", AgentCopyAction::LastAgentMessage),
            ("Copy agent details", AgentCopyAction::Summary),
        ];
        let menu_w = AGENT_COPY_MENU_W;
        let menu_h = items.len() as f32 * AGENT_COPY_MENU_ROW_H + 8.;
        let max_x = (self.dimensions.pixel_width as f32 - menu_w - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let max_y = (self.dimensions.pixel_height as f32 - menu_h - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let menu_x =
            (menu.x as f32 - menu_w + AGENT_TOOLBELT_BUTTON_W).clamp(AGENT_TOOLBELT_GAP, max_x);
        let menu_y = (menu.y as f32 + AGENT_TOOLBELT_GAP).clamp(AGENT_TOOLBELT_GAP, max_y);

        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_else(TabBarColors::default);
        let active_tab = colors.active_tab();
        let inactive_tab = colors.inactive_tab();
        let fg = active_tab.fg_color.to_linear();
        let bg = opaque(inactive_tab.bg_color.to_linear());
        let hover_bg = lerp_rgba(bg, fg, 0.18);
        let pressed_bg = lerp_rgba(bg, fg, 0.28);

        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(menu_x, menu_y, menu_w, menu_h),
            RADIUS,
            bg,
        )?;

        let palette = self.palette().clone();
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let cell_width = self.render_metrics.cell_size.width;
        let cell_height = self.render_metrics.cell_size.height;
        let cell_h_f = cell_height as f32;
        let render_text = |this: &mut Self,
                           layers: &mut TripleLayerQuadAllocator,
                           text: &str,
                           x: f32,
                           y: f32,
                           pixel_width: f32,
                           fg: LinearRgba,
                           default_bg: LinearRgba|
         -> anyhow::Result<()> {
            let cols = (pixel_width / cell_width as f32).max(1.) as usize;
            let mut attrs = CellAttributes::default();
            attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
            let mut line = Line::from_text(text, &attrs, 1, None);
            line.resize(cols.max(1), SEQ_ZERO);
            this.render_screen_line(
                RenderScreenLineParams {
                    top_pixel_y: y,
                    left_pixel_x: x,
                    pixel_width,
                    stable_line_idx: None,
                    line: &line,
                    selection: 0..0,
                    cursor: &Default::default(),
                    palette: &palette,
                    dims: &RenderableDimensions {
                        cols,
                        physical_top: 0,
                        scrollback_rows: 0,
                        scrollback_top: 0,
                        viewport_rows: 1,
                        dpi: this.terminal_size.dpi,
                        pixel_height: cell_height as usize,
                        pixel_width: pixel_width as usize,
                        reverse_video: false,
                    },
                    config: &this.config,
                    cursor_border_color: LinearRgba::default(),
                    foreground: fg,
                    pane: None,
                    is_active: true,
                    selection_fg: LinearRgba::default(),
                    selection_bg: LinearRgba::default(),
                    cursor_fg: LinearRgba::default(),
                    cursor_bg: LinearRgba::default(),
                    cursor_is_default_color: true,
                    white_space,
                    filled_box,
                    window_is_transparent: true,
                    default_bg,
                    style: None,
                    font: None,
                    use_pixel_positioning: this.config.experimental_pixel_positioning,
                    render_metrics: this.render_metrics,
                    shape_key: None,
                    password_input: false,
                },
                layers,
            )
            .map(|_| ())
        };

        let hovered_item = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let left_pressed = self.current_mouse_buttons.contains(&MousePress::Left);
        for (idx, (label, action)) in items.iter().enumerate() {
            let item_type = UIItemType::AgentCopyMenuItem {
                pane_id: menu.pane_id,
                action: action.clone(),
            };
            let row_y = menu_y + 4. + idx as f32 * AGENT_COPY_MENU_ROW_H;
            let hovered = hovered_item.as_ref() == Some(&item_type);
            let pressed =
                hovered && left_pressed && self.pressed_ui_item.as_ref() == Some(&item_type);
            let row_bg = if pressed {
                pressed_bg
            } else if hovered {
                hover_bg
            } else {
                bg
            };
            if hovered || pressed {
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(menu_x + 4., row_y, menu_w - 8., AGENT_COPY_MENU_ROW_H),
                    5.,
                    row_bg,
                )?;
            }
            render_text(
                self,
                layers,
                label,
                menu_x + 12.,
                row_y + (AGENT_COPY_MENU_ROW_H - cell_h_f) * 0.5,
                menu_w - 24.,
                contrast_label_color(row_bg),
                row_bg,
            )?;
            self.ui_items.push(UIItem {
                x: (menu_x + 4.) as usize,
                y: row_y as usize,
                width: (menu_w - 8.) as usize,
                height: AGENT_COPY_MENU_ROW_H as usize,
                item_type,
            });
        }

        Ok(())
    }

    pub fn paint_sidebar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        self.settle_sidebar_auto_hide_close();
        let border = self.get_os_border();
        let width = self.sidebar_width();
        if width == 0 {
            return Ok(());
        }

        let left = match self.config.sidebar_position {
            SidebarPosition::Left => border.left.get() as f32,
            SidebarPosition::Right => {
                (self.dimensions.pixel_width as f32 - border.right.get() as f32 - width as f32)
                    .max(0.)
            }
        };
        let top = border.top.get() as f32;
        let height = (self.dimensions.pixel_height as f32
            - border.top.get() as f32
            - border.bottom.get() as f32)
            .max(0.);

        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_else(TabBarColors::default);
        let bg = colors.background().to_linear();
        let active_tab = colors.active_tab();
        let inactive_tab = colors.inactive_tab();
        let active_fg = active_tab.fg_color.to_linear();
        let inactive_fg = inactive_tab.fg_color.to_linear();
        let inactive_bg = inactive_tab.bg_color.to_linear();
        let hover_colors = colors.inactive_tab_hover();
        let hover_fg = hover_colors.fg_color.to_linear();
        let surface = opaque(bg);
        let hover_fill = lerp_rgba(surface, inactive_fg, 0.10);
        let pressed_fill = lerp_rgba(surface, inactive_fg, 0.15);
        let active_fill = lerp_rgba(surface, inactive_fg, 0.18);
        let search_fill = lerp_rgba(surface, inactive_fg, 0.07);
        let focused_search_fill = lerp_rgba(surface, inactive_fg, 0.13);
        let divider = inactive_bg.mul_alpha(0.8);
        let accent = active_fg;
        let hovered_item = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let left_pressed = self.current_mouse_buttons.contains(&MousePress::Left);
        let drop_flash = if let Some((tab_idx, started)) = self.sidebar_drop_flash {
            let elapsed = started.elapsed();
            let duration = Duration::from_millis(180);
            if elapsed < duration {
                *self.has_animation.borrow_mut() = Some(Instant::now() + Duration::from_millis(16));
                Some((tab_idx, 1. - elapsed.as_secs_f32() / duration.as_secs_f32()))
            } else {
                self.sidebar_drop_flash = None;
                None
            }
        } else {
            None
        };

        self.filled_rectangle(
            layers,
            0,
            euclid::rect(left, top, width as f32, height),
            surface,
        )?;
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(left, top, width as f32, height),
            surface,
        )?;

        self.ui_items.push(UIItem {
            x: left as usize,
            y: top as usize,
            width,
            height: height as usize,
            item_type: UIItemType::TabBar(TabBarItem::None),
        });

        let divider_x = match self.config.sidebar_position {
            SidebarPosition::Left => left + width as f32 - 1.,
            SidebarPosition::Right => left,
        };
        self.filled_rectangle(layers, 2, euclid::rect(divider_x, top, 1., height), divider)?;
        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;
        let row_height = self.sidebar_row_height();
        let resize_gap = RESIZE_GRIP_W as f32;
        let item_x = left + INSET;
        let item_w = (width as f32 - INSET * 2.).max(1.);
        let scrollbar_gutter = if self.config.sidebar_scroll_bar {
            SIDEBAR_SCROLLBAR_GUTTER_W
        } else {
            0.
        };
        let content_x = match self.config.sidebar_position {
            SidebarPosition::Left => item_x,
            SidebarPosition::Right => item_x + resize_gap + scrollbar_gutter,
        };
        let content_w = (item_w - resize_gap - scrollbar_gutter).max(1.);
        let sidebar_scrollbar_x = match self.config.sidebar_position {
            SidebarPosition::Left => {
                content_x + content_w + (scrollbar_gutter - SIDEBAR_SCROLLBAR_W).max(0.) * 0.5
            }
            SidebarPosition::Right => {
                item_x + resize_gap + (scrollbar_gutter - SIDEBAR_SCROLLBAR_W).max(0.) * 0.5
            }
        };
        let text_x = content_x + PAD_X + ACTIVE_TEXT_GAP;
        let text_w =
            (content_w - PAD_X * 2. - ACTIVE_TEXT_GAP - CLOSE_ZONE_W).max(cell_width as f32);
        let text_cols = (text_w / cell_width as f32).max(1.) as usize;
        let content_cols =
            ((content_w - PAD_X * 2.).max(cell_width as f32) / cell_width as f32).max(1.) as usize;
        let palette = self.palette().clone();
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        let render_text = |this: &mut Self,
                           layers: &mut TripleLayerQuadAllocator,
                           line: &Line,
                           x: f32,
                           y: f32,
                           cols: usize,
                           pixel_width: f32,
                           fg: LinearRgba,
                           default_bg: LinearRgba|
         -> anyhow::Result<()> {
            let mut line = line.clone();
            line.resize(cols.max(1), SEQ_ZERO);
            this.render_screen_line(
                RenderScreenLineParams {
                    top_pixel_y: y,
                    left_pixel_x: x,
                    pixel_width,
                    stable_line_idx: None,
                    line: &line,
                    selection: 0..0,
                    cursor: &Default::default(),
                    palette: &palette,
                    dims: &RenderableDimensions {
                        cols,
                        physical_top: 0,
                        scrollback_rows: 0,
                        scrollback_top: 0,
                        viewport_rows: 1,
                        dpi: this.terminal_size.dpi,
                        pixel_height: cell_height,
                        pixel_width: pixel_width as usize,
                        reverse_video: false,
                    },
                    config: &this.config,
                    cursor_border_color: LinearRgba::default(),
                    foreground: fg,
                    pane: None,
                    is_active: true,
                    selection_fg: LinearRgba::default(),
                    selection_bg: LinearRgba::default(),
                    cursor_fg: LinearRgba::default(),
                    cursor_bg: LinearRgba::default(),
                    cursor_is_default_color: true,
                    white_space,
                    filled_box,
                    window_is_transparent: false,
                    default_bg,
                    style: None,
                    font: None,
                    use_pixel_positioning: this.config.experimental_pixel_positioning,
                    render_metrics: this.render_metrics,
                    shape_key: None,
                    password_input: false,
                },
                layers,
            )
            .map(|_| ())
        };

        if self.config.sidebar_auto_hide && !self.sidebar_auto_hide_open {
            let tabs: Vec<_> = self
                .tab_bar
                .items()
                .iter()
                .filter_map(|entry| match entry.item {
                    TabBarItem::Tab { tab_idx, active } => {
                        let (title, _) = self.sidebar_tab_labels(tab_idx, &entry.title);
                        Some((tab_idx, active, title))
                    }
                    _ => None,
                })
                .collect();

            let rail_side = (width as f32 - 10.).clamp(38., 46.);
            let rail_x = left + (width as f32 - rail_side) * 0.5;
            let row_stride = rail_side + GAP;
            let list_top = top + INSET;
            let new_tab_y = top + height - INSET - rail_side;
            let list_height = (new_tab_y - GAP - list_top).max(0.);
            let visible_rows = ((list_height + GAP) / row_stride).floor().max(0.) as usize;
            let max_offset = tabs.len().saturating_sub(visible_rows);
            if self.sidebar_scroll_offset > max_offset {
                self.sidebar_scroll_offset = max_offset;
            }

            self.ui_items.push(UIItem {
                x: left as usize,
                y: list_top as usize,
                width,
                height: list_height as usize,
                item_type: UIItemType::SidebarTabList,
            });

            let mut rail_y = list_top;
            for (tab_idx, active, title) in tabs
                .into_iter()
                .skip(self.sidebar_scroll_offset)
                .take(visible_rows)
            {
                let tab_type = UIItemType::SidebarTab { tab_idx, active };
                let tab_hovered = hovered_item.as_ref() == Some(&tab_type);
                let tab_pressed =
                    left_pressed && tab_hovered && self.pressed_ui_item.as_ref() == Some(&tab_type);
                let (symbol, icon_color) = self.sidebar_compact_tab_icon(tab_idx, &title);
                let tab_bg = if active {
                    lerp_rgba(icon_color, surface, 0.16)
                } else if tab_pressed {
                    lerp_rgba(icon_color, surface, 0.30)
                } else if tab_hovered {
                    lerp_rgba(icon_color, surface, 0.42)
                } else {
                    lerp_rgba(icon_color, surface, 0.58)
                };
                let tab_offset = if tab_pressed { 1. } else { 0. };
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(rail_x, rail_y + tab_offset, rail_side, rail_side),
                    RADIUS,
                    tab_bg,
                )?;
                if active {
                    let rail_w = 3.;
                    let active_x = match self.config.sidebar_position {
                        SidebarPosition::Left => rail_x - 4.,
                        SidebarPosition::Right => rail_x + rail_side + 1.,
                    };
                    self.sidebar_pill_fill(
                        layers,
                        2,
                        euclid::rect(
                            active_x,
                            rail_y + tab_offset + rail_side * 0.22,
                            rail_w,
                            rail_side * 0.56,
                        ),
                        rail_w * 0.5,
                        accent,
                    )?;
                }

                let mut symbol = symbol;
                let mut symbol_cols = symbol.chars().count().clamp(1, 2);
                let mut symbol_pixel_width = cell_width as f32 * symbol_cols as f32;
                if symbol_cols > 1 && symbol_pixel_width > rail_side - 2. {
                    symbol = symbol.chars().take(1).collect();
                    symbol_cols = 1;
                    symbol_pixel_width = cell_width as f32;
                }
                let label_fg = contrast_label_color(tab_bg);
                let mut symbol_attrs = CellAttributes::default();
                symbol_attrs
                    .set_foreground(ColorAttribute::TrueColorWithDefaultFallback(
                        label_fg.to_srgb(),
                    ))
                    .set_intensity(Intensity::Bold);
                let symbol_line = Line::from_text(&symbol, &symbol_attrs, symbol_cols, None);
                render_text(
                    self,
                    layers,
                    &symbol_line,
                    rail_x + (rail_side - symbol_pixel_width) * 0.5,
                    rail_y + tab_offset + (rail_side - cell_height as f32) * 0.5,
                    symbol_cols,
                    symbol_pixel_width,
                    label_fg,
                    tab_bg,
                )?;
                self.ui_items.push(UIItem {
                    x: left as usize,
                    y: rail_y as usize,
                    width,
                    height: rail_side as usize,
                    item_type: tab_type,
                });
                rail_y += row_stride;
            }

            let new_tab_type = UIItemType::TabBar(TabBarItem::NewTabButton);
            let new_tab_hovered = hovered_item.as_ref() == Some(&new_tab_type);
            let new_tab_pressed = new_tab_hovered && left_pressed;
            let new_tab_bg = if new_tab_pressed {
                pressed_fill
            } else if new_tab_hovered {
                hover_fill
            } else {
                search_fill
            };
            let new_tab_offset = if new_tab_pressed { 1. } else { 0. };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(rail_x, new_tab_y + new_tab_offset, rail_side, rail_side),
                RADIUS,
                new_tab_bg,
            )?;
            let plus_line = Line::from_text("+", &CellAttributes::default(), 1, None);
            render_text(
                self,
                layers,
                &plus_line,
                rail_x + (rail_side - cell_width as f32) * 0.5,
                new_tab_y + new_tab_offset + (rail_side - cell_height as f32) * 0.5,
                1,
                cell_width as f32,
                if new_tab_hovered {
                    hover_fg
                } else {
                    inactive_fg.mul_alpha(0.86)
                },
                new_tab_bg,
            )?;
            self.ui_items.push(UIItem {
                x: left as usize,
                y: new_tab_y as usize,
                width,
                height: rail_side as usize,
                item_type: new_tab_type,
            });

            self.ui_items.push(UIItem {
                x: match self.config.sidebar_position {
                    SidebarPosition::Left => {
                        left as usize + width.saturating_sub(AUTO_HIDE_RESIZE_GRIP_W)
                    }
                    SidebarPosition::Right => left as usize,
                },
                y: top as usize,
                width: AUTO_HIDE_RESIZE_GRIP_W,
                height: height as usize,
                item_type: UIItemType::SidebarResize { start_width: width },
            });

            return Ok(());
        }

        let mut y = top + INSET;
        if width > 96 {
            let focused = self.sidebar_search.is_some();
            let search_hovered = hovered_item.as_ref() == Some(&UIItemType::SidebarSearch);
            let search_pressed = search_hovered && left_pressed;
            let search_bg = if focused {
                focused_search_fill
            } else if search_pressed {
                pressed_fill
            } else if search_hovered {
                hover_fill
            } else {
                search_fill
            };
            let search_offset = if search_pressed { 1. } else { 0. };
            let search_rect = euclid::rect(item_x, y + search_offset, item_w, row_height as f32);
            if focused {
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(
                        search_rect.min_x() - 1.,
                        search_rect.min_y() - 1.,
                        search_rect.width() + 2.,
                        search_rect.height() + 2.,
                    ),
                    RADIUS + 1.,
                    accent.mul_alpha(0.45),
                )?;
            }
            self.sidebar_rounded_fill(layers, 1, search_rect, RADIUS, search_bg)?;

            let search_text = match &self.sidebar_search {
                Some(state) if state.query.is_empty() => "|".to_string(),
                Some(state) => format!("{}|", state.query),
                None => "Search tabs...".to_string(),
            };
            let search_fg = if focused || search_hovered {
                hover_fg
            } else {
                inactive_fg.mul_alpha(0.62)
            };
            let search_line = Line::from_text(&search_text, &CellAttributes::default(), 1, None);
            render_text(
                self,
                layers,
                &search_line,
                text_x,
                y + search_offset + (row_height as f32 - cell_height as f32) * 0.5,
                content_cols,
                content_w - PAD_X * 2.,
                search_fg,
                search_bg,
            )?;
            self.ui_items.push(UIItem {
                x: item_x as usize,
                y: y as usize,
                width: content_w as usize,
                height: row_height,
                item_type: UIItemType::SidebarSearch,
            });
            y += row_height as f32 + GAP;
        }

        let query = self
            .sidebar_search
            .as_ref()
            .map(|state| state.query.clone());
        let tabs: Vec<_> = self
            .tab_bar
            .items()
            .iter()
            .filter_map(|entry| match entry.item {
                TabBarItem::Tab { tab_idx, active } => {
                    let (title, metadata) = self.sidebar_tab_labels(tab_idx, &entry.title);
                    Some((tab_idx, active, title, metadata))
                }
                _ => None,
            })
            .filter(|(_, _, title, metadata)| match &query {
                Some(query) if !query.is_empty() => {
                    let query = query.to_lowercase();
                    title.to_lowercase().contains(&query)
                        || metadata
                            .iter()
                            .any(|item| item.to_lowercase().contains(&query))
                }
                _ => true,
            })
            .collect();

        let tab_list_top = y;
        let bottom_button_rows = if self.sidebar_width() > 180 { 2. } else { 1. };
        let new_tab_y = top + height - INSET - row_height as f32;
        let tab_list_bottom =
            new_tab_y - GAP - (bottom_button_rows - 1.) * (row_height as f32 + GAP);
        let tab_list_height = (tab_list_bottom - tab_list_top).max(0.);
        let row_stride = row_height as f32 + GAP;
        let visible_rows = ((tab_list_height + GAP) / row_stride).floor().max(0.) as usize;
        let max_offset = tabs.len().saturating_sub(visible_rows);
        if self.sidebar_scroll_offset > max_offset {
            self.sidebar_scroll_offset = max_offset;
        }

        self.ui_items.push(UIItem {
            x: content_x as usize,
            y: tab_list_top as usize,
            width: content_w as usize,
            height: tab_list_height as usize,
            item_type: UIItemType::SidebarTabList,
        });

        let total_tabs = tabs.len();
        for (tab_idx, active, title, metadata) in tabs
            .into_iter()
            .skip(self.sidebar_scroll_offset)
            .take(visible_rows)
        {
            let tab_type = UIItemType::SidebarTab { tab_idx, active };
            let close_type = UIItemType::CloseTab(tab_idx);
            let tab_hovered = hovered_item.as_ref() == Some(&tab_type);
            let close_hovered = hovered_item.as_ref() == Some(&close_type);
            let tab_dragging = matches!(
                self.dragging.as_ref().map(|(item, _)| &item.item_type),
                Some(UIItemType::SidebarTab {
                    tab_idx: drag_idx,
                    ..
                }) if *drag_idx == tab_idx
            );
            let tab_pressed =
                left_pressed && tab_hovered && self.pressed_ui_item.as_ref() == Some(&tab_type);
            let close_pressed =
                left_pressed && close_hovered && self.pressed_ui_item.as_ref() == Some(&close_type);
            let row_bg = if tab_dragging {
                lerp_rgba(surface, active_fg, 0.20)
            } else if active {
                active_fill
            } else if tab_pressed {
                pressed_fill
            } else if tab_hovered || close_hovered {
                hover_fill
            } else {
                surface
            };
            let drop_flash_alpha = drop_flash
                .filter(|(flash_idx, _)| *flash_idx == tab_idx)
                .map(|(_, alpha)| alpha)
                .unwrap_or(0.);
            let row_offset = if tab_dragging {
                -2.
            } else if tab_pressed {
                1.
            } else {
                0.
            };
            if active || tab_hovered || close_hovered || tab_dragging {
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(item_x, y + row_offset, item_w, row_height as f32),
                    RADIUS,
                    row_bg,
                )?;
            }
            if drop_flash_alpha > 0. {
                let flash_x = match self.config.sidebar_position {
                    SidebarPosition::Left => item_x + 2.,
                    SidebarPosition::Right => item_x + item_w - ACTIVE_RAIL_W - 2.,
                };
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(flash_x, y + 4., ACTIVE_RAIL_W, row_height as f32 - 8.),
                    active_fg.mul_alpha(0.18 * drop_flash_alpha),
                )?;
            }
            if active {
                let rail_h = (row_height as f32 * 0.55).max(cell_height as f32 * 0.6);
                let rail_y = y + row_offset + (row_height as f32 - rail_h) * 0.5;
                let rail_x = match self.config.sidebar_position {
                    SidebarPosition::Left => item_x + 2.,
                    SidebarPosition::Right => item_x + item_w - ACTIVE_RAIL_W - 2.,
                };
                self.sidebar_rounded_fill(
                    layers,
                    2,
                    euclid::rect(rail_x, rail_y, ACTIVE_RAIL_W, rail_h),
                    ACTIVE_RAIL_W * 0.5,
                    accent,
                )?;
            }

            let agent = self.sidebar_agent_for_tab_idx(tab_idx);
            let agent_badge_w = if agent.is_some() { 12. } else { 0. };
            if agent.is_some() {
                let badge_size = 7.;
                let badge_x = text_x;
                let badge_y = y + row_offset + (row_height as f32 - badge_size) * 0.5;
                let badge_color = if active {
                    accent
                } else {
                    inactive_fg.mul_alpha(0.58)
                };
                self.sidebar_pill_fill(
                    layers,
                    2,
                    euclid::rect(badge_x, badge_y, badge_size, badge_size),
                    badge_size * 0.5,
                    badge_color,
                )?;
            }

            let display_title = if title.trim_start().starts_with(&format!("{}:", tab_idx + 1)) {
                title
            } else {
                format!("{}: {}", tab_idx + 1, title)
            };
            let primary_line = Line::from_text(&display_title, &CellAttributes::default(), 1, None);
            let metadata_text = metadata.join(" · ");
            let show_metadata = !metadata_text.is_empty()
                && self.sidebar_metadata_rows_enabled()
                && (active || tab_hovered || close_hovered);
            let primary_y = if show_metadata {
                y + row_offset + (row_height as f32 - cell_height as f32 * 2.) * 0.5
            } else {
                y + row_offset + (row_height as f32 - cell_height as f32) * 0.5
            };

            render_text(
                self,
                layers,
                &primary_line,
                text_x + agent_badge_w,
                primary_y,
                text_cols,
                (text_w - agent_badge_w).max(cell_width as f32),
                if active || tab_hovered || close_hovered {
                    inactive_fg
                } else {
                    inactive_fg.mul_alpha(0.78)
                },
                row_bg,
            )?;
            if show_metadata {
                let metadata_line =
                    Line::from_text(&metadata_text, &CellAttributes::default(), 1, None);
                render_text(
                    self,
                    layers,
                    &metadata_line,
                    text_x + agent_badge_w,
                    primary_y + cell_height as f32,
                    text_cols,
                    (text_w - agent_badge_w).max(cell_width as f32),
                    if active || tab_hovered || close_hovered {
                        inactive_fg.mul_alpha(0.60)
                    } else {
                        inactive_fg.mul_alpha(0.42)
                    },
                    row_bg,
                )?;
            }
            self.ui_items.push(UIItem {
                x: content_x as usize,
                y: y as usize,
                width: (content_w - CLOSE_ZONE_W).max(0.) as usize,
                height: row_height,
                item_type: tab_type,
            });

            let close_x = content_x + content_w - CLOSE_ZONE_W;
            let close_bg = if close_pressed {
                lerp_rgba(surface, active_fg, 0.38)
            } else if close_hovered {
                lerp_rgba(surface, active_fg, 0.22)
            } else {
                row_bg
            };
            if close_hovered {
                let close_button_side = (cell_height as f32 + 4.)
                    .min(CLOSE_ZONE_W - 8.)
                    .min(row_height as f32 - 6.)
                    .max(18.);
                let close_button_offset = if close_pressed { 1. } else { 0. };
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(
                        close_x + (CLOSE_ZONE_W - close_button_side) * 0.5,
                        y + row_offset
                            + (row_height as f32 - close_button_side) * 0.5
                            + close_button_offset,
                        close_button_side,
                        close_button_side,
                    ),
                    close_button_side * 0.38,
                    close_bg,
                )?;
            }
            let close_line = Line::from_text("×", &CellAttributes::default(), 1, None);
            let close_glyph_x = close_x + (CLOSE_ZONE_W - cell_width as f32) * 0.5;
            let close_glyph_offset = if close_pressed { 1. } else { 0. };
            render_text(
                self,
                layers,
                &close_line,
                close_glyph_x,
                y + row_offset
                    + (row_height as f32 - cell_height as f32) * 0.5
                    + close_glyph_offset,
                1,
                cell_width as f32,
                if close_hovered {
                    hover_fg
                } else if active {
                    active_fg
                } else {
                    inactive_fg.mul_alpha(0.78)
                },
                LinearRgba::default(),
            )?;
            self.ui_items.push(UIItem {
                x: close_x as usize,
                y: y as usize,
                width: CLOSE_ZONE_W as usize,
                height: row_height,
                item_type: close_type,
            });

            y += row_height as f32 + GAP;
        }

        if self.config.sidebar_scroll_bar && total_tabs > visible_rows && tab_list_height > 0. {
            let (track_y, track_h, _, _) = self.sidebar_scroll_track_bounds().unwrap_or((
                tab_list_top,
                tab_list_height,
                visible_rows,
                total_tabs,
            ));
            let (thumb_y, thumb_h, _) = self
                .sidebar_scroll_thumb_bounds()
                .unwrap_or((track_y, track_h, 0));
            self.sidebar_pill_fill(
                layers,
                2,
                euclid::rect(sidebar_scrollbar_x, thumb_y, SIDEBAR_SCROLLBAR_W, thumb_h),
                SIDEBAR_SCROLLBAR_W * 0.5,
                inactive_fg.mul_alpha(0.42),
            )?;
            self.ui_items.push(UIItem {
                x: (sidebar_scrollbar_x - 8.).max(0.) as usize,
                y: track_y as usize,
                width: (SIDEBAR_SCROLLBAR_W + 16.) as usize,
                height: track_h as usize,
                item_type: UIItemType::SidebarScrollTrack,
            });
            self.ui_items.push(UIItem {
                x: (sidebar_scrollbar_x - 8.).max(0.) as usize,
                y: thumb_y as usize,
                width: (SIDEBAR_SCROLLBAR_W + 16.) as usize,
                height: thumb_h as usize,
                item_type: UIItemType::SidebarScrollThumb,
            });
        }

        let worktree_y = new_tab_y - row_height as f32 - GAP;
        if width > 180 {
            let worktree_type = UIItemType::SidebarWorktreeButton;
            let worktree_hovered = hovered_item.as_ref() == Some(&worktree_type);
            let worktree_pressed = worktree_hovered
                && left_pressed
                && self.pressed_ui_item.as_ref() == Some(&worktree_type);
            let worktree_bg = if worktree_pressed {
                pressed_fill
            } else if worktree_hovered {
                hover_fill
            } else {
                search_fill
            };
            let worktree_offset = if worktree_pressed { 1. } else { 0. };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(
                    item_x,
                    worktree_y + worktree_offset,
                    item_w,
                    row_height as f32,
                ),
                RADIUS,
                worktree_bg,
            )?;
            let folder_color = if worktree_hovered {
                hover_fg.mul_alpha(0.90)
            } else {
                inactive_fg.mul_alpha(0.70)
            };
            let icon_x = content_x + PAD_X;
            let icon_y = worktree_y
                + worktree_offset
                + (row_height as f32 - cell_height as f32) * 0.5
                + (cell_height as f32 - 16.) * 0.5;
            self.sidebar_rounded_fill(
                layers,
                2,
                euclid::rect(icon_x + 2., icon_y, 10., 5.),
                2.,
                folder_color.mul_alpha(0.84),
            )?;
            self.sidebar_rounded_fill(
                layers,
                2,
                euclid::rect(icon_x, icon_y + 4., 18., 12.),
                3.,
                folder_color,
            )?;
            let worktree_line = Line::from_text("Worktree", &CellAttributes::default(), 1, None);
            let worktree_text_x = icon_x + ACTION_ICON_W + ACTION_ICON_GAP;
            render_text(
                self,
                layers,
                &worktree_line,
                worktree_text_x,
                worktree_y + worktree_offset + (row_height as f32 - cell_height as f32) * 0.5,
                content_cols,
                content_w - PAD_X * 2. - ACTION_ICON_W - ACTION_ICON_GAP,
                if worktree_hovered {
                    hover_fg
                } else {
                    inactive_fg.mul_alpha(0.86)
                },
                worktree_bg,
            )?;
            self.ui_items.push(UIItem {
                x: content_x as usize,
                y: worktree_y as usize,
                width: content_w as usize,
                height: row_height,
                item_type: worktree_type,
            });
        }

        let new_tab_type = UIItemType::TabBar(TabBarItem::NewTabButton);
        let new_tab_hovered = hovered_item.as_ref() == Some(&new_tab_type);
        let new_tab_pressed = new_tab_hovered && left_pressed;
        let new_tab_bg = if new_tab_pressed {
            pressed_fill
        } else if new_tab_hovered {
            hover_fill
        } else {
            search_fill
        };
        let new_tab_offset = if new_tab_pressed { 1. } else { 0. };
        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(
                item_x,
                new_tab_y + new_tab_offset,
                item_w,
                row_height as f32,
            ),
            RADIUS,
            new_tab_bg,
        )?;
        let new_tab_line = Line::from_text("+ New Tab", &CellAttributes::default(), 1, None);
        render_text(
            self,
            layers,
            &new_tab_line,
            text_x,
            new_tab_y + new_tab_offset + (row_height as f32 - cell_height as f32) * 0.5,
            content_cols,
            content_w - PAD_X * 2. - ACTIVE_TEXT_GAP,
            if new_tab_hovered {
                hover_fg
            } else {
                inactive_fg
            },
            new_tab_bg,
        )?;
        self.ui_items.push(UIItem {
            x: content_x as usize,
            y: new_tab_y as usize,
            width: content_w as usize,
            height: row_height,
            item_type: new_tab_type,
        });

        self.ui_items.push(UIItem {
            x: match self.config.sidebar_position {
                SidebarPosition::Left => left as usize + width.saturating_sub(RESIZE_GRIP_W),
                SidebarPosition::Right => left as usize,
            },
            y: top as usize,
            width: RESIZE_GRIP_W,
            height: height as usize,
            item_type: UIItemType::SidebarResize { start_width: width },
        });

        Ok(())
    }

    pub(crate) fn sidebar_rounded_fill(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        rect: RectF,
        radius: f32,
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        let r = radius
            .min(rect.size.width * 0.5)
            .min(rect.size.height * 0.5)
            .max(0.0);
        if r <= 0.5 {
            self.filled_rectangle(layers, layer_num, rect, color)?;
            return Ok(());
        }

        let x = rect.min_x();
        let y = rect.min_y();
        let w = rect.size.width;
        let h = rect.size.height;

        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x, y + r, w, h - 2.0 * r),
            color,
        )?;
        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x + r, y, w - 2.0 * r, r),
            color,
        )?;
        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x + r, y + h - r, w - 2.0 * r, r),
            color,
        )?;

        let underline_height = 1;
        self.poly_quad(
            layers,
            layer_num,
            euclid::point2(x, y),
            TOP_LEFT_ROUNDED_CORNER,
            underline_height,
            euclid::size2(r, r),
            color,
        )?;
        self.poly_quad(
            layers,
            layer_num,
            euclid::point2(x + w - r, y),
            TOP_RIGHT_ROUNDED_CORNER,
            underline_height,
            euclid::size2(r, r),
            color,
        )?;
        self.poly_quad(
            layers,
            layer_num,
            euclid::point2(x, y + h - r),
            BOTTOM_LEFT_ROUNDED_CORNER,
            underline_height,
            euclid::size2(r, r),
            color,
        )?;
        self.poly_quad(
            layers,
            layer_num,
            euclid::point2(x + w - r, y + h - r),
            BOTTOM_RIGHT_ROUNDED_CORNER,
            underline_height,
            euclid::size2(r, r),
            color,
        )?;
        Ok(())
    }

    pub(crate) fn sidebar_pill_fill(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        layer_num: usize,
        rect: RectF,
        radius: f32,
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        let r = radius
            .min(rect.size.width * 0.5)
            .min(rect.size.height * 0.5)
            .max(0.0);
        if r <= 0.5 {
            self.filled_rectangle(layers, layer_num, rect, color)?;
            return Ok(());
        }

        let x = rect.min_x();
        let y = rect.min_y();
        let w = rect.size.width;
        let h = rect.size.height;
        if h > 2.0 * r {
            self.filled_rectangle(
                layers,
                layer_num,
                euclid::rect(x, y + r, w, h - 2.0 * r),
                color,
            )?;
        }

        let cap_rows = r.ceil() as usize;
        for row in 0..cap_rows {
            let row_f = row as f32 + 0.5;
            let dy = (r - row_f).max(0.0);
            let inset = (r - (r * r - dy * dy).max(0.0).sqrt()).ceil();
            let strip_w = (w - inset * 2.0).max(1.0);
            self.filled_rectangle(
                layers,
                layer_num,
                euclid::rect(x + inset, y + row as f32, strip_w, 1.0),
                color,
            )?;
            self.filled_rectangle(
                layers,
                layer_num,
                euclid::rect(x + inset, y + h - row as f32 - 1.0, strip_w, 1.0),
                color,
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_metadata_evidence_requires_agent_fields() {
        let mut vars = HashMap::new();
        assert!(!has_agent_metadata_evidence(&vars));

        vars.insert("SHELL".to_string(), "zsh".to_string());
        assert!(!has_agent_metadata_evidence(&vars));

        vars.insert("agent.model".to_string(), "gpt-5".to_string());
        assert!(has_agent_metadata_evidence(&vars));
    }
}
