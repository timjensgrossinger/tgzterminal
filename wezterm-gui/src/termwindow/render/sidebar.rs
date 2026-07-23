use crate::quad::TripleLayerQuadAllocator;
use crate::spawn::SpawnWhere;
use crate::tabbar::TabBarItem;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::render::RenderScreenLineParams;
use crate::termwindow::{AgentCopyAction, AgentToolbeltAction, UIItem, UIItemType};
use config::keyassignment::SpawnCommand;
use config::{
    default_agent_adapters, AgentAdapterConfig, AgentTelemetryField, AgentToolbeltPosition,
    SidebarPosition, SidebarTabDensity, SidebarTabMetadata, SidebarTabTitleSource, TabBarColors,
};
use mux::pane::{CachePolicy, Pane};
use mux::renderable::RenderableDimensions;
use mux::tab::PositionedPane;
use mux::Mux;
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::surface::{Line, SEQ_ZERO};
use url::Url;
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
const AGENT_TOOLBELT_MIN_BUTTON_W: f32 = 88.;
const AGENT_TOOLBELT_BUTTON_PAD_X: f32 = 24.;
const AGENT_TOOLBELT_DOT_SIZE: f32 = 7.;
const AGENT_TOOLBELT_MAX_W: f32 = 760.;
const AGENT_TOOLBELT_MIN_W: f32 = 360.;
const AGENT_TOOLBELT_RIGHT_INSET: f32 = 44.;
const AGENT_COPY_MENU_W: f32 = 360.;
const AGENT_COPY_MENU_ROW_H: f32 = 28.;
const MAX_AGENT_PATTERN_LEN: usize = 256;
const AGENT_PATTERN_REGEX_CACHE_LIMIT: usize = 128;
const WAITING_NOTIFICATION_THROTTLE: Duration = Duration::from_secs(60);
const DEFAULT_AGENT_ADAPTER_IDS: [&str; 7] = [
    "claude", "codex", "gemini", "opencode", "copilot", "cursor", "amp",
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct SidebarScrollGeometry {
    track_y: f32,
    track_h: f32,
    thumb_y: f32,
    thumb_h: f32,
    max_offset: usize,
}

impl SidebarScrollGeometry {
    fn new(
        track_y: f32,
        track_h: f32,
        visible: usize,
        total: usize,
        offset: usize,
        min_thumb_h: f32,
    ) -> Option<Self> {
        if total <= visible || visible == 0 || track_h <= 0. {
            return None;
        }

        let max_offset = total.saturating_sub(visible);
        let thumb_h = (track_h * visible as f32 / total as f32)
            .max(min_thumb_h)
            .min(track_h);
        let scroll_range = (track_h - thumb_h).max(0.);
        let thumb_y = if max_offset == 0 {
            track_y
        } else {
            track_y + scroll_range * offset.min(max_offset) as f32 / max_offset as f32
        };

        Some(Self {
            track_y,
            track_h,
            thumb_y,
            thumb_h,
            max_offset,
        })
    }

    fn offset_for_thumb_top(&self, thumb_top: f32) -> Option<usize> {
        if self.max_offset == 0 {
            return None;
        }

        let scroll_range = (self.track_h - self.thumb_h).max(0.);
        if scroll_range <= 0. {
            return None;
        }

        let thumb_top = thumb_top.clamp(self.track_y, self.track_y + scroll_range);
        Some(
            (((thumb_top - self.track_y) / scroll_range) * self.max_offset as f32).round() as usize,
        )
    }
}

lazy_static::lazy_static! {
    static ref BUILT_IN_AGENT_ADAPTERS: config::AgentAdaptersConfig = default_agent_adapters();
    static ref AGENT_PATTERN_REGEX_CACHE: Mutex<HashMap<String, Option<regex::Regex>>> =
        Mutex::new(HashMap::new());
}

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
    split_agent_user_prompt_line(line).is_some()
}

fn clean_agent_content_line(line: &str) -> String {
    let trimmed = line.trim_end();
    trimmed
        .strip_prefix("⏺ ")
        .or_else(|| trimmed.strip_prefix("● "))
        .map(str::trim_start)
        .unwrap_or(trimmed)
        .to_string()
}

fn split_agent_user_prompt_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for prefix in ["> ", "› ", "❯ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest.trim_start());
        }
    }
    if matches!(trimmed, ">" | "›" | "❯") {
        Some("")
    } else {
        None
    }
}

fn is_agent_separator_line(trimmed: &str) -> bool {
    let mut count = 0;
    for ch in trimmed.chars().filter(|ch| !ch.is_whitespace()) {
        if !matches!(
            ch,
            '-' | '=' | '_' | '─' | '━' | '═' | '┄' | '┈' | '•' | '·'
        ) {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn starts_with_time_usage(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    let first = chars.next();
    let second = chars.next();
    matches!((first, second), (Some(a), Some(':')) if a.is_ascii_digit())
        || matches!(
            (first, second, chars.next(), chars.next()),
            (Some(a), Some(b), Some(':'), Some(_)) if a.is_ascii_digit() && b.is_ascii_digit()
        )
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    !needle.trim().is_empty()
        && haystack
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
}

fn built_in_agent_adapters() -> &'static config::AgentAdaptersConfig {
    &BUILT_IN_AGENT_ADAPTERS
}

fn cached_regex_matches(haystack: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.len() > MAX_AGENT_PATTERN_LEN {
        return false;
    }
    let mut cache = AGENT_PATTERN_REGEX_CACHE.lock().unwrap();
    if !cache.contains_key(pattern) {
        if cache.len() >= AGENT_PATTERN_REGEX_CACHE_LIMIT {
            cache.clear();
        }
        let compiled = match RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(regex) => Some(regex),
            Err(err) => {
                log::warn!(
                    "ignoring invalid agent_ui regex pattern {:?}: {}",
                    pattern,
                    err
                );
                None
            }
        };
        cache.insert(pattern.to_string(), compiled);
    }
    cache
        .get(pattern)
        .and_then(|regex| regex.as_ref())
        .map(|regex| regex.is_match(haystack))
        .unwrap_or(false)
}

fn agent_pattern_matches(haystack: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.len() > MAX_AGENT_PATTERN_LEN + 3 {
        return false;
    }
    if let Some(regex) = pattern.strip_prefix("re:") {
        cached_regex_matches(haystack, regex)
    } else {
        contains_case_insensitive(haystack, pattern)
    }
}

/// Like `agent_pattern_matches` but assumes `haystack_lower` is already ASCII-lowercased,
/// avoiding a redundant allocation inside `contains_case_insensitive`.
fn agent_pattern_matches_pre_lowered(haystack_lower: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.len() > MAX_AGENT_PATTERN_LEN + 3 {
        return false;
    }
    if let Some(regex) = pattern.strip_prefix("re:") {
        // regex is built with case_insensitive(true), safe to pass pre-lowered text
        cached_regex_matches(haystack_lower, regex)
    } else {
        // haystack is already lowercase; only lowercase the needle
        !pattern.trim().is_empty() && haystack_lower.contains(&pattern.to_ascii_lowercase())
    }
}

fn adapter_patterns<'a>(
    adapter: Option<&'a AgentAdapterConfig>,
    field: PatternField,
) -> Vec<String> {
    match adapter {
        Some(adapter) => match field {
            PatternField::Visible => adapter.visible_patterns.clone(),
            PatternField::Strip => adapter.strip_patterns.clone(),
            PatternField::Model => adapter.model_patterns.clone(),
        },
        None => built_in_agent_adapters()
            .values()
            .flat_map(|adapter| match field {
                PatternField::Visible => adapter.visible_patterns.clone(),
                PatternField::Strip => adapter.strip_patterns.clone(),
                PatternField::Model => adapter.model_patterns.clone(),
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum PatternField {
    Visible,
    Strip,
    Model,
}

fn is_agent_transcript_chrome_line(line: &str, adapter: Option<&AgentAdapterConfig>) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_agent_separator_line(trimmed) {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("⏵⏵") || lower.starts_with("▶▶") || lower.starts_with("▸▸") {
        return true;
    }
    if (starts_with_time_usage(trimmed)
        && (lower.contains("tokens") || lower.contains("reset") || lower.contains("ctx")))
        || (lower.contains("tokens") && (trimmed.contains('│') || trimmed.contains('┃')))
        || (lower.contains("% used") && lower.contains("% left"))
    {
        return true;
    }
    if matches!(trimmed.chars().next(), Some('✻' | '✽' | '✶' | '✢' | '*')) {
        return true;
    }

    adapter_patterns(adapter, PatternField::Strip)
        .iter()
        .any(|pattern| agent_pattern_matches(trimmed, pattern))
}

fn trim_blank_edges(lines: &mut Vec<String>) {
    while lines
        .first()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.remove(0);
    }
    while lines
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }
}

fn push_agent_transcript_line(lines: &mut Vec<String>, text: String) {
    if text.trim().is_empty() {
        if !lines.is_empty()
            && !lines
                .last()
                .map(|line| line.trim().is_empty())
                .unwrap_or(false)
        {
            lines.push(String::new());
        }
    } else {
        lines.push(text.trim_end().to_string());
    }
}

fn clean_agent_conversation_transcript(raw: &str, adapter: Option<&AgentAdapterConfig>) -> String {
    let mut lines = Vec::new();
    let skip_until_first_prompt = raw.lines().any(is_agent_user_prompt_line);
    let mut seen_prompt = !skip_until_first_prompt;
    for line in raw.lines() {
        if !seen_prompt {
            if is_agent_user_prompt_line(line) {
                seen_prompt = true;
            } else {
                continue;
            }
        }

        if line.trim().is_empty() {
            push_agent_transcript_line(&mut lines, String::new());
            continue;
        }
        if is_agent_transcript_chrome_line(line, adapter) {
            continue;
        }
        if is_agent_user_prompt_line(line) {
            let prompt = split_agent_user_prompt_line(line).unwrap_or("");
            push_agent_transcript_line(&mut lines, prompt.to_string());
        } else {
            push_agent_transcript_line(&mut lines, clean_agent_content_line(line));
        }
    }
    trim_blank_edges(&mut lines);
    lines.join("\n")
}

fn clean_agent_last_message_transcript(raw: &str, adapter: Option<&AgentAdapterConfig>) -> String {
    let mut current = Vec::new();
    let mut last_message = Vec::new();

    for line in raw.lines() {
        if line.trim().is_empty() {
            push_agent_transcript_line(&mut current, String::new());
            continue;
        }
        if is_agent_transcript_chrome_line(line, adapter) {
            continue;
        }
        if is_agent_user_prompt_line(line) {
            trim_blank_edges(&mut current);
            if !current.is_empty() {
                last_message = current;
            }
            current = Vec::new();
            continue;
        }
        push_agent_transcript_line(&mut current, clean_agent_content_line(line));
    }

    trim_blank_edges(&mut current);
    if !current.is_empty() {
        last_message = current;
    }
    trim_blank_edges(&mut last_message);
    last_message.join("\n")
}

fn agent_copy_payload_from_text(
    action: &AgentCopyAction,
    raw_transcript: &str,
    summary: &str,
    adapter: Option<&AgentAdapterConfig>,
) -> String {
    match action {
        AgentCopyAction::Conversation => {
            clean_agent_conversation_transcript(raw_transcript, adapter)
        }
        AgentCopyAction::Markdown => clean_agent_markdown_transcript(raw_transcript, adapter),
        AgentCopyAction::LastAgentMessage => {
            clean_agent_last_message_transcript(raw_transcript, adapter)
        }
        AgentCopyAction::Summary => summary.to_string(),
    }
}

fn clean_agent_markdown_transcript(raw: &str, adapter: Option<&AgentAdapterConfig>) -> String {
    let conversation = clean_agent_conversation_transcript(raw, adapter);
    if conversation.trim().is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    let mut current_heading: Option<&'static str> = None;
    let mut current = Vec::new();
    for line in conversation.lines() {
        let heading = if raw.lines().any(|raw_line| {
            split_agent_user_prompt_line(raw_line)
                .map(|prompt| prompt == line)
                .unwrap_or(false)
        }) {
            "User"
        } else {
            "Agent"
        };
        if current_heading != Some(heading) {
            if let Some(existing) = current_heading {
                trim_blank_edges(&mut current);
                if !current.is_empty() {
                    sections.push(format!("## {existing}\n\n{}", current.join("\n")));
                }
            }
            current_heading = Some(heading);
            current = Vec::new();
        }
        push_agent_transcript_line(&mut current, line.to_string());
    }
    if let Some(existing) = current_heading {
        trim_blank_edges(&mut current);
        if !current.is_empty() {
            sections.push(format!("## {existing}\n\n{}", current.join("\n")));
        }
    }
    sections.join("\n\n")
}

fn agent_transcript_start(scrollback_top: isize, end: isize, max_rows: isize) -> isize {
    scrollback_top.max(end.saturating_sub(max_rows.max(1)))
}

fn agent_toolbelt_button_width(label: &str, cell_width: usize, dpi_scale: f32) -> f32 {
    let text_w = label.chars().count() as f32 * cell_width as f32;
    (text_w + AGENT_TOOLBELT_BUTTON_PAD_X * dpi_scale * 2.)
        .max(AGENT_TOOLBELT_MIN_BUTTON_W * dpi_scale)
}

fn agent_toolbelt_button_area(buttons: &[(&str, AgentToolbeltAction, f32)]) -> f32 {
    let widths = buttons.iter().map(|(_, _, width)| *width).sum::<f32>();
    widths + buttons.len().saturating_sub(1) as f32 * AGENT_TOOLBELT_GAP
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
    agent_adapter: Option<&AgentAdapterConfig>,
    command: Option<&str>,
    pane_title: Option<&str>,
) -> String {
    if let Some(kind) = agent_kind {
        return adapter_short_label(agent_adapter, kind);
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
    agent_adapter: Option<&AgentAdapterConfig>,
    command: Option<&str>,
    pane_title: Option<&str>,
) -> LinearRgba {
    if let Some(kind) = agent_kind {
        return adapter_color(agent_adapter, kind);
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

fn encode_claude_project_path(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '-',
            _ => ch,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaudeLogsPathError {
    Missing,
    OutsideProjects,
    NotDirectory,
}

fn resolve_claude_logs_path_under(home: &Path, cwd: &Path) -> Result<PathBuf, ClaudeLogsPathError> {
    let projects_root = home.join(".claude").join("projects");
    let project_dir = projects_root.join(encode_claude_project_path(cwd));
    let root = projects_root
        .canonicalize()
        .map_err(|_| ClaudeLogsPathError::Missing)?;
    let path = project_dir
        .canonicalize()
        .map_err(|_| ClaudeLogsPathError::Missing)?;
    if !path.starts_with(&root) {
        return Err(ClaudeLogsPathError::OutsideProjects);
    }
    if !path.is_dir() {
        return Err(ClaudeLogsPathError::NotDirectory);
    }
    Ok(path)
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

fn truthy_agent_var(vars: &HashMap<String, String>, key: &str) -> bool {
    user_var(vars, key)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn agent_control_actions_allowed(
    config_enabled: bool,
    trusted_controls: bool,
    vars: &HashMap<String, String>,
) -> bool {
    trusted_controls || config_enabled || truthy_agent_var(vars, "agent.enable_control_actions")
}

#[derive(Clone, Debug, Default)]
struct AgentActionTemplateValues {
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    attach_url: Option<String>,
}

impl AgentActionTemplateValues {
    fn from_vars(vars: &HashMap<String, String>, cwd: Option<&Path>) -> Self {
        Self {
            session_id: user_var(vars, "agent.session_id")
                .or_else(|| user_var(vars, "agent.session"))
                .map(ToString::to_string),
            cwd: cwd.map(Path::to_path_buf),
            home: dirs_next::home_dir(),
            attach_url: user_var(vars, "agent.attach_url")
                .or_else(|| user_var(vars, "agent.attach"))
                .map(ToString::to_string),
        }
    }

    fn from_agent(agent: &AgentPaneState) -> Self {
        Self {
            session_id: agent.session_id.clone(),
            cwd: agent.cwd.clone(),
            home: dirs_next::home_dir(),
            attach_url: agent.attach_url.clone(),
        }
    }

    fn value(&self, name: &str) -> Option<String> {
        match name {
            "session_id" => self.session_id.clone(),
            "cwd" => self
                .cwd
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            "home" => self
                .home
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            "attach_url" => self.attach_url.clone(),
            "claude_project_path" => self
                .cwd
                .as_ref()
                .map(|path| encode_claude_project_path(path)),
            _ => None,
        }
    }
}

fn expand_agent_action_template(
    template: &str,
    values: &AgentActionTemplateValues,
) -> Option<String> {
    let mut output = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let end = after_start.find('}')?;
        let name = &after_start[..end];
        output.push_str(&values.value(name)?);
        rest = &after_start[end + 1..];
    }
    if rest.contains('}') {
        return None;
    }
    output.push_str(rest);
    Some(output)
}

#[cfg(unix)]
fn path_is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn path_is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn command_exists_on_path(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return path_is_executable(command_path);
    }
    env::var_os("PATH")
        .map(|path| {
            env::split_paths(&path).any(|dir| {
                let candidate = dir.join(command);
                path_is_executable(&candidate)
            })
        })
        .unwrap_or(false)
}

fn resolve_agent_command(
    command: Option<&Vec<String>>,
    values: &AgentActionTemplateValues,
) -> Option<Vec<String>> {
    let command = command?;
    if command.is_empty() {
        return None;
    }
    let argv = command
        .iter()
        .map(|arg| expand_agent_action_template(arg, values))
        .collect::<Option<Vec<_>>>()?;
    if argv
        .first()
        .is_none_or(|command| !command_exists_on_path(command))
    {
        return None;
    }
    Some(argv)
}

fn resolve_agent_resume_command(
    adapter: &AgentAdapterConfig,
    values: &AgentActionTemplateValues,
) -> Option<Vec<String>> {
    if values.session_id.is_some() {
        resolve_agent_command(adapter.resume_command.as_ref(), values)
    } else {
        resolve_agent_command(adapter.resume_latest_command.as_ref(), values)
    }
}

fn resolve_agent_attach_command(
    adapter: &AgentAdapterConfig,
    values: &AgentActionTemplateValues,
) -> Option<Vec<String>> {
    resolve_agent_command(adapter.attach_command.as_ref(), values)
}

fn expand_agent_detail_path(template: &str, values: &AgentActionTemplateValues) -> Option<PathBuf> {
    let expanded = expand_agent_action_template(template, values)?;
    if let Some(stripped) = expanded.strip_prefix("~/") {
        return values.home.as_ref().map(|home| home.join(stripped));
    }
    Some(PathBuf::from(expanded))
}

fn resolve_agent_detail_path(
    adapter_id: Option<&str>,
    adapter: &AgentAdapterConfig,
    values: &AgentActionTemplateValues,
) -> Option<PathBuf> {
    for template in adapter.detail_paths.as_deref().unwrap_or_default() {
        if adapter_id == Some("claude")
            && template == "{home}/.claude/projects/{claude_project_path}"
        {
            let (Some(home), Some(cwd)) = (&values.home, &values.cwd) else {
                continue;
            };
            if let Ok(path) = resolve_claude_logs_path_under(home, cwd) {
                return Some(path);
            }
            continue;
        }
        let Some(path) = expand_agent_detail_path(template, values) else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        let path = path.canonicalize().ok().unwrap_or(path);
        if adapter_id == Some("claude") {
            let Some(home) = &values.home else {
                continue;
            };
            let projects_root = home.join(".claude").join("projects");
            let Ok(root) = projects_root.canonicalize() else {
                continue;
            };
            if !path.starts_with(root) || !path.is_dir() {
                continue;
            }
        }
        return Some(path);
    }
    None
}

fn agent_detail_button_label(
    adapter_id: Option<&str>,
    adapter: Option<&AgentAdapterConfig>,
) -> &'static str {
    if adapter_id == Some("claude") {
        return "Logs";
    }
    let only_logs = adapter
        .and_then(|adapter| adapter.detail_paths.as_deref())
        .map(|paths| {
            !paths.is_empty()
                && paths
                    .iter()
                    .all(|path| path.to_ascii_lowercase().contains("log"))
        })
        .unwrap_or(false);
    if only_logs {
        "Logs"
    } else {
        "Details"
    }
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
    fn from_adapter_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::OpenCode),
            "copilot" => Some(Self::Copilot),
            "cursor" => Some(Self::Cursor),
            "amp" => Some(Self::Amp),
            _ => None,
        }
    }

    fn from_hint(hint: &str) -> Option<Self> {
        let lower = basename(hint).to_ascii_lowercase();
        built_in_agent_adapters().iter().find_map(|(id, adapter)| {
            adapter
                .process_names
                .iter()
                .any(|process| basename(process).eq_ignore_ascii_case(&lower))
                .then(|| {
                    Self::from_adapter_id(id)
                        .unwrap_or_else(|| Self::Unknown(adapter_label(adapter, id)))
                })
        })
    }

    /// Like [`from_hint`] but also checks `merged_adapters` so user-configured
    /// adapters are recognized in the explicit `agent.kind` user-var path.
    fn from_hint_with_adapters(
        hint: &str,
        merged_adapters: &[(String, AgentAdapterConfig)],
    ) -> Option<Self> {
        let lower = basename(hint).to_ascii_lowercase();
        merged_adapters.iter().find_map(|(id, adapter)| {
            adapter
                .process_names
                .iter()
                .any(|process| basename(process).eq_ignore_ascii_case(&lower))
                .then(|| {
                    Self::from_adapter_id(id)
                        .unwrap_or_else(|| Self::Unknown(adapter_label(adapter, id)))
                })
        })
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

fn adapter_label(adapter: &AgentAdapterConfig, id: &str) -> String {
    adapter
        .label
        .as_deref()
        .filter(|label| !label.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| title_case_label(id).unwrap_or_else(|| id.to_string()))
}

fn adapter_short_label(adapter: Option<&AgentAdapterConfig>, kind: &AgentKind) -> String {
    adapter
        .and_then(|adapter| adapter.short_label.as_deref())
        .filter(|label| !label.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| match kind {
            AgentKind::Claude => "Cl".to_string(),
            AgentKind::Codex => "Cx".to_string(),
            AgentKind::Gemini => "G".to_string(),
            AgentKind::OpenCode => "Oc".to_string(),
            AgentKind::Copilot => "Cp".to_string(),
            AgentKind::Cursor => "Cu".to_string(),
            AgentKind::Amp => "A".to_string(),
            AgentKind::Unknown(value) => compact_label(value, "Ag"),
        })
}

fn parse_adapter_color(value: &str) -> Option<LinearRgba> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if hex.len() != 6 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.;
    Some(LinearRgba(r, g, b, 1.0))
}

fn adapter_color(adapter: Option<&AgentAdapterConfig>, kind: &AgentKind) -> LinearRgba {
    if let Some(color) = adapter
        .and_then(|adapter| adapter.color.as_deref())
        .and_then(parse_adapter_color)
    {
        return color;
    }
    match kind {
        AgentKind::Claude => LinearRgba(0.86, 0.48, 0.32, 1.0),
        AgentKind::Codex => LinearRgba(0.24, 0.64, 0.48, 1.0),
        AgentKind::Gemini => LinearRgba(0.28, 0.52, 0.92, 1.0),
        AgentKind::OpenCode => LinearRgba(0.22, 0.66, 0.70, 1.0),
        AgentKind::Copilot => LinearRgba(0.34, 0.66, 0.38, 1.0),
        AgentKind::Cursor => LinearRgba(0.44, 0.42, 0.82, 1.0),
        AgentKind::Amp => LinearRgba(0.74, 0.36, 0.68, 1.0),
        AgentKind::Unknown(_) => LinearRgba(0.58, 0.50, 0.82, 1.0),
    }
}

fn merge_agent_adapter_config(
    base: &AgentAdapterConfig,
    configured: &AgentAdapterConfig,
) -> AgentAdapterConfig {
    AgentAdapterConfig {
        enabled: configured.enabled,
        label: configured.label.clone().or_else(|| base.label.clone()),
        short_label: configured
            .short_label
            .clone()
            .or_else(|| base.short_label.clone()),
        color: configured.color.clone().or_else(|| base.color.clone()),
        process_names: if configured.process_names.is_empty() {
            base.process_names.clone()
        } else {
            configured.process_names.clone()
        },
        title_patterns: if configured.title_patterns.is_empty() {
            base.title_patterns.clone()
        } else {
            configured.title_patterns.clone()
        },
        visible_patterns: if configured.visible_patterns.is_empty() {
            base.visible_patterns.clone()
        } else {
            configured.visible_patterns.clone()
        },
        strip_patterns: if configured.strip_patterns.is_empty() {
            base.strip_patterns.clone()
        } else {
            configured.strip_patterns.clone()
        },
        model_patterns: if configured.model_patterns.is_empty() {
            base.model_patterns.clone()
        } else {
            configured.model_patterns.clone()
        },
        resume_command: configured
            .resume_command
            .clone()
            .or_else(|| base.resume_command.clone()),
        resume_latest_command: configured
            .resume_latest_command
            .clone()
            .or_else(|| base.resume_latest_command.clone()),
        attach_command: configured
            .attach_command
            .clone()
            .or_else(|| base.attach_command.clone()),
        detail_paths: configured
            .detail_paths
            .clone()
            .or_else(|| base.detail_paths.clone()),
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
pub(crate) struct AgentPaneState {
    adapter_id: Option<String>,
    kind: AgentKind,
    trusted_controls: bool,
    status: AgentStatus,
    model: Option<String>,
    session_id: Option<String>,
    attach_url: Option<String>,
    cwd: Option<PathBuf>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cost: Option<String>,
    actions: AgentActions,
}

fn agent_toolbelt_buttons(
    agent_ui: &config::AgentUiConfig,
    agent: &AgentPaneState,
    adapter: Option<&AgentAdapterConfig>,
    rich_input_enabled: bool,
    rich_input_docked: bool,
) -> Vec<(&'static str, AgentToolbeltAction)> {
    if !agent_ui.enabled
        || !agent_ui.show_pane_toolbelt
        || adapter.map(|adapter| !adapter.enabled).unwrap_or(false)
    {
        return Vec::new();
    }

    let mut buttons = Vec::new();
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
        buttons.push((
            agent_detail_button_label(agent.adapter_id.as_deref(), adapter),
            AgentToolbeltAction::OpenLogs,
        ));
    }
    if rich_input_enabled {
        if rich_input_docked {
            // Button that activates the persistent docked input strip for this
            // agent pane (the strip is not shown until toggled here).
            buttons.push(("Input", AgentToolbeltAction::DockInput));
        } else {
            buttons.push(("Compose", AgentToolbeltAction::Compose));
        }
    }
    buttons
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentDetectionCacheKey {
    foreground_process: Option<String>,
    pane_title: String,
    relevant_user_vars: Vec<(String, String)>,
    viewport_top: isize,
    viewport_rows: usize,
    visible_fingerprint: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct AgentDetectionCacheEntry {
    key: AgentDetectionCacheKey,
    state: Option<AgentPaneState>,
    last_wait_notification: Option<Instant>,
    detected_at: Instant,
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

fn adapter_kind_from_id(id: &str, adapter: &AgentAdapterConfig) -> AgentKind {
    AgentKind::from_adapter_id(id).unwrap_or_else(|| AgentKind::Unknown(adapter_label(adapter, id)))
}

fn title_agent_hint(title: &str) -> Option<(String, AgentKind)> {
    let lower = title.to_ascii_lowercase();
    for (id, adapter) in built_in_agent_adapters() {
        if adapter.title_patterns.iter().any(|pattern| {
            let pattern = pattern.trim();
            !pattern.is_empty() && agent_pattern_matches(&lower, pattern)
        }) {
            return Some((id.clone(), adapter_kind_from_id(&id, &adapter)));
        }
    }
    None
}

fn visible_agent_kind_hint(text: &str, adapter: Option<&AgentAdapterConfig>) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    adapter_patterns(adapter, PatternField::Visible)
        .into_iter()
        .find(|pattern| agent_pattern_matches_pre_lowered(&lower, pattern))
}

fn visible_agent_match_from_adapters(
    text: &str,
    adapters: impl Iterator<Item = (String, AgentAdapterConfig)>,
) -> Option<(String, AgentKind)> {
    adapters.into_iter().find_map(|(id, adapter)| {
        if !adapter.enabled {
            return None;
        }
        visible_agent_kind_hint(text, Some(&adapter))
            .is_some()
            .then(|| (id.clone(), adapter_kind_from_id(&id, &adapter)))
    })
}

fn visible_model_hint(text: &str, adapter: Option<&AgentAdapterConfig>) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    adapter_patterns(adapter, PatternField::Model)
        .into_iter()
        .find(|pattern| agent_pattern_matches_pre_lowered(&lower, pattern))
}

fn infer_agent_status_from_visible_text(text: &str) -> AgentStatus {
    let recent: Vec<&str> = text.lines().rev().take(20).collect();
    // The "esc to interrupt" hint is printed only while the agent is actively
    // working. It is the authoritative running signal: the input prompt box is
    // drawn below the spinner, so a bottom-up scan would otherwise read the
    // prompt and report WaitingForInput mid-run.
    for line in &recent {
        let lower = line.to_ascii_lowercase();
        if lower.contains("esc to interrupt") {
            return AgentStatus::Running;
        }
    }
    // Spinner glyph on a recent line also indicates active work.
    for line in &recent {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(
            trimmed.chars().next(),
            Some('✻' | '✽' | '✶' | '✷' | '✳' | '✢' | '∗')
        ) {
            return AgentStatus::Running;
        }
        break;
    }
    // Otherwise, a bare prompt at the bottom means it is waiting for input.
    for line in &recent {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed, "❯" | ">" | "›")
            || trimmed.starts_with("❯ ")
            || trimmed.starts_with("> ")
            || trimmed.starts_with("› ")
        {
            return AgentStatus::WaitingForInput;
        }
        break;
    }
    AgentStatus::Unknown
}

fn should_load_visible_agent_text(
    matched_without_visible: bool,
    detect_processes: bool,
    explicit_status: Option<&str>,
) -> bool {
    detect_processes && (!matched_without_visible || explicit_status.is_none())
}

fn waiting_notification_update(
    enabled: bool,
    current_status: AgentStatus,
    previous_status: Option<AgentStatus>,
    previous_wait_notification: Option<Instant>,
    now: Instant,
) -> (bool, Option<Instant>) {
    if current_status != AgentStatus::WaitingForInput {
        return (false, None);
    }

    let transitioned_to_waiting = previous_status != Some(AgentStatus::WaitingForInput);
    let throttle_allows = previous_wait_notification
        .map(|last| now.duration_since(last) >= WAITING_NOTIFICATION_THROTTLE)
        .unwrap_or(true);
    let should_notify = enabled && transitioned_to_waiting && throttle_allows;
    let last_wait_notification = if should_notify {
        Some(now)
    } else {
        previous_wait_notification
    };

    (should_notify, last_wait_notification)
}

fn visible_text_fingerprint(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    })
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
        let id = kind.config_key();
        self.agent_adapter_config_by_id(id)
            .map(|adapter| adapter.enabled)
            .unwrap_or(true)
    }

    fn agent_adapter_config_by_id(&self, id: Option<&str>) -> Option<AgentAdapterConfig> {
        let id = id?;
        let base = built_in_agent_adapters().get(id);
        let configured = self.config.agent_ui.adapters.get(id);
        match (base, configured) {
            (Some(base), Some(configured)) => Some(merge_agent_adapter_config(base, configured)),
            (Some(base), None) => Some(base.clone()),
            (None, Some(configured)) => Some(configured.clone()),
            (None, None) => None,
        }
    }

    fn merged_agent_adapters(&self) -> Arc<Vec<(String, AgentAdapterConfig)>> {
        let gen = self.config.generation();
        {
            let cached = self.adapter_cache.borrow();
            if let Some((cached_gen, ref adapters)) = *cached {
                if cached_gen == gen {
                    return Arc::clone(adapters);
                }
            }
        }
        let mut adapters = Vec::new();
        for id in DEFAULT_AGENT_ADAPTER_IDS {
            if let Some(base) = built_in_agent_adapters().get(id) {
                let adapter = self
                    .config
                    .agent_ui
                    .adapters
                    .get(id)
                    .map(|configured| merge_agent_adapter_config(base, configured))
                    .unwrap_or_else(|| base.clone());
                adapters.push((id.to_string(), adapter));
            }
        }
        for (id, adapter) in &self.config.agent_ui.adapters {
            if !DEFAULT_AGENT_ADAPTER_IDS.contains(&id.as_str()) {
                adapters.push((id.clone(), adapter.clone()));
            }
        }
        let result = Arc::new(adapters);
        *self.adapter_cache.borrow_mut() = Some((gen, Arc::clone(&result)));
        result
    }

    fn configured_agent_match(
        &self,
        process: Option<&str>,
        title: &str,
    ) -> Option<(String, AgentKind)> {
        let process = process.map(|process| basename(process).to_ascii_lowercase());
        let title = title.to_ascii_lowercase();

        for (id, adapter) in self.merged_agent_adapters().iter().cloned() {
            if !adapter.enabled {
                continue;
            }
            if let Some(process) = &process {
                if adapter
                    .process_names
                    .iter()
                    .any(|name| basename(name).eq_ignore_ascii_case(process))
                {
                    return Some((id.clone(), adapter_kind_from_id(&id, &adapter)));
                }
            }
            if adapter.title_patterns.iter().any(|pattern| {
                let pattern = pattern.trim();
                !pattern.is_empty() && agent_pattern_matches(&title, pattern)
            }) {
                return Some((id.clone(), adapter_kind_from_id(&id, &adapter)));
            }
        }

        None
    }

    fn agent_supported_actions(
        &self,
        adapter_id: Option<&str>,
        vars: &HashMap<String, String>,
        cwd: Option<&Path>,
        trusted_controls: bool,
    ) -> AgentActions {
        let mut actions = PassiveAgentAdapter.supported_actions(vars);
        actions.attach = false;
        let controls_allowed = agent_control_actions_allowed(
            self.config.agent_ui.enable_control_actions,
            trusted_controls,
            vars,
        );
        if controls_allowed {
            if let Some(adapter) = self.agent_adapter_config_by_id(adapter_id) {
                let values = AgentActionTemplateValues::from_vars(vars, cwd);
                actions.attach = resolve_agent_attach_command(&adapter, &values).is_some();
                actions.resume = resolve_agent_resume_command(&adapter, &values).is_some();
                actions.open_logs =
                    resolve_agent_detail_path(adapter_id, &adapter, &values).is_some();
            }
        }
        actions
    }

    fn visible_agent_text(&self, pane: &Arc<dyn Pane>) -> String {
        let dims = pane.get_dimensions();
        let start = dims.physical_top;
        let end = dims.physical_top + dims.viewport_rows.min(120) as isize;
        let mut text = String::new();
        for logical in pane.get_logical_lines(start..end) {
            text.push_str(&line_to_string(&logical.logical));
            text.push('\n');
        }
        text
    }

    fn visible_agent_match(&self, text: &str) -> Option<(String, AgentKind)> {
        visible_agent_match_from_adapters(text, self.merged_agent_adapters().iter().cloned())
    }

    fn relevant_agent_user_vars(vars: &HashMap<String, String>) -> Vec<(String, String)> {
        let mut relevant = vars
            .iter()
            .filter(|(key, _)| {
                key.starts_with("agent.")
                    || key.starts_with("agent_")
                    || matches!(key.as_str(), "WEZTERM_PROG" | "PROG")
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        relevant.sort();
        relevant
    }

    /// Whether the given pane is currently detected as an agent pane.
    /// Exposed for the rich-input composer's agent-only gating.
    pub(crate) fn pane_is_agent(&self, pane: &Arc<dyn Pane>) -> bool {
        self.detect_agent_pane(pane).is_some()
    }

    fn detect_agent_pane(&self, pane: &Arc<dyn Pane>) -> Option<AgentPaneState> {
        if !self.config.agent_ui.enabled {
            return None;
        }

        let vars = pane.copy_user_vars();
        let foreground_process = pane.get_foreground_process_name(CachePolicy::AllowStale);
        let pane_title = pane.get_title();
        let dims = pane.get_dimensions();
        let mut visible_text = String::new();
        let mut visible_text_loaded = false;
        let cache_key = AgentDetectionCacheKey {
            foreground_process: foreground_process.clone(),
            pane_title: pane_title.clone(),
            relevant_user_vars: Self::relevant_agent_user_vars(&vars),
            viewport_top: dims.physical_top,
            viewport_rows: dims.viewport_rows,
            visible_fingerprint: 0,
        };
        let previous_entry = self
            .agent_detection_cache
            .borrow()
            .get(&pane.pane_id())
            .cloned();
        let previous_wait_notification = previous_entry
            .as_ref()
            .and_then(|entry| entry.last_wait_notification);

        // Fast path: if cheap fields are unchanged and the cache entry is fresh,
        // skip all adapter merging, visible-text loading, and FS probing.
        if let Some(entry) = previous_entry.as_ref() {
            let k = &entry.key;
            if k.foreground_process == cache_key.foreground_process
                && k.pane_title == cache_key.pane_title
                && k.relevant_user_vars == cache_key.relevant_user_vars
                && k.viewport_top == cache_key.viewport_top
                && k.viewport_rows == cache_key.viewport_rows
                && entry.detected_at.elapsed() < Duration::from_millis(500)
            {
                return entry.state.clone();
            }
        }

        let explicit_adapter_id = user_var(&vars, "agent.adapter").map(str::to_ascii_lowercase);
        let explicit_kind = user_var(&vars, "agent.kind").map(|kind| {
            let merged = self.merged_agent_adapters();
            let resolved = AgentKind::from_hint_with_adapters(kind, &merged)
                .unwrap_or_else(|| AgentKind::from_user_var(kind));
            let adapter_id = explicit_adapter_id
                .clone()
                .or_else(|| resolved.config_key().map(ToString::to_string));
            (adapter_id, resolved)
        });
        let configured_kind = if self.config.agent_ui.detect_processes {
            self.configured_agent_match(foreground_process.as_deref(), &pane_title)
                .map(|(id, kind)| (Some(id), kind))
        } else {
            None
        };
        let title_kind = if self.config.agent_ui.detect_processes {
            title_agent_hint(&pane_title).map(|(id, kind)| (Some(id), kind))
        } else {
            None
        };
        let metadata_kind = if has_agent_metadata_evidence(&vars) {
            Some((None, AgentKind::Unknown("Agent".to_string())))
        } else {
            None
        };
        let explicit_status = user_var(&vars, "agent.status");
        let matched_without_visible = explicit_kind
            .as_ref()
            .or(title_kind.as_ref())
            .or(configured_kind.as_ref())
            .or(metadata_kind.as_ref())
            .is_some();
        let visible_kind = if should_load_visible_agent_text(
            matched_without_visible,
            self.config.agent_ui.detect_processes,
            explicit_status,
        ) {
            visible_text = self.visible_agent_text(pane);
            visible_text_loaded = true;
            if !matched_without_visible {
                self.visible_agent_match(&visible_text)
                    .map(|(id, kind)| (Some(id), kind))
            } else {
                None
            }
        } else {
            None
        };
        let visible_fingerprint = if visible_text_loaded {
            visible_text_fingerprint(&visible_text)
        } else {
            0
        };
        let cache_key = AgentDetectionCacheKey {
            visible_fingerprint,
            ..cache_key
        };
        if let Some(entry) = previous_entry.as_ref() {
            if entry.key == cache_key {
                return entry.state.clone();
            }
        }
        let trusted_controls = explicit_kind.is_some()
            || title_kind.is_some()
            || configured_kind.is_some()
            || truthy_agent_var(&vars, "agent.enable_control_actions");
        let control_actions_allowed = agent_control_actions_allowed(
            self.config.agent_ui.enable_control_actions,
            trusted_controls,
            &vars,
        );

        let Some((adapter_id, kind)) = explicit_kind
            .or(title_kind)
            .or(configured_kind)
            .or(visible_kind)
            .or(metadata_kind)
        else {
            // Sticky detection: a pane previously detected as an agent should not
            // vanish just because the identifying banner scrolled out of the
            // visible region. If the cheap identity fields (foreground process,
            // pane title, relevant user vars) are unchanged, retain the last known
            // state so the toolbelt stays stable across streamed messages.
            let sticky_state = previous_entry.as_ref().and_then(|entry| {
                let k = &entry.key;
                if entry.state.is_some()
                    && k.foreground_process == cache_key.foreground_process
                    && k.pane_title == cache_key.pane_title
                    && k.relevant_user_vars == cache_key.relevant_user_vars
                {
                    entry.state.clone()
                } else {
                    None
                }
            });
            self.agent_detection_cache.borrow_mut().insert(
                pane.pane_id(),
                AgentDetectionCacheEntry {
                    key: cache_key,
                    state: sticky_state.clone(),
                    last_wait_notification: previous_wait_notification,
                    detected_at: Instant::now(),
                },
            );
            return sticky_state;
        };
        let adapter = self.agent_adapter_config_by_id(adapter_id.as_deref());
        if adapter_id.is_some()
            && !adapter
                .as_ref()
                .map(|adapter| adapter.enabled)
                .unwrap_or(true)
        {
            self.agent_detection_cache.borrow_mut().insert(
                pane.pane_id(),
                AgentDetectionCacheEntry {
                    key: cache_key,
                    state: None,
                    last_wait_notification: previous_wait_notification,
                    detected_at: Instant::now(),
                },
            );
            return None;
        }
        if adapter_id.is_none() && !self.agent_adapter_enabled(&kind) {
            self.agent_detection_cache.borrow_mut().insert(
                pane.pane_id(),
                AgentDetectionCacheEntry {
                    key: cache_key,
                    state: None,
                    last_wait_notification: previous_wait_notification,
                    detected_at: Instant::now(),
                },
            );
            return None;
        }

        let cwd = pane_working_dir(pane);
        let status = if explicit_status.is_some() {
            AgentStatus::from_hint(explicit_status)
        } else if visible_text_loaded {
            infer_agent_status_from_visible_text(&visible_text)
        } else {
            AgentStatus::Unknown
        };
        let actions = self.agent_supported_actions(
            adapter_id.as_deref(),
            &vars,
            cwd.as_deref(),
            trusted_controls,
        );
        let model = user_var(&vars, "agent.model")
            .map(ToString::to_string)
            .or_else(|| {
                visible_text_loaded
                    .then(|| visible_model_hint(&visible_text, adapter.as_ref()))
                    .flatten()
            });
        let state = Some(AgentPaneState {
            adapter_id: adapter_id.clone(),
            kind,
            trusted_controls: control_actions_allowed,
            status,
            model,
            session_id: user_var(&vars, "agent.session_id")
                .or_else(|| user_var(&vars, "agent.session"))
                .map(ToString::to_string),
            attach_url: user_var(&vars, "agent.attach_url")
                .or_else(|| user_var(&vars, "agent.attach"))
                .map(ToString::to_string),
            cwd,
            input_tokens: parse_u64_var(&vars, "agent.input_tokens"),
            output_tokens: parse_u64_var(&vars, "agent.output_tokens"),
            cost: user_var(&vars, "agent.cost")
                .or_else(|| user_var(&vars, "agent.estimated_cost"))
                .map(ToString::to_string),
            actions,
        });
        let previous_status = previous_entry
            .as_ref()
            .and_then(|entry| entry.state.as_ref())
            .map(|state| state.status.clone());
        let (should_notify_waiting, last_wait_notification) = waiting_notification_update(
            self.config.agent_ui.waiting_notification,
            state.as_ref().unwrap().status.clone(),
            previous_status,
            previous_wait_notification,
            Instant::now(),
        );
        if should_notify_waiting {
            let label = state.as_ref().unwrap().kind.label().to_string();
            promise::spawn::spawn_into_main_thread(async move {
                wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                    title: "Agent waiting".to_string(),
                    message: format!("{} is waiting for input", label),
                    url: None,
                    timeout: Some(Duration::from_millis(2500)),
                });
            })
            .detach();
        }
        self.agent_detection_cache.borrow_mut().insert(
            pane.pane_id(),
            AgentDetectionCacheEntry {
                key: cache_key,
                state: state.clone(),
                last_wait_notification,
                detected_at: Instant::now(),
            },
        );
        state
    }

    fn prune_agent_detection_cache(&self) {
        let mux = Mux::get();
        let Some(window) = mux.get_window(self.mux_window_id) else {
            self.agent_detection_cache.borrow_mut().clear();
            return;
        };
        let mut live = HashSet::new();
        for tab_idx in 0..window.len() {
            if let Some(tab) = window.get_by_idx(tab_idx) {
                if let Some(pane) = tab.get_active_pane() {
                    live.insert(pane.pane_id());
                }
                for pos in tab.iter_panes_ignoring_zoom() {
                    live.insert(pos.pane.pane_id());
                }
            }
        }
        self.agent_detection_cache
            .borrow_mut()
            .retain(|pane_id, _| live.contains(pane_id));
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

    fn agent_pane_raw_conversation_text(&self, pane: &Arc<dyn Pane>) -> String {
        let dims = pane.get_dimensions();
        let end = dims.physical_top + dims.viewport_rows as isize;
        let max_rows = self.config.agent_ui.copy_scrollback_lines.max(1) as isize;
        let start = agent_transcript_start(dims.scrollback_top, end, max_rows);
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

    pub(crate) fn agent_pane_conversation_text(&self, pane: &Arc<dyn Pane>) -> String {
        let raw = self.agent_pane_raw_conversation_text(pane);
        let adapter = self
            .detect_agent_pane(pane)
            .and_then(|agent| self.agent_adapter_config_by_id(agent.adapter_id.as_deref()));
        agent_copy_payload_from_text(&AgentCopyAction::Conversation, &raw, "", adapter.as_ref())
    }

    pub(crate) fn agent_pane_markdown_text(&self, pane: &Arc<dyn Pane>) -> String {
        let raw = self.agent_pane_raw_conversation_text(pane);
        let adapter = self
            .detect_agent_pane(pane)
            .and_then(|agent| self.agent_adapter_config_by_id(agent.adapter_id.as_deref()));
        agent_copy_payload_from_text(&AgentCopyAction::Markdown, &raw, "", adapter.as_ref())
    }

    pub(crate) fn agent_pane_last_message_text(&self, pane: &Arc<dyn Pane>) -> String {
        let raw = self.agent_pane_raw_conversation_text(pane);
        if raw.trim().is_empty() {
            return self.agent_pane_summary(pane);
        }

        let adapter = self
            .detect_agent_pane(pane)
            .and_then(|agent| self.agent_adapter_config_by_id(agent.adapter_id.as_deref()));
        let message = agent_copy_payload_from_text(
            &AgentCopyAction::LastAgentMessage,
            &raw,
            "",
            adapter.as_ref(),
        );
        if message.is_empty() {
            clean_agent_conversation_transcript(&raw, adapter.as_ref())
        } else {
            message
        }
    }

    pub(crate) fn agent_resume_pane(&self, pane: &Arc<dyn Pane>) {
        let Some(agent) = self.detect_agent_pane(pane) else {
            return;
        };
        let Some(adapter_id) = agent.adapter_id.as_deref() else {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent control".to_string(),
                message: "Resume is not configured for this agent".to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2200)),
            });
            return;
        };
        let Some(adapter) = self.agent_adapter_config_by_id(Some(adapter_id)) else {
            return;
        };
        if !agent.trusted_controls {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent control".to_string(),
                message:
                    "Resume requires trusted agent evidence or explicit agent_ui control enablement"
                        .to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        if !agent.actions.resume {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent control".to_string(),
                message: "Resume is not configured or its command is not on PATH".to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        let values = AgentActionTemplateValues::from_agent(&agent);
        let Some(argv) = resolve_agent_resume_command(&adapter, &values) else {
            return;
        };
        let label = adapter_label(&adapter, adapter_id);
        self.spawn_command(
            &SpawnCommand {
                label: Some(format!("{label} Resume")),
                args: Some(argv),
                cwd: agent.cwd,
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
        wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
            title: "Agent control".to_string(),
            message: format!("Started {label} resume in a new tab"),
            url: None,
            timeout: Some(Duration::from_millis(2200)),
        });
    }

    pub(crate) fn agent_attach_pane(&self, pane: &Arc<dyn Pane>) {
        let Some(agent) = self.detect_agent_pane(pane) else {
            return;
        };
        let Some(adapter_id) = agent.adapter_id.as_deref() else {
            return;
        };
        let Some(adapter) = self.agent_adapter_config_by_id(Some(adapter_id)) else {
            return;
        };
        if !agent.trusted_controls {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent control".to_string(),
                message:
                    "Attach requires trusted agent evidence or explicit agent_ui control enablement"
                        .to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        if !agent.actions.attach {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent control".to_string(),
                message:
                    "Attach is not configured, missing an attach URL, or its command is not on PATH"
                        .to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        let values = AgentActionTemplateValues::from_agent(&agent);
        let Some(argv) = resolve_agent_attach_command(&adapter, &values) else {
            return;
        };
        let label = adapter_label(&adapter, adapter_id);
        self.spawn_command(
            &SpawnCommand {
                label: Some(format!("{label} Attach")),
                args: Some(argv),
                cwd: agent.cwd,
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
        wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
            title: "Agent control".to_string(),
            message: format!("Started {label} attach in a new tab"),
            url: None,
            timeout: Some(Duration::from_millis(2200)),
        });
    }

    pub(crate) fn agent_open_logs_for_pane(&self, pane: &Arc<dyn Pane>) {
        let Some(agent) = self.detect_agent_pane(pane) else {
            return;
        };
        let Some(adapter_id) = agent.adapter_id.as_deref() else {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent details".to_string(),
                message: "Details are not configured for this agent".to_string(),
                url: None,
                timeout: Some(Duration::from_millis(2200)),
            });
            return;
        };
        let Some(adapter) = self.agent_adapter_config_by_id(Some(adapter_id)) else {
            return;
        };
        let detail_label = agent_detail_button_label(Some(adapter_id), Some(&adapter));
        let title = if detail_label == "Logs" {
            "Agent logs"
        } else {
            "Agent details"
        };
        if !agent.trusted_controls {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: title.to_string(),
                message: format!(
                    "Opening {detail_label} requires trusted agent evidence or explicit agent_ui control enablement"
                ),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        if !agent.actions.open_logs {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: title.to_string(),
                message: format!("{detail_label} are not configured or no details path exists"),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        }
        let values = AgentActionTemplateValues::from_agent(&agent);
        let Some(path) = resolve_agent_detail_path(Some(adapter_id), &adapter, &values) else {
            return;
        };
        let url = if path.is_dir() {
            Url::from_directory_path(&path)
        } else {
            Url::from_file_path(&path)
        };
        match url {
            Ok(url) => {
                wezterm_open_url::open_url(url.as_str());
                wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                    title: title.to_string(),
                    message: format!("Opening {}", path.display()),
                    url: None,
                    timeout: Some(Duration::from_millis(2200)),
                });
            }
            Err(()) => {
                wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                    title: title.to_string(),
                    message: "Agent details path could not be opened".to_string(),
                    url: None,
                    timeout: Some(Duration::from_millis(2200)),
                });
            }
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

    fn sidebar_scroll_geometry(&self) -> Option<SidebarScrollGeometry> {
        let (track_y, track_h, visible, total) = self.sidebar_scroll_track_bounds()?;
        SidebarScrollGeometry::new(
            track_y,
            track_h,
            visible,
            total,
            self.sidebar_scroll_offset,
            self.sidebar_row_height() as f32 * 0.75,
        )
    }

    fn sidebar_scroll_thumb_bounds(&self) -> Option<(f32, f32, usize)> {
        let geometry = self.sidebar_scroll_geometry()?;
        Some((geometry.thumb_y, geometry.thumb_h, geometry.max_offset))
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

    pub(crate) fn scroll_sidebar_thumb_top_to(&mut self, thumb_top: isize) -> bool {
        let Some(geometry) = self.sidebar_scroll_geometry() else {
            return false;
        };

        let Some(next) = geometry.offset_for_thumb_top(thumb_top as f32) else {
            return false;
        };
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
        if self.config.agent_telemetry.enabled {
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
        let agent = if self.config.agent_ui.enabled && self.config.agent_ui.show_sidebar_badges {
            pane.as_ref().and_then(|pane| self.detect_agent_pane(pane))
        } else {
            None
        };
        let adapter = agent
            .as_ref()
            .and_then(|agent| self.agent_adapter_config_by_id(agent.adapter_id.as_deref()));
        let command = pane
            .as_ref()
            .and_then(|pane| pane.get_foreground_process_name(CachePolicy::AllowStale))
            .map(|name| basename(&name));
        let pane_title = pane.as_ref().map(|pane| pane.get_title());
        let symbol = compact_tab_symbol(
            title,
            tab_idx,
            agent.as_ref().map(|agent| &agent.kind),
            adapter.as_ref(),
            command.as_deref(),
            pane_title.as_deref(),
        );
        let color = compact_tab_color(
            title,
            tab_idx,
            agent.as_ref().map(|agent| &agent.kind),
            adapter.as_ref(),
            command.as_deref(),
            pane_title.as_deref(),
        );
        (symbol, color)
    }

    fn sidebar_agent_metadata(&self, agent: &AgentPaneState) -> Vec<String> {
        let mut items = Vec::new();
        if !self.config.agent_telemetry.enabled {
            return items;
        }
        let fields = self.config.agent_telemetry.fields.clone();
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
        self.prune_agent_detection_cache();
        let Some(agent) = self.detect_agent_pane(&pos.pane) else {
            return Ok(());
        };

        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;
        let cell_w_f = self.render_metrics.cell_size.width as f32;
        let cell_h_f = self.render_metrics.cell_size.height as f32;
        // DPI-scaled geometry: the strip/button boxes must be derived from the
        // (DPI-scaled) font cell metrics, not just the unscaled layout
        // constants below, or the label glyphs spill outside the hand-drawn
        // rounded rects on HiDPI/Retina displays. Mirrors the dpi_scale idiom
        // used by paint_scrollbar_edge_overlay in paint.rs.
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let vpad = 6. * dpi_scale;
        let strip_h = (cell_h_f + 2. * vpad).max(AGENT_TOOLBELT_H * dpi_scale);
        let button_margin = 5. * dpi_scale;
        let button_inner_vpad = 4. * dpi_scale;
        let button_h = (cell_h_f + button_inner_vpad).min(strip_h - 2. * button_margin);
        let strip_radius = (RADIUS * dpi_scale).min(strip_h * 0.5);
        let button_radius = (5. * dpi_scale).min(button_h * 0.5);
        let pad_x = PAD_X * dpi_scale;
        let dot_size = AGENT_TOOLBELT_DOT_SIZE * dpi_scale;
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
        if pane_w < 140. || pane_h < strip_h + AGENT_TOOLBELT_GAP * 2. {
            return Ok(());
        }

        let mut buttons: Vec<(&str, AgentToolbeltAction)> = Vec::new();
        let adapter = self.agent_adapter_config_by_id(agent.adapter_id.as_deref());
        buttons.extend(agent_toolbelt_buttons(
            &self.config.agent_ui,
            &agent,
            adapter.as_ref(),
            self.config.rich_input.enabled,
            self.config.rich_input.docked,
        ));
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

        let max_tool_w = (pane_w - AGENT_TOOLBELT_RIGHT_INSET - AGENT_TOOLBELT_GAP)
            .max(1.)
            .min(AGENT_TOOLBELT_MAX_W);
        let fixed_controls_w = pad_x * 2. + dot_size + AGENT_TOOLBELT_GAP;
        let max_button_area = (max_tool_w - fixed_controls_w).max(0.);
        let mut visible_buttons = buttons
            .into_iter()
            .map(|(label, action)| {
                let width = agent_toolbelt_button_width(label, cell_width, dpi_scale);
                (label, action, width)
            })
            .collect::<Vec<_>>();
        while !visible_buttons.is_empty()
            && agent_toolbelt_button_area(&visible_buttons) > max_button_area
        {
            let remove_idx = visible_buttons
                .iter()
                .rposition(|(_, action, _)| action != &AgentToolbeltAction::CopyMenu)
                .unwrap_or(visible_buttons.len() - 1);
            visible_buttons.remove(remove_idx);
        }
        if visible_buttons.is_empty() {
            return Ok(());
        }

        let button_area = agent_toolbelt_button_area(&visible_buttons);
        let label_target_w = (label.chars().count() as f32 * cell_w_f).min(280. * dpi_scale);
        let desired_w = (fixed_controls_w + button_area + AGENT_TOOLBELT_GAP + label_target_w)
            .max(AGENT_TOOLBELT_MIN_W)
            .min(AGENT_TOOLBELT_MAX_W);
        let tool_w = desired_w
            .min(max_tool_w)
            .max(fixed_controls_w + button_area);
        let tool_x = pane_x + pane_w - tool_w - AGENT_TOOLBELT_RIGHT_INSET;
        let tool_y = match self.config.agent_ui.toolbelt_position {
            AgentToolbeltPosition::Top => pane_y + AGENT_TOOLBELT_GAP,
            AgentToolbeltPosition::Bottom => pane_y + pane_h - strip_h - AGENT_TOOLBELT_GAP,
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
        let accent = if agent.status == AgentStatus::WaitingForInput {
            LinearRgba(0.94, 0.72, 0.26, 1.0)
        } else {
            adapter_color(adapter.as_ref(), &agent.kind)
        };

        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(tool_x, tool_y, tool_w, strip_h),
            strip_radius,
            bg,
        )?;
        self.sidebar_pill_fill(
            layers,
            1,
            euclid::rect(
                tool_x + pad_x,
                tool_y + (strip_h - dot_size) * 0.5,
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

        let button_start_x = tool_x + tool_w - pad_x - button_area;
        let label_x = tool_x + pad_x + dot_size + AGENT_TOOLBELT_GAP;
        let label_w = button_start_x - AGENT_TOOLBELT_GAP - label_x;
        if label_w >= (cell_width * 6) as f32 {
            render_text(
                self,
                layers,
                &label,
                label_x,
                tool_y + (strip_h - cell_h_f) * 0.5,
                label_w,
                fg,
                bg,
                false,
            )?;
        }

        let hovered_item = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let left_pressed = self.current_mouse_buttons.contains(&MousePress::Left);
        let mut button_x = button_start_x;
        let button_right_limit = tool_x + tool_w - pad_x;
        for (button_label, action, button_w) in visible_buttons {
            if button_x + button_w > button_right_limit + 0.5 {
                break;
            }
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
            // Button box is centered within the strip and sized to enclose
            // the (DPI-scaled) glyph height; hit-rect below must track this
            // exact rect so clicks keep landing on the drawn box.
            let button_box_y = tool_y + (strip_h - button_h) * 0.5;
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(button_x, button_box_y + offset, button_w, button_h),
                button_radius,
                button_bg,
            )?;
            let button_fg = contrast_label_color(button_bg);
            let button_side_pad = AGENT_TOOLBELT_BUTTON_PAD_X * dpi_scale * 0.5;
            render_text(
                self,
                layers,
                button_label,
                button_x + button_side_pad,
                button_box_y + offset + (button_h - cell_h_f) * 0.5,
                (button_w - button_side_pad * 2.).max(1.),
                button_fg,
                button_bg,
                true,
            )?;
            self.ui_items.push(UIItem {
                x: button_x as usize,
                y: button_box_y as usize,
                width: button_w.ceil() as usize,
                height: button_h.ceil() as usize,
                item_type,
            });
            button_x += button_w + AGENT_TOOLBELT_GAP;
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
            ("Copy as Markdown", AgentCopyAction::Markdown),
            ("Copy last message", AgentCopyAction::LastAgentMessage),
            ("Copy agent details", AgentCopyAction::Summary),
        ];
        let cell_width = self.render_metrics.cell_size.width;
        let cell_height = self.render_metrics.cell_size.height;
        let cell_h_f = cell_height as f32;
        // DPI-scaled geometry: row height must enclose the (DPI-scaled) font
        // cell height, not just the fixed AGENT_COPY_MENU_ROW_H constant, or
        // labels spill outside the row on HiDPI/Retina displays.
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let row_vpad = 6. * dpi_scale;
        let row_h = (cell_h_f + 2. * row_vpad).max(AGENT_COPY_MENU_ROW_H * dpi_scale);
        let menu_pad = 8. * dpi_scale;
        let menu_radius = (RADIUS * dpi_scale).min(row_h * 0.5);
        let row_radius = (5. * dpi_scale).min(row_h * 0.5);
        let row_inset = 4. * dpi_scale;
        let row_text_inset = 12. * dpi_scale;
        let menu_w = AGENT_COPY_MENU_W;
        let menu_h = items.len() as f32 * row_h + menu_pad;
        let max_x = (self.dimensions.pixel_width as f32 - menu_w - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let max_y = (self.dimensions.pixel_height as f32 - menu_h - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let menu_x =
            (menu.x as f32 - menu_w + AGENT_TOOLBELT_MIN_BUTTON_W).clamp(AGENT_TOOLBELT_GAP, max_x);
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
            menu_radius,
            bg,
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
            let row_y = menu_y + menu_pad * 0.5 + idx as f32 * row_h;
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
                    euclid::rect(menu_x + row_inset, row_y, menu_w - row_inset * 2., row_h),
                    row_radius,
                    row_bg,
                )?;
            }
            render_text(
                self,
                layers,
                label,
                menu_x + row_text_inset,
                row_y + (row_h - cell_h_f) * 0.5,
                menu_w - row_text_inset * 2.,
                contrast_label_color(row_bg),
                row_bg,
            )?;
            self.ui_items.push(UIItem {
                x: (menu_x + row_inset) as usize,
                y: row_y as usize,
                width: (menu_w - row_inset * 2.) as usize,
                height: row_h.ceil() as usize,
                item_type,
            });
        }

        Ok(())
    }

    pub fn paint_sidebar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        self.prune_agent_detection_cache();
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
        // DPI-scaled geometry for the fixed-size hand-drawn boxes below
        // (compact rail buttons, "+" new-tab button); row_height (used by the
        // expanded search field / tab rows further down) already derives
        // from cell_height directly so it does not need this adjustment.
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
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

            // The rail button is a square icon box; its ceiling must grow
            // with the (DPI-scaled) font cell height (keeping the original
            // 46px as a scaled floor) so the glyph never exceeds the box.
            let rail_ceiling = (cell_height as f32 + 12. * dpi_scale).max(46. * dpi_scale);
            let rail_side = (width as f32 - 10.).clamp(38. * dpi_scale, rail_ceiling);
            let rail_radius = (RADIUS * dpi_scale).min(rail_side * 0.5);
            let rail_x = left + (width as f32 - rail_side) * 0.5;
            let row_stride = rail_side + GAP;
            // Auto-hide toggle occupies the first rail slot so it stays
            // reachable to turn auto-hide back off from the collapsed rail.
            let toggle_top = top + INSET;
            self.paint_sidebar_autohide_toggle(
                layers,
                euclid::rect(rail_x, toggle_top, rail_side, rail_side),
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                accent,
                hover_fill,
                pressed_fill,
            )?;
            let list_top = toggle_top + row_stride;
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
                    rail_radius,
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
                rail_radius,
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
        let toggle_side = row_height as f32;
        if width > 96 {
            // Auto-hide toggle and search share the top row: the toggle sits
            // flush at the left of the content column and the search bar fills
            // the remaining width to its right. Keeping them on one row means
            // the tab list starts right below with no leftover gap up top.
            let toggle_rect = euclid::rect(content_x, y, toggle_side, toggle_side);
            self.paint_sidebar_autohide_toggle(
                layers,
                toggle_rect,
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                accent,
                hover_fill,
                pressed_fill,
            )?;

            // Search bar is shortened by the toggle width + gap.
            let search_x = content_x + toggle_side + GAP;
            let search_w = (content_w - toggle_side - GAP).max(1.);
            let search_text_x = search_x + PAD_X + ACTIVE_TEXT_GAP;
            let search_cols = ((search_w - PAD_X * 2.).max(cell_width as f32) / cell_width as f32)
                .max(1.) as usize;

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
            let search_rect = euclid::rect(search_x, y + search_offset, search_w, row_height as f32);
            // row_height (self.sidebar_row_height()) already derives its
            // height directly from the DPI-scaled cell height, so no
            // additional height derivation is needed here; only the corner
            // radius (an unscaled px constant) needs the dpi_scale idiom.
            let search_radius = (RADIUS * dpi_scale).min(row_height as f32 * 0.5);
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
                    search_radius + dpi_scale,
                    accent.mul_alpha(0.45),
                )?;
            }
            self.sidebar_rounded_fill(layers, 1, search_rect, search_radius, search_bg)?;

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
                search_text_x,
                y + search_offset + (row_height as f32 - cell_height as f32) * 0.5,
                search_cols,
                search_w - PAD_X * 2.,
                search_fg,
                search_bg,
            )?;
            self.ui_items.push(UIItem {
                x: search_x as usize,
                y: y as usize,
                width: search_w as usize,
                height: row_height,
                item_type: UIItemType::SidebarSearch,
            });
            y += row_height as f32 + GAP;
        } else {
            // Narrow sidebar: no search field is drawn, so the toggle keeps
            // its own row at the top-left of the content column.
            let toggle_rect = euclid::rect(content_x, y, toggle_side, toggle_side);
            self.paint_sidebar_autohide_toggle(
                layers,
                toggle_rect,
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                accent,
                hover_fill,
                pressed_fill,
            )?;
            y += toggle_side + GAP;
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
        // row_height (self.sidebar_row_height()) already derives its height
        // directly from the DPI-scaled cell height, so rows already enclose
        // the glyphs; only the corner radius (an unscaled px constant) needs
        // dpi_scale applied.
        let tab_row_radius = (RADIUS * dpi_scale).min(row_height as f32 * 0.5);
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
                    tab_row_radius,
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
            if let Some(agent) = &agent {
                let badge_size = 7.;
                let badge_x = text_x;
                let badge_y = y + row_offset + (row_height as f32 - badge_size) * 0.5;
                let badge_color = if agent.status == AgentStatus::WaitingForInput {
                    LinearRgba(0.94, 0.72, 0.26, 1.0)
                } else if active {
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
            let geometry = self
                .sidebar_scroll_geometry()
                .unwrap_or(SidebarScrollGeometry {
                    track_y: tab_list_top,
                    track_h: tab_list_height,
                    thumb_y: tab_list_top,
                    thumb_h: tab_list_height,
                    max_offset: 0,
                });
            let thumb_hit_y = geometry.thumb_y.round().max(0.) as usize;
            let thumb_hit_h = geometry.thumb_h.round().max(1.) as usize;
            self.sidebar_pill_fill(
                layers,
                2,
                euclid::rect(
                    sidebar_scrollbar_x,
                    geometry.thumb_y,
                    SIDEBAR_SCROLLBAR_W,
                    geometry.thumb_h,
                ),
                SIDEBAR_SCROLLBAR_W * 0.5,
                inactive_fg.mul_alpha(0.42),
            )?;
            self.ui_items.push(UIItem {
                x: (sidebar_scrollbar_x - 8.).max(0.) as usize,
                y: geometry.track_y.round().max(0.) as usize,
                width: (SIDEBAR_SCROLLBAR_W + 16.) as usize,
                height: geometry.track_h.round().max(1.) as usize,
                item_type: UIItemType::SidebarScrollTrack,
            });
            self.ui_items.push(UIItem {
                x: (sidebar_scrollbar_x - 8.).max(0.) as usize,
                y: thumb_hit_y,
                width: (SIDEBAR_SCROLLBAR_W + 16.) as usize,
                height: thumb_hit_h,
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

    /// Paint the sidebar auto-hide toggle button into `rect` and register its
    /// hit region. The icon (a "rail + panel" glyph) is filled/accented when
    /// auto-hide is ON and dimmed when OFF, so the button reflects current state.
    fn paint_sidebar_autohide_toggle(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        rect: RectF,
        dpi_scale: f32,
        hovered_item: &Option<UIItemType>,
        left_pressed: bool,
        surface: LinearRgba,
        inactive_fg: LinearRgba,
        accent: LinearRgba,
        hover_fill: LinearRgba,
        pressed_fill: LinearRgba,
    ) -> anyhow::Result<()> {
        let item_type = UIItemType::SidebarAutoHideToggle;
        let hovered = hovered_item.as_ref() == Some(&item_type);
        let pressed = left_pressed && hovered && self.pressed_ui_item.as_ref() == Some(&item_type);
        let on = self.config.sidebar_auto_hide;

        let bg = if pressed {
            pressed_fill
        } else if hovered {
            hover_fill
        } else {
            lerp_rgba(surface, inactive_fg, 0.07)
        };
        let offset = if pressed { 1. } else { 0. };
        let box_rect = euclid::rect(
            rect.min_x(),
            rect.min_y() + offset,
            rect.size.width,
            rect.size.height,
        );
        let radius = (RADIUS * dpi_scale).min(box_rect.size.height * 0.5);
        self.sidebar_rounded_fill(layers, 1, box_rect, radius, bg)?;

        // Icon: a solid vertical "rail" bar on the left plus an outlined
        // "panel" to its right. The rail is accented when auto-hide is ON.
        let pad = (box_rect.size.width * 0.26).max(3.);
        let inner_h = (box_rect.size.height - pad * 2.).max(1.);
        let rail_color = if on {
            accent
        } else {
            inactive_fg.mul_alpha(0.55)
        };
        let rail_w = (box_rect.size.width * 0.16).max(2.);
        let rail_rect = euclid::rect(
            box_rect.min_x() + pad,
            box_rect.min_y() + pad,
            rail_w,
            inner_h,
        );
        self.sidebar_rounded_fill(layers, 2, rail_rect, rail_w * 0.4, rail_color)?;

        let panel_x = rail_rect.max_x() + rail_w * 0.6;
        let panel_w = (box_rect.max_x() - pad - panel_x).max(1.);
        let panel_rect = euclid::rect(panel_x, box_rect.min_y() + pad, panel_w, inner_h);
        let panel_color = inactive_fg.mul_alpha(if on { 0.30 } else { 0.18 });
        self.sidebar_rounded_fill(layers, 2, panel_rect, 2. * dpi_scale, panel_color)?;

        self.ui_items.push(UIItem {
            x: rect.min_x() as usize,
            y: rect.min_y() as usize,
            width: rect.size.width as usize,
            height: rect.size.height as usize,
            item_type,
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

    #[test]
    fn visible_agent_kind_detects_claude_header_when_title_changes() {
        let text = [
            "Claude Code v2.1.198",
            "Sonnet 5 with medium effort · Claude Team",
            "~/Documents/copilot/auto-time-management",
            "❯ where is my open pla?",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            visible_agent_kind_hint(&text, Some(&adapter)),
            Some("claude code".to_string())
        );
        assert_eq!(
            visible_model_hint(&text, Some(&adapter)),
            Some("sonnet".to_string())
        );
    }

    #[test]
    fn passive_visible_detection_does_not_enable_control_actions() {
        let text = "Claude Code v2.1.198\n❯ continue\n";
        assert!(
            visible_agent_match_from_adapters(text, default_agent_adapters().into_iter()).is_some()
        );
        let vars = HashMap::new();

        assert!(!agent_control_actions_allowed(false, false, &vars));
        assert!(agent_control_actions_allowed(false, true, &vars));
    }

    #[test]
    fn explicit_user_var_enables_control_actions() {
        let mut vars = HashMap::new();
        vars.insert(
            "agent.enable_control_actions".to_string(),
            "true".to_string(),
        );

        assert!(agent_control_actions_allowed(false, false, &vars));
    }

    #[test]
    fn action_command_resolves_templates_and_requires_path_command() {
        let command = env::current_exe().unwrap().to_string_lossy().to_string();
        let adapter = AgentAdapterConfig {
            resume_command: Some(vec![command.clone(), "{session_id}".to_string()]),
            resume_latest_command: Some(vec![
                "tgzterminal-definitely-missing-agent-command".to_string()
            ]),
            attach_command: Some(vec![command.clone(), "{attach_url}".to_string()]),
            ..Default::default()
        };
        let values = AgentActionTemplateValues {
            session_id: Some("session-123".to_string()),
            attach_url: Some("file:///tmp/socket".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_agent_resume_command(&adapter, &values),
            Some(vec![command.clone(), "session-123".to_string()])
        );
        assert_eq!(
            resolve_agent_attach_command(&adapter, &values),
            Some(vec![command.clone(), "file:///tmp/socket".to_string()])
        );

        let missing_session = AgentActionTemplateValues::default();
        assert!(resolve_agent_resume_command(&adapter, &missing_session).is_none());

        let missing_attach_url = AgentActionTemplateValues {
            session_id: Some("session-123".to_string()),
            ..Default::default()
        };
        assert!(resolve_agent_attach_command(&adapter, &missing_attach_url).is_none());
    }

    #[test]
    fn detail_path_uses_first_existing_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join(".codex").join("log");
        std::fs::create_dir_all(&existing).unwrap();
        let adapter = AgentAdapterConfig {
            detail_paths: Some(vec![
                "{home}/.codex/sessions/{session_id}".to_string(),
                "{home}/.codex/log".to_string(),
            ]),
            ..Default::default()
        };
        let values = AgentActionTemplateValues {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };

        assert_eq!(
            resolve_agent_detail_path(Some("codex"), &adapter, &values),
            Some(existing.canonicalize().unwrap())
        );
    }

    #[test]
    fn claude_default_detail_path_keeps_project_root_safety() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/Users/example/project");
        let projects = temp.path().join(".claude").join("projects");
        let project = projects.join(encode_claude_project_path(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let adapter = default_agent_adapters().remove("claude").unwrap();
        let values = AgentActionTemplateValues {
            cwd: Some(cwd),
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };

        assert_eq!(
            resolve_agent_detail_path(Some("claude"), &adapter, &values),
            Some(project.canonicalize().unwrap())
        );
    }

    #[test]
    fn detail_button_uses_details_for_session_paths() {
        let codex = default_agent_adapters().remove("codex").unwrap();
        let claude = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            agent_detail_button_label(Some("codex"), Some(&codex)),
            "Details"
        );
        assert_eq!(
            agent_detail_button_label(Some("claude"), Some(&claude)),
            "Logs"
        );
    }

    #[test]
    fn toolbelt_buttons_respect_global_and_adapter_visibility() {
        let mut agent_ui = config::AgentUiConfig::default();
        let agent = AgentPaneState {
            adapter_id: Some("codex".to_string()),
            kind: AgentKind::Codex,
            trusted_controls: true,
            status: AgentStatus::Running,
            model: None,
            session_id: Some("session-123".to_string()),
            attach_url: None,
            cwd: None,
            input_tokens: None,
            output_tokens: None,
            cost: None,
            actions: AgentActions {
                interrupt: true,
                copy_summary: true,
                attach: false,
                resume: true,
                open_logs: true,
            },
        };
        let adapter = AgentAdapterConfig {
            detail_paths: Some(vec!["{home}/.codex/sessions".to_string()]),
            ..Default::default()
        };

        assert!(
            !agent_toolbelt_buttons(&agent_ui, &agent, Some(&adapter), false, false).is_empty()
        );

        // The Compose button appears only when rich_input is enabled and not docked.
        assert!(
            !agent_toolbelt_buttons(&agent_ui, &agent, Some(&adapter), false, false)
                .iter()
                .any(|(_, action)| action == &AgentToolbeltAction::Compose)
        );
        assert!(
            agent_toolbelt_buttons(&agent_ui, &agent, Some(&adapter), true, false)
                .iter()
                .any(|(_, action)| action == &AgentToolbeltAction::Compose)
        );
        // With docked enabled, the Input (DockInput) button replaces Compose.
        let docked_buttons = agent_toolbelt_buttons(&agent_ui, &agent, Some(&adapter), true, true);
        assert!(docked_buttons
            .iter()
            .any(|(_, action)| action == &AgentToolbeltAction::DockInput));
        assert!(!docked_buttons
            .iter()
            .any(|(_, action)| action == &AgentToolbeltAction::Compose));

        agent_ui.show_pane_toolbelt = false;
        assert!(agent_toolbelt_buttons(&agent_ui, &agent, Some(&adapter), false, false).is_empty());

        agent_ui.show_pane_toolbelt = true;
        let disabled_adapter = AgentAdapterConfig {
            enabled: false,
            ..adapter
        };
        assert!(
            agent_toolbelt_buttons(&agent_ui, &agent, Some(&disabled_adapter), false, false)
                .is_empty()
        );
    }

    #[test]
    fn shell_prompts_are_not_agent_user_prompts() {
        assert!(!is_agent_user_prompt_line("$ echo hi"));
        assert!(!is_agent_user_prompt_line("# apt install ripgrep"));
        assert!(is_agent_user_prompt_line("❯ hello"));
    }

    #[test]
    fn agent_copy_last_message_filters_claude_startup_to_response() {
        let raw = [
            "Claude Code v2.0.1",
            "Model: Claude Sonnet",
            "CWD: /Users/example/project",
            "────────────────────────",
            "❯ explain this",
            "✻ Brewed for 5s",
            "The relevant fix is to clean the transcript before copying.",
            "",
            "ctx: 12% · 1,204 tokens",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            clean_agent_last_message_transcript(&raw, Some(&adapter)),
            "The relevant fix is to clean the transcript before copying."
        );
    }

    #[test]
    fn agent_copy_last_message_filters_current_claude_status_footer() {
        let raw = [
            "⏺ Test received. Ready.",
            "",
            "✻ Cooked for 3s",
            "",
            "",
            "────────────────────────────────────────────",
            "❯",
            "────────────────────────────────────────────",
            "  █░░░░░░░░░░░░░░░░░░░ 7% used · 93% left (70k/1000k tokens) · tim.grossinger · Sonnet 5 (1x) · 5h: 3% (resets 05:00)  7d: 0% · expires Sun Jul 05 14:00",
            "▶▶ auto mode on (shift+tab to cycle) · ← for agents",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            clean_agent_last_message_transcript(&raw, Some(&adapter)),
            "Test received. Ready."
        );
    }

    #[test]
    fn agent_copy_conversation_filters_prompt_status_token_bar_and_padding() {
        let raw = [
            "",
            "",
            "❯ write tests",
            "Done.",
            "Next line.",
            "│ token usage 1,120 tokens │",
            "⏵⏵ auto mode",
            "",
            "",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            clean_agent_conversation_transcript(&raw, Some(&adapter)),
            "write tests\nDone.\nNext line."
        );
    }

    #[test]
    fn agent_copy_conversation_preserves_meaningful_user_and_agent_content() {
        let raw = [
            "Fable 5 is back",
            "SessionStart:startup says hello",
            "❯ summarize the change",
            "It copies semantic agent content now.",
            "",
            "❯ what changed in layout",
            "Buttons are measured from their rendered labels.",
            "12:45 · reset usage · 3,200 tokens",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            clean_agent_conversation_transcript(&raw, Some(&adapter)),
            "summarize the change\nIt copies semantic agent content now.\n\nwhat changed in layout\nButtons are measured from their rendered labels."
        );
    }

    #[test]
    fn agent_copy_conversation_starts_at_first_visible_prompt() {
        let raw = [
            "▐▛███▜▌   Claude Code v2.1.198",
            "▝▜█████▛▘  Sonnet 5 with medium effort · Claude Team",
            "  ▘▘ ▝▝    ~/Documents/copilot/auto-time-management",
            "",
            " ▎ Until July 7, you can use up to 50% of your plan's weekly usage limit on Fable 5.",
            " ▎ Opus 4.8. Learn more",
            "",
            "❯ hello",
            "",
            "Hi. Ready. What need?",
            "",
            "* Brewed for 2s",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            clean_agent_conversation_transcript(&raw, Some(&adapter)),
            "hello\n\nHi. Ready. What need?"
        );
    }

    #[test]
    fn copy_as_markdown_uses_cleaned_verbatim_lines() {
        let raw = [
            "❯ show code",
            "```rust",
            "fn main() {",
            "    println!(\"hi\");",
            "}",
            "```",
            "ctx: 1%",
        ]
        .join("\n");
        let adapter = default_agent_adapters().remove("claude").unwrap();

        assert_eq!(
            agent_copy_payload_from_text(&AgentCopyAction::Markdown, &raw, "", Some(&adapter)),
            "## User\n\nshow code\n\n## Agent\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```"
        );
    }

    #[test]
    fn transcript_start_is_bounded_by_configured_line_limit() {
        assert_eq!(agent_transcript_start(-5000, 200, 500), -300);
        assert_eq!(agent_transcript_start(0, 200, 500), 0);
        assert_eq!(agent_transcript_start(-5000, 200, 0), 199);
    }

    #[test]
    fn status_inference_detects_running_and_waiting() {
        assert_eq!(
            infer_agent_status_from_visible_text("Thinking\n✻ Brewing"),
            AgentStatus::Running
        );
        assert_eq!(
            infer_agent_status_from_visible_text("Done\n\n❯"),
            AgentStatus::WaitingForInput
        );
        assert_eq!(
            infer_agent_status_from_visible_text("Here is the answer."),
            AgentStatus::Unknown
        );
    }

    #[test]
    fn process_detected_agent_loads_visible_text_for_status() {
        assert!(should_load_visible_agent_text(true, true, None));
        assert!(!should_load_visible_agent_text(true, true, Some("running")));
        assert!(should_load_visible_agent_text(false, true, None));
        assert!(!should_load_visible_agent_text(true, false, None));
    }

    #[test]
    fn waiting_notification_only_fires_on_transition() {
        let now = Instant::now();
        let (notify, marker) = waiting_notification_update(
            true,
            AgentStatus::WaitingForInput,
            Some(AgentStatus::Running),
            None,
            now,
        );
        assert!(notify);
        assert_eq!(marker, Some(now));

        let (notify, marker) = waiting_notification_update(
            true,
            AgentStatus::WaitingForInput,
            Some(AgentStatus::WaitingForInput),
            Some(now - Duration::from_secs(120)),
            now,
        );
        assert!(!notify);
        assert_eq!(marker, Some(now - Duration::from_secs(120)));

        let (notify, marker) = waiting_notification_update(
            true,
            AgentStatus::Running,
            Some(AgentStatus::WaitingForInput),
            Some(now),
            now,
        );
        assert!(!notify);
        assert_eq!(marker, None);
    }

    #[test]
    fn toolbelt_shrink_keeps_copy_before_lower_priority_actions() {
        let buttons = vec![
            ("Stop", AgentToolbeltAction::Interrupt, 88.),
            ("Copy", AgentToolbeltAction::CopyMenu, 88.),
            ("Resume", AgentToolbeltAction::Resume, 112.),
            ("Logs", AgentToolbeltAction::OpenLogs, 88.),
        ];
        let mut visible = buttons.clone();
        while !visible.is_empty() && agent_toolbelt_button_area(&visible) > 190. {
            let remove_idx = visible
                .iter()
                .rposition(|(_, action, _)| action != &AgentToolbeltAction::CopyMenu)
                .unwrap_or(visible.len() - 1);
            visible.remove(remove_idx);
        }

        assert!(visible
            .iter()
            .any(|(_, action, _)| action == &AgentToolbeltAction::CopyMenu));
        assert!(!visible
            .iter()
            .any(|(_, action, _)| action == &AgentToolbeltAction::OpenLogs));
    }

    #[test]
    fn adapter_color_and_short_label_come_from_config() {
        let adapter = AgentAdapterConfig {
            short_label: Some("Mx".to_string()),
            color: Some("#112233".to_string()),
            ..Default::default()
        };

        assert_eq!(
            adapter_short_label(Some(&adapter), &AgentKind::Unknown("Mystery".to_string())),
            "Mx"
        );
        assert_eq!(
            adapter_color(Some(&adapter), &AgentKind::Unknown("Mystery".to_string())),
            LinearRgba(17. / 255., 34. / 255., 51. / 255., 1.0)
        );
    }

    #[test]
    fn compact_symbol_does_not_use_agent_command_without_detected_agent() {
        assert_ne!(
            compact_tab_symbol("shell", 0, None, None, Some("claude"), Some("claude")),
            "Cl"
        );
        assert_ne!(
            compact_tab_symbol("shell", 0, None, None, Some("codex"), Some("codex")),
            "Cx"
        );
    }

    #[test]
    fn partial_adapter_config_preserves_default_matchers() {
        let defaults = default_agent_adapters();
        let base = defaults.get("claude").unwrap();
        let configured = AgentAdapterConfig {
            enabled: false,
            short_label: Some("Cd".to_string()),
            ..Default::default()
        };
        let merged = merge_agent_adapter_config(base, &configured);

        assert!(!merged.enabled);
        assert_eq!(merged.short_label.as_deref(), Some("Cd"));
        assert!(merged
            .process_names
            .iter()
            .any(|process| process == "claude"));
        assert!(merged
            .strip_patterns
            .iter()
            .any(|pattern| pattern == "auto mode"));
    }

    #[test]
    fn configured_custom_visible_match_returns_unknown_label() {
        let adapter = AgentAdapterConfig {
            label: Some("My Agent".to_string()),
            visible_patterns: vec!["my-agent ready".to_string()],
            ..Default::default()
        };

        assert_eq!(
            visible_agent_match_from_adapters(
                "banner\nmy-agent ready\n",
                vec![("myagent".to_string(), adapter)].into_iter()
            ),
            Some((
                "myagent".to_string(),
                AgentKind::Unknown("My Agent".to_string())
            ))
        );
    }

    #[test]
    fn adapter_patterns_support_cached_regex_prefix() {
        assert!(agent_pattern_matches(
            "Claude Sonnet 5",
            "re:sonnet\\s+\\d+"
        ));
        assert!(!agent_pattern_matches("Claude Sonnet", "re:sonnet\\s+\\d+"));
    }

    #[test]
    fn regex_patterns_are_bounded_and_invalid_patterns_do_not_match() {
        AGENT_PATTERN_REGEX_CACHE.lock().unwrap().clear();
        assert!(!agent_pattern_matches(
            "anything",
            &format!("re:{}", "a".repeat(MAX_AGENT_PATTERN_LEN + 1))
        ));
        assert!(!agent_pattern_matches("anything", "re:["));
        for idx in 0..(AGENT_PATTERN_REGEX_CACHE_LIMIT + 10) {
            let pattern = format!("re:test-{idx}");
            let _ = agent_pattern_matches("test", &pattern);
        }
        assert!(AGENT_PATTERN_REGEX_CACHE.lock().unwrap().len() <= AGENT_PATTERN_REGEX_CACHE_LIMIT);
    }

    #[test]
    fn claude_logs_path_must_resolve_under_projects_root() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/Users/example/project");
        let projects = temp.path().join(".claude").join("projects");
        let project = projects.join(encode_claude_project_path(&cwd));
        std::fs::create_dir_all(&project).unwrap();

        assert_eq!(
            resolve_claude_logs_path_under(temp.path(), &cwd).unwrap(),
            project.canonicalize().unwrap()
        );
    }

    #[test]
    fn missing_claude_logs_path_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/Users/example/project");

        assert_eq!(
            resolve_claude_logs_path_under(temp.path(), &cwd),
            Err(ClaudeLogsPathError::Missing)
        );
    }

    #[cfg(unix)]
    #[test]
    fn claude_logs_symlink_outside_projects_root_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = PathBuf::from("/Users/example/project");
        let projects = temp.path().join(".claude").join("projects");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, projects.join(encode_claude_project_path(&cwd)))
            .unwrap();

        assert_eq!(
            resolve_claude_logs_path_under(temp.path(), &cwd),
            Err(ClaudeLogsPathError::OutsideProjects)
        );
    }

    #[test]
    fn agent_copy_summary_payload_is_metadata_only() {
        let raw = [
            "❯ include transcript",
            "This response should not be appended to details.",
        ]
        .join("\n");
        let summary = "Agent: Claude\nPane: 7\nStatus: running";

        assert_eq!(
            agent_copy_payload_from_text(&AgentCopyAction::Summary, &raw, summary, None),
            summary
        );
    }

    #[test]
    fn sidebar_scroll_geometry_preserves_thumb_grab_offset() {
        let geometry = SidebarScrollGeometry::new(12., 160., 3, 100, 40, 12.).unwrap();
        let grabbed_near_bottom = geometry.thumb_y + geometry.thumb_h - 1.;
        let centered_top = grabbed_near_bottom - geometry.thumb_h * 0.5;
        let drag_delta = 18.;
        let expected_after_drag = geometry
            .offset_for_thumb_top(geometry.thumb_y + drag_delta)
            .unwrap();

        assert_eq!(geometry.offset_for_thumb_top(geometry.thumb_y).unwrap(), 40);
        assert_ne!(geometry.offset_for_thumb_top(centered_top).unwrap(), 40);

        for grab_offset in [0., geometry.thumb_h * 0.5, geometry.thumb_h - 1.] {
            let start_y = geometry.thumb_y + grab_offset;
            let current_y = start_y + drag_delta;
            let dragged_top = geometry.thumb_y + (current_y - start_y);
            assert_eq!(
                geometry.offset_for_thumb_top(dragged_top).unwrap(),
                expected_after_drag
            );
        }
    }

    #[test]
    fn sidebar_scroll_geometry_clamps_thumb_top_to_track() {
        let geometry = SidebarScrollGeometry::new(20., 120., 4, 80, 30, 10.).unwrap();

        assert_eq!(geometry.offset_for_thumb_top(-500.).unwrap(), 0);
        assert_eq!(
            geometry.offset_for_thumb_top(500.).unwrap(),
            geometry.max_offset
        );
    }

    #[test]
    fn sidebar_scroll_geometry_returns_none_without_overflow() {
        assert!(SidebarScrollGeometry::new(0., 100., 10, 10, 0, 10.).is_none());
        assert!(SidebarScrollGeometry::new(0., 100., 0, 10, 0, 10.).is_none());
        assert!(SidebarScrollGeometry::new(0., 0., 1, 10, 0, 10.).is_none());
    }
}
