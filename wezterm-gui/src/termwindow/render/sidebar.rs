use crate::agent_herd::claude::{self, ProjectDirError as ClaudeLogsPathError};
use crate::agent_herd::vendor::{AgentVendor, VendorSession};
use crate::agent_herd::{HerdAgent, HerdStatus};
use crate::quad::TripleLayerQuadAllocator;
use crate::spawn::SpawnWhere;
use crate::tabbar::TabBarItem;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::render::RenderScreenLineParams;
use crate::termwindow::{
    agent_launch, wsl_paths, AgentCopyAction, AgentLauncherEntry, AgentToolbeltAction,
    CloseTabMenuAction, CloseTabSource, ExpandedMenuRow, NewTabMenuEntry, NewTabTarget,
    SshQuickLaunchEntry, TermWindowNotif, UIItem, UIItemType,
};
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use config::{
    default_agent_adapters, AgentAdapterConfig, AgentLaunchTarget, AgentRemoteBehavior,
    AgentSplitDirection, AgentTelemetryField, AgentToolbeltPosition, ConfigHandle, SidebarPosition,
    SidebarTabDensity, SidebarTabMetadata, SidebarTabTitleSource, TabBarColors,
};
use finl_unicode::grapheme_clusters::Graphemes;
use mux::pane::{CachePolicy, Pane, PaneId};
use mux::renderable::RenderableDimensions;
use mux::tab::{PositionedPane, SplitDirection};
use mux::Mux;
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs};
use termwiz::cell::{grapheme_column_width, unicode_column_width, CellAttributes, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::surface::{Line, SEQ_ZERO};
use url::Url;
use window::color::LinearRgba;
use window::{MousePress, RectF, WindowOps};

const INSET: f32 = 8.;
const GAP: f32 = 4.;
const PAD_X: f32 = 10.;
const ACTIVE_RAIL_W: f32 = 3.;
const ACTIVE_TEXT_GAP: f32 = 7.;
/// Horizontal step a pane row is inset from its parent tab row.
const PANE_ROW_INDENT: f32 = 14.;
/// Space between the expand chevron and the label that follows it.
const CHEVRON_GAP: f32 = 4.;
const ACTION_ICON_W: f32 = 16.;
const ACTION_ICON_GAP: f32 = 8.;
const RADIUS: f32 = 7.;
const CLOSE_ZONE_W: f32 = 34.;
/// Gap between the close `×` glyph and the right edge of its zone. The text
/// reserve is derived from it in `sidebar_close_text_reserve`.
const CLOSE_GLYPH_INSET: f32 = 6.;
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
/// Narrower than the copy menu: rows are short agent names, not sentences.
const AGENT_LAUNCH_MENU_W: f32 = 200.;
/// Minimum width of the close-tab context submenu. The labels here
/// ("Close Tabs to the Right", "Close All Other Tabs") are sentences, not
/// one-word commands, so the agent-launch width clips the trailing word.
const CLOSE_TAB_MENU_MIN_W: f32 = 320.;
/// Width of the launch dropdown while the resume submenu is open. Session rows
/// carry a project name, an optional branch and a sentence of description, none
/// of which fits the width a list of agent names needs.
const AGENT_RESUME_MENU_W: f32 = 420.;
/// Ceiling on `agent_ui.launcher.resume_menu_sessions`. Each row costs a
/// transcript read, and a dropdown taller than this stops being a menu.
const MAX_RESUME_MENU_SESSIONS: u8 = 25;
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

/// Repaint cadence while a status dot is pulsing. ~30fps is ample for a 1.6s
/// breath and half the cost of the 16ms drop-flash interval.
const AGENT_PULSE_FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Slow pulse in `0.0..=1.0`, smoothstep-eased.
///
/// Derived from wall clock so every dot in the window breathes in phase with no
/// per-dot state. Smoothstep over a triangle wave is continuous in value and
/// first derivative at the turnaround, so the dot breathes instead of ticking.
fn agent_pulse_phase(elapsed: Duration, period: Duration) -> f32 {
    let period_ms = period.as_millis() as f32;
    if period_ms <= 1. {
        return 0.;
    }
    let t = (elapsed.as_millis() as f32 % period_ms) / period_ms;
    let triangle = 1. - (2. * t - 1.).abs();
    triangle * triangle * (3. - 2. * triangle)
}

/// Accent for an agent status dot.
///
/// `pulse` is `None` when nothing should animate. A phase modulates brightness
/// only, never geometry, so a pulse can never trigger a relayout. Phase 1.0 is
/// exactly the static colour, so enabling the pulse never makes a dot brighter
/// than it was before.
fn agent_status_dot_accent(
    status: &AgentStatus,
    base: LinearRgba,
    surface: LinearRgba,
    pulse: Option<f32>,
) -> LinearRgba {
    let color = if *status == AgentStatus::WaitingForInput {
        LinearRgba(0.94, 0.72, 0.26, 1.0)
    } else {
        base
    };
    match pulse {
        // Breathe between 55% and 100% of the accent against the surface behind
        // it: enough to read as motion, never dim enough to look disabled.
        Some(phase) => lerp_rgba(surface, color, 0.55 + 0.45 * phase.clamp(0., 1.)),
        None => color,
    }
}

fn opaque(color: LinearRgba) -> LinearRgba {
    LinearRgba(color.0, color.1, color.2, 1.0)
}

fn srgb8_to_linear(r: u8, g: u8, b: u8) -> LinearRgba {
    LinearRgba::with_srgba(r, g, b, 255)
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

/// Case-insensitive whole-word containment. Unlike `contains_case_insensitive`,
/// the needle must be bounded by non-alphanumeric characters (or the string
/// edges), so a short agent pattern like "amp" no longer matches inside
/// ordinary words such as "example" or "sample". Used only on the loose
/// title/visible-text detection paths, where plain substring matching produced
/// false positives on normal shell / ssh output. `haystack_lower` is assumed
/// already ASCII-lowercased; `needle` is lowercased here.
/// Bytes that *glue* a token to a neighbour rather than ending a word: path
/// separators, hyphenated compounds, HTML entities, dotted identifiers.
///
/// Treating them as plain boundaries is why `&amp;`, `/opt/amp-tools` and
/// `amp-hour` all matched the pattern "amp". They still count as boundaries when
/// they behave like punctuation — nothing alphanumeric on their far side — so
/// `starting codex.` keeps matching.
const WORD_GLUE_BYTES: &[u8] = b"-_/\\.:&;@+#~";

fn word_boundary_before(bytes: &[u8], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let previous = bytes[idx - 1];
    !previous.is_ascii_alphanumeric() && !WORD_GLUE_BYTES.contains(&previous)
}

fn word_boundary_after(bytes: &[u8], end: usize) -> bool {
    if end >= bytes.len() {
        return true;
    }
    let next = bytes[end];
    if next.is_ascii_alphanumeric() {
        return false;
    }
    if WORD_GLUE_BYTES.contains(&next) {
        // Glue only ends a word when nothing follows it, i.e. it is trailing
        // punctuation rather than joining the token to something else.
        return bytes
            .get(end + 1)
            .is_none_or(|following| !following.is_ascii_alphanumeric());
    }
    true
}

fn contains_word_lower(haystack_lower: &str, needle: &str) -> bool {
    let needle = needle.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack_lower.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack_lower[start..].find(needle.as_str()) {
        let idx = start + pos;
        let after = idx + needle.len();
        if word_boundary_before(bytes, idx) && word_boundary_after(bytes, after) {
            return true;
        }
        start = idx + 1;
    }
    false
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

/// Word-boundary variant of `agent_pattern_matches_pre_lowered`, used by the
/// title and visible-text detection paths. Regex patterns (`re:`) keep their
/// exact semantics; plain patterns must match as whole words so a short
/// literal like "amp" does not fire on "example"/"sample" in ordinary output.
fn agent_word_pattern_matches_pre_lowered(haystack_lower: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.len() > MAX_AGENT_PATTERN_LEN + 3 {
        return false;
    }
    if let Some(regex) = pattern.strip_prefix("re:") {
        cached_regex_matches(haystack_lower, regex)
    } else {
        contains_word_lower(haystack_lower, pattern)
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
#[allow(dead_code)]
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

/// Order buttons are dropped in when the strip is too narrow, first dropped
/// first. Stop and Copy are absent on purpose: Stop is the only mouse path to
/// halt a runaway agent, and Copy is the one action that never needs trust.
const AGENT_TOOLBELT_TRIM_ORDER: &[AgentToolbeltAction] = &[
    AgentToolbeltAction::DockInput,
    AgentToolbeltAction::Compose,
    AgentToolbeltAction::OpenLogs,
    AgentToolbeltAction::Attach,
    AgentToolbeltAction::Resume,
];

fn trim_agent_toolbelt_buttons(buttons: &mut Vec<(&str, AgentToolbeltAction, f32)>, max_area: f32) {
    while !buttons.is_empty() && agent_toolbelt_button_area(buttons) > max_area {
        let remove_idx = AGENT_TOOLBELT_TRIM_ORDER
            .iter()
            .find_map(|action| {
                buttons
                    .iter()
                    .position(|(_, candidate, _)| candidate == action)
            })
            // Everything droppable is gone: fall back to the rightmost
            // non-Copy button, then to the last one standing.
            .or_else(|| {
                buttons
                    .iter()
                    .rposition(|(_, action, _)| action != &AgentToolbeltAction::CopyMenu)
            })
            .unwrap_or(buttons.len() - 1);
        buttons.remove(remove_idx);
    }
}

/// Whole glyph cells that fit *entirely* inside `pixel_width`.
///
/// `render_screen_line` does not clip glyphs: it stops only once a glyph's left
/// edge has passed `pixel_width`, so the glyph straddling the boundary is
/// painted at full size and overhangs the region. Every sidebar caller must
/// therefore derive its cell count from the same pixel width it passes down.
/// Returns 0 — draw nothing — for a region narrower than one cell, rather than
/// forcing a cell that cannot fit.
fn sidebar_text_cols(pixel_width: f32, cell_width: usize) -> usize {
    if cell_width == 0 || !pixel_width.is_finite() || pixel_width < cell_width as f32 {
        return 0;
    }
    (pixel_width / cell_width as f32) as usize
}

/// Longest prefix of `text` occupying at most `cols` terminal columns.
///
/// Column-aware rather than char-aware: `Line::resize` truncates the *cell*
/// vector, so cutting a double-width grapheme leaves the wide cell as the last
/// cell and it paints a full cell past the region.
fn truncate_to_cols(text: &str, cols: usize) -> &str {
    if cols == 0 {
        return "";
    }
    let mut end = 0;
    let mut width = 0;
    for grapheme in Graphemes::new(text) {
        let grapheme_width = grapheme_column_width(grapheme, None);
        if width + grapheme_width > cols {
            break;
        }
        width += grapheme_width;
        end += grapheme.len();
    }
    &text[..end]
}

/// Like [`truncate_to_cols`] but keeps the *tail*.
///
/// The sidebar search field renders as `"{query}|"`; head-truncating it would
/// hide the caret the user is typing at.
fn truncate_to_cols_from_end(text: &str, cols: usize) -> &str {
    if cols == 0 {
        return "";
    }
    let graphemes: Vec<&str> = Graphemes::new(text).collect();
    let mut start = text.len();
    let mut width = 0;
    for grapheme in graphemes.into_iter().rev() {
        let grapheme_width = grapheme_column_width(grapheme, None);
        if width + grapheme_width > cols {
            break;
        }
        width += grapheme_width;
        start -= grapheme.len();
    }
    &text[start..]
}

/// Widest label variant that fits `cols`, candidates ordered widest first.
///
/// The render region hard-truncates to whole cells with no ellipsis, so instead
/// of letting "Worktree" clip to "Workt" we pick the widest whole variant that
/// fits. `None` means not even the narrowest rung fits, which callers answer by
/// drawing their icon or dot alone.
fn fit_label<'a>(candidates: &[&'a str], cols: usize) -> Option<&'a str> {
    if cols == 0 {
        return None;
    }
    candidates
        .iter()
        .copied()
        .find(|candidate| unicode_column_width(candidate, None) <= cols)
}

/// Diameter of an agent status dot. Single source of truth: the sidebar tab
/// badge and the launcher button used to disagree (a fixed 7px vs a cell-derived
/// clamp), so the same status read differently in two places.
fn sidebar_status_dot_size(cell_height: f32) -> f32 {
    (cell_height * 0.42).clamp(5., 10.)
}

/// Horizontal space a row reserves for the status dot, dot plus its trailing gap.
fn sidebar_agent_badge_w(cell_height: f32) -> f32 {
    sidebar_status_dot_size(cell_height) + GAP
}

/// Horizontal space a row reserves for the close `×` when laying out text.
///
/// Smaller than `CLOSE_ZONE_W` (the hit target) because the glyph is
/// right-aligned inside that zone, so text may run closer before truncating.
/// Derived from `cell_width` because the glyph's left edge moves with DPI — the
/// previous fixed 22px was calibrated for one cell size and under-reserved on
/// hidpi while wasting room at 1x.
fn sidebar_close_text_reserve(cell_width: f32) -> f32 {
    (cell_width + CLOSE_GLYPH_INSET + GAP).min(CLOSE_ZONE_W)
}

/// Horizontal composition of a sidebar row's leading decorations.
///
/// The chevron and the agent status dot are painted *before* the title, so the
/// title's origin **and** its width must both step past them. Getting only the
/// width right is what painted the leading "N: " tab index on top of the
/// chevron and the status dot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SidebarRowColumns {
    /// Only meaningful when `chevron_w > 0`.
    pub chevron_x: f32,
    /// Glyph cell plus its trailing gap; the gap itself is not painted.
    pub chevron_w: f32,
    /// Only meaningful when `badge_w > 0`.
    pub badge_x: f32,
    pub badge_w: f32,
    /// Shared by the title line and the metadata sub-line.
    pub text_x: f32,
    pub text_w: f32,
}

fn sidebar_row_columns(
    label_x: f32,
    label_w: f32,
    cell_width: f32,
    cell_height: f32,
    has_chevron: bool,
    has_agent_badge: bool,
) -> SidebarRowColumns {
    let chevron_w = if has_chevron {
        cell_width + CHEVRON_GAP
    } else {
        0.
    };
    let badge_w = if has_agent_badge {
        sidebar_agent_badge_w(cell_height)
    } else {
        0.
    };
    let text_x = label_x + chevron_w + badge_w;
    SidebarRowColumns {
        chevron_x: label_x,
        chevron_w,
        badge_x: label_x + chevron_w,
        badge_w,
        text_x,
        // Clamped at zero rather than at one cell: a region narrower than a
        // glyph must render nothing, not overhang.
        text_w: (label_w - chevron_w - badge_w).max(0.),
    }
}

/// Vertical composition of a row: y offsets from the row top for the title line
/// and, when shown, the metadata sub-line.
fn sidebar_row_text_offsets(
    row_height: f32,
    cell_height: f32,
    show_metadata: bool,
) -> (f32, Option<f32>) {
    if show_metadata {
        // Clamped at the row top: a row too short for two lines (a Compact row
        // that somehow shows metadata) must start inside itself rather than
        // drawing the title above the row.
        let primary = ((row_height - cell_height * 2.) * 0.5).max(0.);
        (primary, Some(primary + cell_height))
    } else {
        (((row_height - cell_height) * 0.5).max(0.), None)
    }
}

/// Horizontal composition of the shared Worktree / agent-launcher row.
///
/// Both halves derive from one split point so they can never overlap, whichever
/// side the scrollbar gutter is on (the content column is narrower than the row
/// and is not centered in it). The two *fills* still reach the row's outer
/// edges; only the text budgets are confined to the content column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SidebarBottomRowLayout {
    pub worktree_fill_x: f32,
    pub worktree_fill_w: f32,
    pub worktree_icon_x: f32,
    pub worktree_text_x: f32,
    pub worktree_text_w: f32,
    pub agent_fill_x: f32,
    pub agent_fill_w: f32,
    pub agent_dot_x: f32,
    pub agent_text_x: f32,
    pub agent_text_w: f32,
}

fn sidebar_bottom_row_layout(
    item_x: f32,
    item_w: f32,
    content_x: f32,
    content_w: f32,
    dot_size: f32,
    has_agent: bool,
) -> SidebarBottomRowLayout {
    let worktree_icon_x = content_x + PAD_X;
    if !has_agent {
        return SidebarBottomRowLayout {
            worktree_fill_x: item_x,
            worktree_fill_w: item_w.max(1.),
            worktree_icon_x,
            worktree_text_x: worktree_icon_x + ACTION_ICON_W + ACTION_ICON_GAP,
            worktree_text_w: (content_w - PAD_X * 2. - ACTION_ICON_W - ACTION_ICON_GAP).max(0.),
            agent_fill_x: 0.,
            agent_fill_w: 0.,
            agent_dot_x: 0.,
            agent_text_x: 0.,
            agent_text_w: 0.,
        };
    }

    let content_half = ((content_w - GAP) * 0.5).max(0.);
    let agent_x = content_x + content_half + GAP;
    let agent_w = (content_w - content_half - GAP).max(0.);
    let agent_dot_x = agent_x + PAD_X;
    let agent_text_x = agent_dot_x + dot_size + ACTION_ICON_GAP;
    SidebarBottomRowLayout {
        worktree_fill_x: item_x,
        worktree_fill_w: (agent_x - GAP - item_x).max(1.),
        worktree_icon_x,
        worktree_text_x: worktree_icon_x + ACTION_ICON_W + ACTION_ICON_GAP,
        worktree_text_w: (content_half - PAD_X * 2. - ACTION_ICON_W - ACTION_ICON_GAP).max(0.),
        agent_fill_x: agent_x,
        // The pill reaches the row's right edge even though the text budget is
        // confined to the content column.
        agent_fill_w: (item_x + item_w - agent_x).max(1.),
        agent_dot_x,
        agent_text_x,
        agent_text_w: (agent_x + agent_w - PAD_X - agent_text_x).max(0.),
    }
}

/// Density factor applied to the configured sidebar widths.
///
/// The config values are calibrated for a 2x display, so a 2x display uses them
/// verbatim and a 1x display halves them. Free-standing rather than a method so
/// window creation, which has no `TermWindow` yet, resolves the same width the
/// paint path will.
pub(crate) fn sidebar_width_scale_for_dpi(dpi: f64) -> f32 {
    #[cfg(target_os = "macos")]
    let base_dpi = 72.0_f32;
    #[cfg(not(target_os = "macos"))]
    let base_dpi = 96.0_f32;
    let backing_scale = (dpi as f32 / base_dpi).max(0.1);
    (backing_scale / 2.0).clamp(0.5, 1.25)
}

pub(crate) fn sidebar_collapsed_width_for_config(config: &ConfigHandle, dpi: f64) -> usize {
    let scaled = (config.sidebar_collapsed_width_px as f32 * sidebar_width_scale_for_dpi(dpi))
        .round() as usize;
    if config.sidebar_auto_hide {
        scaled.max(MIN_AUTO_HIDE_RAIL_W)
    } else {
        scaled
    }
}

pub(crate) fn sidebar_expanded_width_for_config(config: &ConfigHandle, dpi: f64) -> usize {
    let scaled =
        (config.sidebar_width_px as f32 * sidebar_width_scale_for_dpi(dpi)).round() as usize;
    scaled.max(sidebar_collapsed_width_for_config(config, dpi))
}

/// Width the window reserves for the sidebar at creation, before any
/// `TermWindow` exists. Mirrors `sidebar_reserved_width` for the initial frame.
pub(crate) fn sidebar_reserved_width_for_config(config: &ConfigHandle, dpi: f64) -> usize {
    if config.sidebar_auto_hide {
        sidebar_collapsed_width_for_config(config, dpi)
    } else {
        sidebar_expanded_width_for_config(config, dpi)
    }
}

/// Label ladders for the sidebar's fixed-width buttons, widest first.
const WORKTREE_LABELS: [&str; 3] = ["Worktree", "Tree", "Wt"];
const NEW_TAB_LABELS: [&str; 3] = ["+ New Tab", "+ Tab", "+"];
const SSH_ROW_LABELS: [&str; 3] = ["SSH Connect", "SSH", ">_"];
const SEARCH_PLACEHOLDER_LABELS: [&str; 3] = ["Search tabs...", "Search...", "Search"];

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

/// Side length (px) of the square icon box in the collapsed auto-hide rail
/// (toggle button, per-tab icon, "+ New Tab"). Must be tall enough to fully
/// enclose a text glyph's line height (`cell_height`) — otherwise ascenders
/// (e.g. the "l" in the Claude "Cl" badge) poke past the rounded pill fill,
/// which is visible because the glyph cell itself renders transparent.
fn sidebar_rail_icon_side(rail_width: f32, cell_height: f32, dpi_scale: f32) -> f32 {
    // Ceiling grows with the (DPI-scaled) font cell height, keeping the
    // original 46px as a scaled floor, so the box has room to grow.
    let rail_ceiling = (cell_height + 12. * dpi_scale).max(46. * dpi_scale);
    // Preferred size leaves a small margin on each side of the strip. Floored
    // at cell_height (+4px) so a narrow collapsed rail can't squeeze the box
    // shorter than the glyph's line height; that floor is itself capped just
    // inside rail_width so the box never spills past both edges of the strip.
    let min_fit = (cell_height + 4.).min(rail_width - 2.).max(1.);
    (rail_width - 8.).max(min_fit).min(rail_ceiling).max(1.)
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

/// One drawable line of the expanded sidebar list.
///
/// Built by `TermWindow::sidebar_rows`. Flattening tabs and their panes into a
/// single sequence is what lets the scroll offset, the visible-row window and
/// the scrollbar all speak in the same units.
#[derive(Debug, Clone)]
pub(crate) enum SidebarRow {
    Tab {
        tab_idx: usize,
        active: bool,
        title: String,
        metadata: Vec<String>,
        /// Panes in this tab. A count above one is what earns an expand
        /// chevron; single-pane tabs stay exactly as they were.
        pane_count: usize,
        expanded: bool,
    },
    Pane {
        pane_id: PaneId,
        active: bool,
        label: String,
        /// Marks a pane living on another host, so a local agent split off an
        /// SSH shell is visibly distinct from the shell it sits beside.
        is_remote: bool,
    },
}

/// A tab and its panes, gathered from the mux before row assembly.
pub(crate) struct SidebarTabInput {
    tab_idx: usize,
    active: bool,
    title: String,
    metadata: Vec<String>,
    panes: Vec<SidebarRow>,
}

/// Flatten tabs and their panes into the sidebar's row sequence.
///
/// Split out from `TermWindow::sidebar_rows` so the ordering, filtering and
/// expansion rules can be tested without a live window: everything above this
/// point is mux lookups, everything below is pure list assembly.
fn assemble_sidebar_rows(
    tabs: Vec<SidebarTabInput>,
    expanded_tabs: &HashSet<usize>,
    query: Option<&str>,
) -> Vec<SidebarRow> {
    let query = query.filter(|query| !query.is_empty());
    let mut rows = Vec::new();

    for tab in tabs {
        if let Some(query) = query {
            if !query_matches(&tab.title, &tab.metadata, query) {
                continue;
            }
        }

        // A tab matching the search keeps its pane children, so expanding a
        // filtered result still shows what is inside it. A single-pane tab is
        // never expandable: its one pane is what the tab row already describes.
        let pane_count = tab.panes.len();
        let expanded = pane_count > 1 && expanded_tabs.contains(&tab.tab_idx);

        rows.push(SidebarRow::Tab {
            tab_idx: tab.tab_idx,
            active: tab.active,
            title: tab.title,
            metadata: tab.metadata,
            pane_count,
            expanded,
        });
        if expanded {
            rows.extend(tab.panes);
        }
    }

    rows
}

/// Case-insensitive substring match of `query` against a tab's title or any of
/// its metadata fields.
fn query_matches(title: &str, metadata: &[String], query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    title.to_lowercase().contains(&query)
        || metadata
            .iter()
            .any(|item| item.to_lowercase().contains(&query))
}

/// Whether a launch should be pulled back to this machine.
///
/// Split out from `TermWindow` so the policy is testable without a window: the
/// two inputs are the configured behavior and whether the active pane looks
/// like a session elsewhere.
fn agent_launch_forced_local(behavior: AgentRemoteBehavior, pane_looks_remote: bool) -> bool {
    matches!(behavior, AgentRemoteBehavior::ForceLocal) && pane_looks_remote
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

pub(crate) fn pane_working_dir(pane: &Arc<dyn Pane>) -> Option<PathBuf> {
    pane.get_current_working_dir(CachePolicy::AllowStale)
        .and_then(|url| url.to_file_path().ok())
}

/// Encode a working directory the way Claude Code names its project folders.
///
/// This used to be a second, subtly wrong copy of the same mapping: it dashed
/// separators and colons but not **dots**, so a home directory like
/// `/Users/first.last` encoded to `-Users-first.last-…` and matched nothing,
/// silently disabling the Claude `Logs` action for every user with a dot in
/// their path. There is now one implementation, in [`claude`].
fn encode_claude_project_path(cwd: &Path) -> String {
    claude::encode_project_path(cwd)
}

fn resolve_claude_logs_path_under(home: &Path, cwd: &Path) -> Result<PathBuf, ClaudeLogsPathError> {
    claude::resolve_project_dir(home, cwd)
}

/// Flatten a color to an 8-bit-per-channel sRGB triple.
///
/// The overview renders into terminal cells, which take sRGB components, and
/// `parse_adapter_color` already stores config hex values componentwise, so this
/// is a plain scale rather than a gamma conversion.
#[allow(dead_code)]
fn srgb8(color: LinearRgba) -> (u8, u8, u8) {
    let to_u8 = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    (to_u8(color.0), to_u8(color.1), to_u8(color.2))
}

/// Translate pane-detection status into the herd overview's vocabulary.
///
/// `Exited` becomes `Done`: from the overview's point of view a finished agent
/// is a result you haven't collected yet, not an error.
fn herd_status_from_agent(status: AgentStatus) -> HerdStatus {
    match status {
        AgentStatus::Running | AgentStatus::Streaming => HerdStatus::Working,
        AgentStatus::WaitingForInput => HerdStatus::Blocked,
        AgentStatus::Idle => HerdStatus::Idle,
        AgentStatus::Exited => HerdStatus::Done,
        AgentStatus::Unknown => HerdStatus::Unknown,
    }
}

/// Every pid in this pane's foreground process tree.
///
/// This is what lets an agent discovered on disk be matched to the pane that
/// owns it: the agent's own pid is somewhere in this set, usually below a shell
/// or a `node` wrapper rather than being the process-group leader itself.
///
/// `AllowStale` is deliberate — a fresh process walk must never happen on a
/// per-frame path.
#[allow(dead_code)]
fn foreground_process_pids(pane: &Arc<dyn Pane>) -> HashSet<u32> {
    fn flatten(info: &procinfo::LocalProcessInfo, pids: &mut HashSet<u32>) {
        pids.insert(info.pid);
        for child in info.children.values() {
            flatten(child, pids);
        }
    }

    let mut pids = HashSet::new();
    if let Some(info) = pane.get_foreground_process_info(CachePolicy::AllowStale) {
        flatten(&info, &mut pids);
    }
    pids
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

/// Control actions (Resume / Attach / Details) require **both** an explicit
/// opt-in **and** trusted evidence.
///
/// This was previously an OR, which meant trusted evidence alone unlocked
/// process-spawning actions even with `enable_control_actions = false` — the
/// opposite of the documented model, and strictly looser than the default the
/// config comment promises. The per-pane `agent.enable_control_actions` var acts
/// as the opt-in for a single pane, not as a bypass of the evidence check.
fn agent_control_actions_allowed(
    config_enabled: bool,
    trusted_controls: bool,
    vars: &HashMap<String, String>,
) -> bool {
    let opted_in = config_enabled || truthy_agent_var(vars, "agent.enable_control_actions");
    opted_in && trusted_controls
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

/// Directories probed after `$PATH` when resolving an agent command.
///
/// A GUI app launched from Finder/Dock inherits launchd's minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`), not the login shell's, so an agent
/// installed under the user's home — Claude Code lands in `~/.local/bin` — is
/// invisible to a plain PATH probe and its launcher button never appears.
fn fallback_command_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".local/bin",
            "bin",
            ".claude/local",
            ".bun/bin",
            ".cargo/bin",
            ".volta/bin",
            ".npm-global/bin",
            ".yarn/bin",
        ] {
            dirs.push(home.join(rel));
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// Absolute path of an installed command, or `None` when it is not installed.
///
/// Callers spawn the resolved path rather than the bare name: the child
/// inherits this process's PATH, so a name found only through
/// `fallback_command_dirs` would fail to exec.
fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return path_is_executable(command_path).then(|| command_path.to_path_buf());
    }
    let path_dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(fallback_command_dirs())
        .map(|dir| dir.join(command))
        .find(|candidate| path_is_executable(candidate))
}

#[allow(dead_code)]
fn command_exists_on_path(command: &str) -> bool {
    resolve_command_path(command).is_some()
}

fn resolve_agent_command(
    command: Option<&Vec<String>>,
    values: &AgentActionTemplateValues,
) -> Option<Vec<String>> {
    let command = command?;
    if command.is_empty() {
        return None;
    }
    let mut argv = command
        .iter()
        .map(|arg| expand_agent_action_template(arg, values))
        .collect::<Option<Vec<_>>>()?;
    // Absolute path, not the bare name: see `resolve_command_path`.
    let program = resolve_command_path(argv.first()?)?;
    argv[0] = program.to_string_lossy().into_owned();
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
        running_patterns: if configured.running_patterns.is_empty() {
            base.running_patterns.clone()
        } else {
            configured.running_patterns.clone()
        },
        chrome_patterns: if configured.chrome_patterns.is_empty() {
            base.chrome_patterns.clone()
        } else {
            configured.chrome_patterns.clone()
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
        launch_command: configured
            .launch_command
            .clone()
            .or_else(|| base.launch_command.clone()),
        launch_domain: configured
            .launch_domain
            .clone()
            .or_else(|| base.launch_domain.clone()),
    }
}

/// Shells present on this machine, as `(label, argv)`.
///
/// WezTerm ships an empty `launch_menu` on every platform, so without this the
/// new-tab dropdown would have nothing to offer but domains.
fn discovered_shells() -> Vec<(String, Vec<String>)> {
    let mut shells = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |label: &str, argv: Vec<String>, shells: &mut Vec<(String, Vec<String>)>| {
        if seen.insert(argv.clone()) {
            shells.push((label.to_string(), argv));
        }
    };

    #[cfg(windows)]
    {
        // Friendly names rather than executable names: this list is read by
        // people, not shells.
        for (label, program) in [
            ("PowerShell", "powershell.exe"),
            ("PowerShell 7", "pwsh.exe"),
            ("Command Prompt", "cmd.exe"),
        ] {
            if command_exists_on_path(program) {
                push(label, vec![program.to_string()], &mut shells);
            }
        }
        // Git Bash is not on PATH in a default install, so probe the two
        // standard locations directly.
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            if path_is_executable(Path::new(candidate)) {
                push("Git Bash", vec![candidate.to_string()], &mut shells);
                break;
            }
        }
    }

    #[cfg(unix)]
    {
        // /etc/shells is the system's own list of login shells; anything in it
        // is by definition intended to be used interactively.
        let listed = fs::read_to_string("/etc/shells").unwrap_or_default();
        for line in listed.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let path = Path::new(line);
            if !path_is_executable(path) {
                continue;
            }
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| line.to_string());
            push(&label, vec![line.to_string()], &mut shells);
        }
    }

    shells
}

/// One row in a sidebar dropdown. `dot_color` and `checkbox` are mutually
/// exclusive leading markers; a row with neither is plain text.
struct SidebarDropdownRow {
    label: String,
    dot_color: Option<LinearRgba>,
    checkbox: Option<bool>,
    /// Draw a separator band above this row.
    divider_above: bool,
    /// Indent this row under the item above it, e.g. a launch-target row
    /// nested under its agent.
    indent: bool,
    /// Draw a trailing `>` chevron, marking a row that expands into a
    /// submenu rather than acting immediately.
    trailing_chevron: bool,
    item_type: UIItemType,
}

/// Walk up from `start` to the nearest directory containing one of `markers`.
///
/// Returns `None` when no marker is found before the filesystem root. The depth
/// cap is a cheap guard against pathological symlink/mount layouts; real trees
/// are nowhere near it.
fn nearest_project_root(start: &Path, markers: &[String]) -> Option<PathBuf> {
    const MAX_DEPTH: usize = 64;

    if markers.is_empty() {
        return None;
    }
    let mut dir = Some(start);
    for _ in 0..MAX_DEPTH {
        let current = dir?;
        for marker in markers {
            let marker = marker.trim();
            // A blank or path-shaped marker would let a config typo escape the
            // walk (e.g. "../x"); only plain directory entry names are valid.
            if marker.is_empty() || Path::new(marker).components().count() != 1 {
                continue;
            }
            if current.join(marker).exists() {
                return Some(current.to_path_buf());
            }
        }
        dir = current.parent();
    }
    None
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
    /// What identified this agent. Carried on the state so the cache can decide
    /// whether the identity may be reused, and whether its trust bit still holds.
    evidence: AgentEvidence,
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

fn herd_agent_from_vendor_session(session: &VendorSession) -> HerdAgent {
    let project_root = session
        .project_root
        .clone()
        .or_else(|| crate::agent_herd::project_root_for(&session.cwd));
    HerdAgent {
        name: session
            .name
            .clone()
            .unwrap_or_else(|| session.vendor.label().to_string()),
        provider: session.vendor.label().to_ascii_lowercase(),
        vendor: session.vendor.clone(),
        status: session.status,
        blocked_reason: session.blocked_reason.clone(),
        model: None,
        cwd: Some(session.cwd.clone()),
        project_root,
        git_branch: None,
        pid: Some(session.pid),
        pane_id: None,
        session_id: (!session.session_id.is_empty()).then(|| session.session_id.clone()),
        started_at: session.started_at,
        status_changed_at: session.status_changed_at,
        subagents: session.subagents.clone(),
        activity: None,
    }
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
    /// When the current status was last observed fresh, for the running grace.
    status_at: Instant,
    /// Consecutive detections agreeing on an adapter that is not the
    /// established one, for the identity switch hysteresis.
    pending_switch: Option<(String, u32)>,
}

/// Agent identity carried over from this pane's previous detection.
#[derive(Clone, Debug, PartialEq)]
struct StickyAgentIdentity {
    adapter_id: Option<String>,
    kind: AgentKind,
    evidence: AgentEvidence,
    trusted_controls: bool,
}

/// Identity to reuse when the current frame found no fresh evidence.
///
/// Title and visible-text evidence are both transient: agents rewrite the pane
/// title as their task changes, and the identifying startup banner scrolls out
/// of the visible region. Neither means the agent went away, so the badge must
/// not flicker off. What it *must not* do is outlive its evidence indefinitely,
/// which is how one bad frame pinned a wrong badge for the whole life of a
/// `node` process. So reuse is scoped to the class that earned it:
///
/// - process / user-var identities last as long as that anchor is unchanged;
/// - title-derived identities die with the title they came from, whatever the
///   process is;
/// - visible-text and metadata identities expire after a TTL and must be
///   re-earned.
fn sticky_agent_identity(
    previous: Option<&AgentDetectionCacheEntry>,
    key: &AgentDetectionCacheKey,
    now: Instant,
    trust_visible: bool,
) -> Option<StickyAgentIdentity> {
    let entry = previous?;
    let state = entry.state.as_ref()?;
    let previous_key = &entry.key;
    if previous_key.relevant_user_vars != key.relevant_user_vars
        || previous_key.foreground_process != key.foreground_process
    {
        return None;
    }
    if !state.evidence.survives_title_change() && previous_key.pane_title != key.pane_title {
        return None;
    }
    match state.evidence {
        AgentEvidence::VisibleChrome | AgentEvidence::Metadata => {
            if now.duration_since(entry.detected_at) >= AGENT_STICKY_VISIBLE_TTL {
                return None;
            }
        }
        _ => {}
    }
    Some(StickyAgentIdentity {
        adapter_id: state.adapter_id.clone(),
        kind: state.kind.clone(),
        evidence: state.evidence,
        // Trust never outlives the class that granted it: a bit computed while
        // `trust_visible_evidence` was on must not survive turning it off.
        trusted_controls: state.trusted_controls && state.evidence.is_trusted(trust_visible),
    })
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

/// Strength of the evidence that identified an agent, strongest first.
///
/// `Ord` *is* the precedence policy. Nothing else may reorder candidates — in
/// particular adapter map order must never decide identity, which is how an
/// alphabetically-first adapter ("amp") won ties against a correct process-name
/// match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgentEvidence {
    /// `agent.kind` / `agent.adapter` user vars.
    UserVar,
    /// Foreground process basename matched `process_names`.
    Process,
    /// A multi-word `title_patterns` entry matched the pane title.
    TitlePhrase,
    /// The adapter's own TUI chrome is on screen, with enough exclusive signals
    /// agreeing. Cannot be produced by merely echoing a brand name.
    VisibleChrome,
    /// A single bare brand token matched the title. A pane title is prose the
    /// user or the agent typed ("fix the amp meter bug"), so it ranks below the
    /// running agent's own chrome.
    TitleToken,
    /// Generic `agent.*` telemetry with no identity of its own.
    Metadata,
}

impl AgentEvidence {
    /// Evidence classes allowed to unlock Resume/Attach/Details.
    fn is_trusted(self, trust_visible: bool) -> bool {
        match self {
            Self::UserVar | Self::Process | Self::TitlePhrase | Self::TitleToken => true,
            Self::VisibleChrome => trust_visible,
            Self::Metadata => false,
        }
    }

    /// Whether an identity from this class survives a pane retitling itself.
    ///
    /// Title-derived identities must not: they were only ever as good as the
    /// title they came from, and Claude rewrites its title every turn.
    fn survives_title_change(self) -> bool {
        !matches!(self, Self::TitlePhrase | Self::TitleToken)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentIdentityCandidate {
    adapter_id: Option<String>,
    kind: AgentKind,
    evidence: AgentEvidence,
    /// Distinct adapter-exclusive patterns that agreed. Breaks ties *within* one
    /// evidence class only.
    signals: u32,
}

/// Strongest class wins; within a class, most agreeing signals wins.
///
/// A genuine tie between two *different* adapters is never broken by ordering:
/// for the weak classes that shape is precisely the false positive, so it
/// yields `None`; for strong classes something is certainly there and it yields
/// an unnamed agent instead of guessing which one.
fn resolve_agent_identity(
    mut candidates: Vec<AgentIdentityCandidate>,
) -> Option<AgentIdentityCandidate> {
    candidates.sort_by(|a, b| a.evidence.cmp(&b.evidence).then(b.signals.cmp(&a.signals)));
    let best = candidates.first()?.clone();
    let tied_other_adapter = candidates
        .iter()
        .skip(1)
        .filter(|candidate| {
            candidate.evidence == best.evidence && candidate.signals == best.signals
        })
        .any(|candidate| candidate.adapter_id != best.adapter_id);
    if !tied_other_adapter {
        return Some(best);
    }
    match best.evidence {
        AgentEvidence::UserVar | AgentEvidence::Process | AgentEvidence::TitlePhrase => {
            Some(AgentIdentityCandidate {
                adapter_id: None,
                kind: AgentKind::Unknown("Agent".to_string()),
                ..best
            })
        }
        _ => None,
    }
}

/// Every adapter whose `title_patterns` match, in deterministic order.
///
/// A multi-word pattern is a phrase and outranks visible text; a bare token is
/// weak evidence. Matching is word-boundary in both cases — the plain substring
/// matcher this path used before made "amp" fire on "example" and "&amp;".
fn title_agent_candidates(
    title: &str,
    adapters: impl Iterator<Item = (String, AgentAdapterConfig)>,
) -> Vec<AgentIdentityCandidate> {
    let lower = title.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for (id, adapter) in adapters {
        if !adapter.enabled {
            continue;
        }
        let mut phrase_hits = 0;
        let mut token_hits = 0;
        for pattern in &adapter.title_patterns {
            let pattern = pattern.trim();
            if pattern.is_empty() || !agent_word_pattern_matches_pre_lowered(&lower, pattern) {
                continue;
            }
            if pattern.starts_with("re:") || pattern.contains(char::is_whitespace) {
                phrase_hits += 1;
            } else {
                token_hits += 1;
            }
        }
        let (evidence, signals) = if phrase_hits > 0 {
            (AgentEvidence::TitlePhrase, phrase_hits)
        } else if token_hits > 0 {
            (AgentEvidence::TitleToken, token_hits)
        } else {
            continue;
        };
        candidates.push(AgentIdentityCandidate {
            kind: adapter_kind_from_id(&id, &adapter),
            adapter_id: Some(id),
            evidence,
            signals,
        });
    }
    candidates
}

/// Lowercased patterns claimed by more than one enabled adapter.
///
/// They still drive status but carry zero identity weight: "esc to interrupt" is
/// printed by Claude, Codex and Copilot alike and cannot say which one it is.
fn ambiguous_agent_patterns(adapters: &[(String, AgentAdapterConfig)]) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (_, adapter) in adapters {
        if !adapter.enabled {
            continue;
        }
        let mut adapter_patterns: HashSet<String> = HashSet::new();
        for pattern in adapter
            .visible_patterns
            .iter()
            .chain(&adapter.running_patterns)
            .chain(&adapter.chrome_patterns)
        {
            let pattern = pattern.trim().to_ascii_lowercase();
            if !pattern.is_empty() {
                adapter_patterns.insert(pattern);
            }
        }
        for pattern in adapter_patterns {
            *seen.entry(pattern).or_insert(0) += 1;
        }
    }
    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(pattern, _)| pattern)
        .collect()
}

/// Distinct adapter-exclusive visible patterns matching `text_lower`, as
/// `(identity_total, chrome_or_running_hits)`.
fn visible_identity_signals(
    text_lower: &str,
    adapter: &AgentAdapterConfig,
    ambiguous: &HashSet<String>,
) -> (u32, u32) {
    let mut identity = 0;
    let mut chrome = 0;
    let mut matched: HashSet<String> = HashSet::new();
    let chrome_and_running: HashSet<&String> = adapter
        .running_patterns
        .iter()
        .chain(&adapter.chrome_patterns)
        .collect();
    for pattern in adapter
        .visible_patterns
        .iter()
        .chain(&adapter.running_patterns)
        .chain(&adapter.chrome_patterns)
    {
        let trimmed = pattern.trim();
        let lowered = trimmed.to_ascii_lowercase();
        if trimmed.is_empty() || ambiguous.contains(&lowered) || !matched.insert(lowered) {
            continue;
        }
        if !agent_word_pattern_matches_pre_lowered(text_lower, trimmed) {
            continue;
        }
        identity += 1;
        if chrome_and_running.contains(pattern) {
            chrome += 1;
        }
    }
    (identity, chrome)
}

/// Adapters whose visible evidence clears the bar, as `VisibleChrome` candidates.
///
/// The bar is: at least one chrome/running hit — waived for adapters that
/// declare none, so user-configured adapters behave as before — plus enough
/// exclusive hits overall. The chrome requirement is load-bearing: this project's
/// own config source contains the literals "claude code" and "claude team" on
/// adjacent lines, so brand phrases alone would badge any pane merely reading it.
fn visible_agent_candidates(
    text: &str,
    adapters: &[(String, AgentAdapterConfig)],
    ambiguous: &HashSet<String>,
    min_signals: u32,
) -> Vec<AgentIdentityCandidate> {
    let lower = text.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for (id, adapter) in adapters {
        if !adapter.enabled {
            continue;
        }
        let declares_chrome =
            !adapter.chrome_patterns.is_empty() || !adapter.running_patterns.is_empty();
        let (identity, chrome) = visible_identity_signals(&lower, adapter, ambiguous);
        if identity == 0 || (declares_chrome && chrome == 0) {
            continue;
        }
        let exclusive_total = adapter
            .visible_patterns
            .iter()
            .chain(&adapter.running_patterns)
            .chain(&adapter.chrome_patterns)
            .filter(|pattern| !ambiguous.contains(&pattern.trim().to_ascii_lowercase()))
            .count() as u32;
        // Clamped by what the adapter actually declares, so a custom adapter
        // with one distinctive string still matches at the default threshold.
        if identity < min_signals.min(exclusive_total.max(1)) {
            continue;
        }
        candidates.push(AgentIdentityCandidate {
            kind: adapter_kind_from_id(id, adapter),
            adapter_id: Some(id.clone()),
            evidence: AgentEvidence::VisibleChrome,
            signals: identity,
        });
    }
    candidates
}

#[allow(dead_code)]
fn visible_agent_kind_hint(text: &str, adapter: Option<&AgentAdapterConfig>) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    adapter_patterns(adapter, PatternField::Visible)
        .into_iter()
        .find(|pattern| agent_word_pattern_matches_pre_lowered(&lower, pattern))
}

#[allow(dead_code)]
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

/// Running markers common to agent CLIs, used when the adapter is unknown or
/// declares none of its own. Private rather than configurable: these are facts
/// about how agent TUIs behave, not preferences.
const GENERIC_AGENT_RUNNING_MARKERS: &[&str] = &[
    "esc to interrupt",
    "press esc to stop",
    "esc to cancel",
    "ctrl+c to interrupt",
    "(interrupt)",
];

/// Spinner glyphs agents lead their working line with.
const AGENT_SPINNER_GLYPHS: &[char] = &['✻', '✽', '✶', '✷', '✳', '✢', '∗'];

/// How long a `Running` status is held after its marker disappears.
const AGENT_RUNNING_GRACE: Duration = Duration::from_secs(3);

/// How long a visible-text or metadata identity may be reused without fresh
/// evidence before it has to be re-earned.
const AGENT_STICKY_VISIBLE_TTL: Duration = Duration::from_secs(30);

/// Infer status from the pane's visible region.
///
/// The previous implementation looked only at the last 20 lines of an up-to-120
/// *logical* line blob. Wrapped agent output collapses dozens of screen rows into
/// one logical line and the docked input strip adds trailing lines, so the
/// running marker routinely fell outside that tail, status went Unknown, and the
/// Stop button disappeared mid-run. The whole visible region is scanned instead:
/// it is the live screen, never scrollback, so a marker found there is current by
/// construction and the input is already bounded.
fn infer_agent_status_from_visible_text(
    text: &str,
    adapter: Option<&AgentAdapterConfig>,
) -> AgentStatus {
    let lower = text.to_ascii_lowercase();
    let adapter_markers = adapter
        .map(|adapter| adapter.running_patterns.clone())
        .unwrap_or_default();
    if adapter_markers
        .iter()
        .any(|pattern| agent_pattern_matches_pre_lowered(&lower, pattern))
    {
        return AgentStatus::Running;
    }
    if GENERIC_AGENT_RUNNING_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return AgentStatus::Running;
    }
    // A spinner anywhere in the last few non-blank lines counts: agents redraw
    // the spinner and the prompt box independently, so the spinner is not
    // reliably the very last line.
    let tail: Vec<&str> = text
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .collect();
    if tail.iter().any(|line| {
        line.chars()
            .next()
            .is_some_and(|c| AGENT_SPINNER_GLYPHS.contains(&c))
    }) {
        return AgentStatus::Running;
    }
    // Otherwise, a bare prompt at the bottom means it is waiting for input.
    if let Some(last) = tail.first() {
        if matches!(*last, "❯" | ">" | "›")
            || last.starts_with("❯ ")
            || last.starts_with("> ")
            || last.starts_with("› ")
        {
            return AgentStatus::WaitingForInput;
        }
    }
    AgentStatus::Unknown
}

/// Hold `Running` for a short grace period after its marker vanishes.
///
/// Agent TUIs repaint the spinner asynchronously, so a frame captured between
/// redraws shows neither a running marker nor a prompt. Dropping to `Unknown`
/// there is what made the Stop button flicker. A prompt line or an explicit
/// `agent.status` ends the grace immediately.
fn stabilize_agent_status(
    fresh: AgentStatus,
    previous: Option<AgentStatus>,
    previous_at: Option<Instant>,
    now: Instant,
    grace: Duration,
) -> AgentStatus {
    if fresh != AgentStatus::Unknown {
        return fresh;
    }
    let was_running = matches!(
        previous,
        Some(AgentStatus::Running) | Some(AgentStatus::Streaming)
    );
    let within_grace = previous_at.is_some_and(|at| now.duration_since(at) < grace);
    if was_running && within_grace {
        return previous.unwrap_or(AgentStatus::Unknown);
    }
    fresh
}

/// Whether an established identity may be replaced by a fresh candidate.
///
/// Stronger evidence switches immediately; equal-or-weaker evidence must agree
/// twice in a row. Without this, a pane whose title changes every turn re-rolls
/// its identity from whatever brand token the new title happens to contain,
/// which is what made a Claude pane flip to Codex and back.
fn agent_identity_switch_allowed(
    established: Option<(&str, AgentEvidence)>,
    candidate_id: Option<&str>,
    candidate_evidence: AgentEvidence,
    agreements: u32,
) -> bool {
    let Some((established_id, established_evidence)) = established else {
        return true;
    };
    if candidate_id == Some(established_id) {
        return true;
    }
    candidate_evidence < established_evidence || agreements >= 2
}

fn should_load_visible_agent_text(
    strongest: Option<AgentEvidence>,
    detect_processes: bool,
    explicit_status: Option<&str>,
) -> bool {
    if !detect_processes {
        return false;
    }
    // Loaded for status whenever the pane does not publish one, and for identity
    // *arbitration* whenever the best evidence so far is weak — a bare title
    // token must not outvote the running agent's own chrome.
    explicit_status.is_none()
        || matches!(
            strongest,
            None | Some(AgentEvidence::TitleToken)
                | Some(AgentEvidence::TitlePhrase)
                | Some(AgentEvidence::Metadata)
        )
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
        // A manual drag-resize is an explicit physical-pixel choice by the
        // user, so it is kept verbatim; only the configured default scales.
        match self.sidebar_drag_width {
            Some(w) => w.max(self.sidebar_collapsed_width()),
            None => sidebar_expanded_width_for_config(&self.config, self.dimensions.dpi as f64),
        }
    }

    pub fn sidebar_collapsed_width(&self) -> usize {
        sidebar_collapsed_width_for_config(&self.config, self.dimensions.dpi as f64)
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

    /// Installed agents offered by the sidebar launcher, in merged-adapter
    /// order (built-ins first, so Claude leads by default).
    ///
    /// Building this probes `$PATH` with filesystem metadata calls, so the
    /// result is cached per config generation and must never be rebuilt per
    /// frame — see the P1 finding in `docs/REPO_AUDIT_FIX_PLAN.md`.
    pub fn agent_launcher_entries(&self) -> Arc<Vec<AgentLauncherEntry>> {
        let gen = self.config.generation();
        {
            let cached = self.launcher_cache.borrow();
            if let Some((cached_gen, ref entries)) = *cached {
                if cached_gen == gen {
                    return Arc::clone(entries);
                }
            }
        }

        let mut entries = Vec::new();
        if self.config.agent_ui.enabled && self.config.agent_ui.launcher.enabled {
            for (id, adapter) in self.merged_agent_adapters().iter() {
                if !adapter.enabled {
                    continue;
                }
                let Some(argv) = adapter.launch_command.as_ref() else {
                    continue;
                };
                let Some(program) = argv.first() else {
                    continue;
                };
                // Availability is the whole discovery mechanism: an agent the
                // user has not installed simply never appears in the UI. The
                // resolved absolute path is what gets spawned — see
                // `resolve_command_path`.
                let Some(resolved) = resolve_command_path(program) else {
                    continue;
                };
                let mut argv = argv.clone();
                argv[0] = resolved.to_string_lossy().into_owned();
                let kind = adapter_kind_from_id(id, adapter);
                entries.push(AgentLauncherEntry {
                    adapter_id: id.clone(),
                    label: adapter_label(adapter, id),
                    short_label: adapter_short_label(Some(adapter), &kind),
                    color: adapter_color(Some(adapter), &kind),
                    launch_domain: adapter.launch_domain.clone(),
                    argv,
                });
            }
        }

        let result = Arc::new(entries);
        *self.launcher_cache.borrow_mut() = Some((gen, Arc::clone(&result)));
        result
    }

    /// Pre-registered SSH connections offered by the sidebar SSH quick-launch
    /// dropdown.
    ///
    /// `WezTerm`/`Ssh` transports spawn through `SpawnTabDomain::DomainName`,
    /// so their `argv` stays empty. `Mosh`/`Et` bypass the mux and run as a
    /// plain shell command; their `argv` is the fully resolved program path
    /// plus `user@host` (plus port for Et) plus the domain's `extra_args`.
    /// Entries whose declared `Mosh`/`Et` sidecar binary is not on `PATH` (and
    /// the fallback dirs below) are dropped, so the dropdown never offers a
    /// row that would fail to spawn.
    ///
    /// Building this probes `$PATH`, so the result is cached per config
    /// generation and must never be rebuilt per frame — same pattern as
    /// `agent_launcher_entries`.
    pub fn ssh_quick_launch_entries(&self) -> Arc<Vec<SshQuickLaunchEntry>> {
        let gen = self.config.generation();
        {
            let cached = self.ssh_launcher_cache.borrow();
            if let Some((cached_gen, ref entries)) = *cached {
                if cached_gen == gen {
                    return Arc::clone(entries);
                }
            }
        }

        let mut entries = Vec::new();
        for domain in self.config.ssh_domains() {
            let transport = domain.transport;
            let argv = match transport {
                config::SshTransport::WezTerm | config::SshTransport::Ssh => Vec::new(),
                config::SshTransport::Mosh | config::SshTransport::Et => {
                    let Some(binary) = transport.binary_name() else {
                        continue;
                    };
                    let Some(resolved) = resolve_command_path(binary) else {
                        continue;
                    };
                    let mut argv = Vec::with_capacity(2 + domain.extra_args.len());
                    argv.push(resolved.to_string_lossy().into_owned());
                    if let Some(user) = domain.username.as_deref() {
                        argv.push(format!("{user}@{}", domain.remote_address));
                    } else {
                        argv.push(domain.remote_address.clone());
                    }
                    argv.extend(domain.extra_args.iter().cloned());
                    argv
                }
                config::SshTransport::Custom => {
                    // The user-supplied argv is the source of truth: no
                    // host/user synthesis, no port flag. An empty command is
                    // a config error; skip it quietly rather than spawn an
                    // empty shell.
                    if domain.custom_command.is_empty() {
                        continue;
                    }
                    let Some(resolved) = resolve_command_path(&domain.custom_command[0]) else {
                        continue;
                    };
                    let mut argv =
                        Vec::with_capacity(domain.custom_command.len() + domain.extra_args.len());
                    argv.push(resolved.to_string_lossy().into_owned());
                    argv.extend(domain.custom_command.iter().skip(1).cloned());
                    argv.extend(domain.extra_args.iter().cloned());
                    argv
                }
            };

            // Display label: the bare user@host for mosh/et (the dropdown badge
            // already says which transport), otherwise the domain name with
            // the conventional SSH:/SSHMUX: prefix stripped. Custom keeps the
            // domain name verbatim — there is no host synthesis to lean on.
            let label = match transport {
                config::SshTransport::WezTerm | config::SshTransport::Ssh => domain
                    .name
                    .strip_prefix("SSH:")
                    .or_else(|| domain.name.strip_prefix("SSHMUX:"))
                    .unwrap_or(&domain.name)
                    .to_string(),
                config::SshTransport::Mosh | config::SshTransport::Et => {
                    if let Some(user) = domain.username.as_deref() {
                        format!("{user}@{}", domain.remote_address)
                    } else {
                        domain.remote_address.clone()
                    }
                }
                config::SshTransport::Custom => domain.name.clone(),
            };

            entries.push(SshQuickLaunchEntry {
                domain_name: domain.name.clone(),
                label,
                transport,
                argv,
            });
        }

        let result = Arc::new(entries);
        *self.ssh_launcher_cache.borrow_mut() = Some((gen, Arc::clone(&result)));
        result
    }

    /// Shells and domains offered by the new-tab dropdown.
    ///
    /// Probing for shells walks `$PATH` and stats files, so like
    /// `agent_launcher_entries` this is cached per config generation. It is
    /// only called while the menu is open, so a closed menu costs nothing.
    pub fn new_tab_menu_entries(&self) -> Arc<Vec<NewTabMenuEntry>> {
        let gen = self.config.generation();
        {
            let cached = self.new_tab_menu_cache.borrow();
            if let Some((cached_gen, ref entries)) = *cached {
                if cached_gen == gen {
                    return Arc::clone(entries);
                }
            }
        }

        let mut entries: Vec<NewTabMenuEntry> = Vec::new();
        let menu = &self.config.new_tab_menu;

        if menu.enabled {
            if menu.show_shells {
                for (label, argv) in discovered_shells() {
                    entries.push(NewTabMenuEntry {
                        label,
                        target: NewTabTarget::Program(argv),
                        group: 0,
                    });
                }
            }

            if menu.show_domains {
                for domain in Mux::get().iter_domains() {
                    if !domain.spawnable() {
                        continue;
                    }
                    let name = domain.domain_name().to_string();
                    // `domain_label()` is async and paint cannot await, so the
                    // name is the label. Strip the WSL: prefix so a distro
                    // reads like the shell entries beside it.
                    let label = name
                        .strip_prefix("WSL:")
                        .map(str::to_string)
                        .unwrap_or_else(|| name.clone());
                    entries.push(NewTabMenuEntry {
                        label,
                        target: NewTabTarget::Domain(name),
                        group: 1,
                    });
                }
            }

            if menu.show_launch_menu {
                for item in &self.config.launch_menu {
                    let argv = match item.args.as_ref() {
                        Some(args) if !args.is_empty() => args.clone(),
                        // A launch_menu entry with no args means "the default
                        // shell", which the plain + button already covers.
                        _ => continue,
                    };
                    entries.push(NewTabMenuEntry {
                        label: item.label.clone().unwrap_or_else(|| argv.join(" ")),
                        target: NewTabTarget::Program(argv),
                        group: 2,
                    });
                }
            }
        }

        let result = Arc::new(entries);
        *self.new_tab_menu_cache.borrow_mut() = Some((gen, Arc::clone(&result)));
        result
    }

    /// Open a new tab from a dropdown row, carrying the current directory
    /// across a domain change where that is meaningful.
    pub fn spawn_new_tab_menu_entry(&mut self, index: usize) {
        let Some(entry) = self.new_tab_menu_entries().get(index).cloned() else {
            return;
        };
        let (domain, args) = match entry.target {
            NewTabTarget::Domain(name) => (SpawnTabDomain::DomainName(name), None),
            NewTabTarget::Program(argv) => (SpawnTabDomain::CurrentPaneDomain, Some(argv)),
        };
        let cwd = crate::termwindow::composer::active_pane_cwd(self)
            .and_then(|cwd| self.translate_cwd_for_domain(PathBuf::from(cwd), &domain));
        self.spawn_command(
            &SpawnCommand {
                label: Some(entry.label),
                args,
                cwd,
                domain,
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
    }

    /// Spawn a sidebar SSH quick-launch entry into a new tab.
    ///
    /// `WezTerm`/`Ssh` transports route through the registered mux domain
    /// (`SpawnTabDomain::DomainName`); `Mosh`/`Et` bypass the mux and run as a
    /// plain shell command in the local domain. The cwd translation that the
    /// new-tab menu applies is skipped for mosh/et: the remote path is opaque
    /// to the local mux and the transport owns its own working directory.
    pub fn spawn_ssh_quick_launch_entry(&mut self, domain_name: &str) {
        let Some(entry) = self
            .ssh_quick_launch_entries()
            .iter()
            .find(|e| e.domain_name == domain_name)
            .cloned()
        else {
            return;
        };
        let (domain, args, cwd) = match entry.transport {
            config::SshTransport::WezTerm | config::SshTransport::Ssh => {
                let domain = SpawnTabDomain::DomainName(entry.domain_name.clone());
                let cwd = crate::termwindow::composer::active_pane_cwd(self)
                    .and_then(|cwd| self.translate_cwd_for_domain(PathBuf::from(cwd), &domain));
                (domain, None, cwd)
            }
            // Local-domain shell command; no cwd translation. Custom lands
            // here too: the argv is opaque to the mux, so it runs in `local`
            // exactly like mosh/et.
            config::SshTransport::Mosh
            | config::SshTransport::Et
            | config::SshTransport::Custom => (
                SpawnTabDomain::DomainName("local".to_string()),
                Some(entry.argv),
                None,
            ),
        };
        self.spawn_command(
            &SpawnCommand {
                label: Some(entry.label),
                args,
                cwd,
                domain,
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
    }

    /// The agent launched by a plain click: the configured `default_adapter`
    /// when it is installed, otherwise the first installed agent.
    pub fn agent_launcher_default(&self) -> Option<AgentLauncherEntry> {
        let entries = self.agent_launcher_entries();
        let configured = self.config.agent_ui.launcher.default_adapter.as_deref();
        if let Some(id) = configured.map(str::trim).filter(|id| !id.is_empty()) {
            if let Some(entry) = entries.iter().find(|entry| entry.adapter_id == id) {
                return Some(entry.clone());
            }
        }
        entries.first().cloned()
    }

    /// Domain name of the active pane, if it can be resolved.
    fn active_pane_domain_name(&self) -> Option<String> {
        let pane = self.get_active_pane_or_overlay()?;
        Mux::get()
            .get_domain(pane.domain_id())
            .map(|domain| domain.domain_name().to_string())
    }

    fn domain_is_registered(&self, name: &str) -> bool {
        Mux::get().get_domain_by_name(name).is_some()
    }

    /// First registered WSL domain, used by `launcher.prefer_wsl`.
    ///
    /// `WslDistro::is_default` is parsed by the config crate but dropped by
    /// `WslDomain::default_domains()`, so "the default distro" is not
    /// available here without editing an upstream struct. Registration order
    /// follows `wsl.exe -l -v` output; pin a specific distro with
    /// `agent_ui.launcher.domain` if that order is not what you want.
    fn first_wsl_domain_name(&self) -> Option<String> {
        Mux::get()
            .iter_domains()
            .iter()
            .filter(|domain| domain.spawnable())
            .map(|domain| domain.domain_name().to_string())
            .find(|name| wsl_paths::is_wsl_domain(name, &self.config))
    }

    /// Domain a launched agent should spawn into.
    ///
    /// Precedence: the adapter's own `launch_domain`, then
    /// `agent_ui.launcher.domain`, then `prefer_wsl`, then the active pane's
    /// domain. A configured name that is not registered warns and falls
    /// through rather than failing the spawn — `resolve_spawn_tab_domain`
    /// returns an error for unknown names, which would surface as a silent
    /// no-op on click.
    /// True when the launch must be pulled back to this machine: the user asked
    /// for `ForceLocal` and the active pane is a session somewhere else.
    ///
    /// The agent CLI and its credentials live on the workstation, so following
    /// an SSH pane onto the remote host almost always fails or starts an
    /// unrelated install. Probing costs a foreground-process fetch, so this is
    /// evaluated once per launch and threaded into the domain and cwd choices
    /// rather than being re-asked by each.
    fn agent_launch_forced_local(&self) -> bool {
        if self.config.agent_ui.launcher.remote_behavior != AgentRemoteBehavior::ForceLocal {
            return false;
        }
        // Only probe the pane once the policy says the answer could matter;
        // `pane_looks_remote` costs a foreground-process fetch.
        let pane_looks_remote = self
            .get_active_pane_or_overlay()
            .is_some_and(|pane| self.pane_looks_remote(&pane));
        agent_launch_forced_local(
            self.config.agent_ui.launcher.remote_behavior,
            pane_looks_remote,
        )
    }

    fn agent_launch_domain(
        &self,
        entry: &AgentLauncherEntry,
        forced_local: bool,
    ) -> SpawnTabDomain {
        let launcher = &self.config.agent_ui.launcher;
        for configured in [entry.launch_domain.as_deref(), launcher.domain.as_deref()]
            .into_iter()
            .flatten()
        {
            let name = configured.trim();
            if name.is_empty() {
                continue;
            }
            if self.domain_is_registered(name) {
                return SpawnTabDomain::DomainName(name.to_string());
            }
            log::warn!(
                "agent launcher: domain {:?} is not registered; \
                 falling back to the active pane's domain",
                name
            );
        }

        if launcher.prefer_wsl {
            // Already inside a distro: stay there rather than hopping to
            // whichever distro happens to be registered first.
            let already_wsl = self
                .active_pane_domain_name()
                .map(|name| wsl_paths::is_wsl_domain(&name, &self.config))
                .unwrap_or(false);
            if !already_wsl {
                if let Some(name) = self.first_wsl_domain_name() {
                    return SpawnTabDomain::DomainName(name);
                }
            }
        }

        // Checked last so an explicitly configured domain, and the WSL
        // preference on Windows, still win — both already name a local target.
        if forced_local {
            return SpawnTabDomain::DomainName("local".to_string());
        }

        SpawnTabDomain::CurrentPaneDomain
    }

    /// Rewrite `cwd` so it means the same directory inside `target`.
    ///
    /// `None` drops the cwd from the spawn, letting the target domain use its
    /// own default — the right answer whenever the path cannot be expressed in
    /// the target's filesystem.
    pub fn translate_cwd_for_domain(
        &self,
        cwd: PathBuf,
        target: &SpawnTabDomain,
    ) -> Option<PathBuf> {
        let source = self.active_pane_domain_name();
        let target_name = match target {
            SpawnTabDomain::DomainName(name) => Some(name.clone()),
            // CurrentPaneDomain and the id/default forms all resolve to
            // something we cannot name cheaply; treat them as "same domain".
            _ => source.clone(),
        };
        let (Some(source), Some(target_name)) = (source, target_name) else {
            return Some(cwd);
        };
        if source == target_name {
            return Some(cwd);
        }

        let source_distro = wsl_paths::distro_for_domain(&source, &self.config);
        let target_distro = wsl_paths::distro_for_domain(&target_name, &self.config);
        match (source_distro, target_distro) {
            // Host -> WSL needs no work: fixup_command hands the path to
            // `wsl.exe --cd`, which accepts Windows paths and translates them.
            (None, Some(_)) => Some(cwd),
            (Some(distro), None) => wsl_paths::wsl_to_windows(&cwd.to_string_lossy(), &distro),
            (Some(source_distro), Some(target_distro)) if source_distro == target_distro => {
                Some(cwd)
            }
            // Distro A's paths generally do not exist in distro B.
            (Some(_), Some(_)) => None,
            // Two different non-WSL domains, e.g. local -> SSH. A local path
            // is meaningless on the remote, so don't send one.
            (None, None) => None,
        }
    }

    /// Apply the project-root walk, staying in the right filesystem view.
    fn project_root_for(&self, cwd: &str, source_distro: Option<&str>) -> PathBuf {
        let markers = &self.config.agent_ui.launcher.project_markers;
        match source_distro {
            // The pane lives inside a distro, so the marker walk has to stat
            // the distro's filesystem. From the Windows side that is only
            // reachable through the UNC share, so walk there and map back.
            Some(distro) => wsl_paths::wsl_to_windows(cwd, distro)
                .and_then(|probe| nearest_project_root(&probe, markers))
                .and_then(|root| wsl_paths::windows_to_wsl(&root.to_string_lossy(), distro))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(cwd)),
            None => {
                let cwd = PathBuf::from(cwd);
                nearest_project_root(&cwd, markers).unwrap_or(cwd)
            }
        }
    }

    /// Directory a launched agent should start in, honoring the sticky
    /// project-root toggle and the target domain. `None` means "let the domain
    /// decide".
    fn agent_launch_cwd(&self, target: &SpawnTabDomain, forced_local: bool) -> Option<PathBuf> {
        if forced_local {
            // The active pane's directory names a path on the remote host and
            // means nothing here, so borrow the newest local pane's directory
            // instead. With no local pane to borrow from, fall back to the home
            // directory rather than letting the domain inherit a remote path.
            let raw = self
                .newest_local_pane_cwd()
                .unwrap_or_else(|| config::HOME_DIR.clone())
                .to_string_lossy()
                .into_owned();
            let cwd = if self.agent_launcher_project_root {
                self.project_root_for(&raw, None)
            } else {
                PathBuf::from(raw)
            };
            return Some(cwd);
        }

        let raw = crate::termwindow::composer::active_pane_cwd(self)?;
        let source_distro = self
            .active_pane_domain_name()
            .and_then(|name| wsl_paths::distro_for_domain(&name, &self.config));

        let cwd = if self.agent_launcher_project_root {
            // Falls back to the pane directory when the pane is not inside a
            // project; refusing to launch would be worse than launching here.
            self.project_root_for(&raw, source_distro.as_deref())
        } else {
            PathBuf::from(raw)
        };

        self.translate_cwd_for_domain(cwd, target)
    }

    /// Start a fresh agent session, placed per `agent_ui.launcher` config
    /// (a new tab, a split, or a split that gets zoomed), the Alt-click
    /// inversion, and — for the second and later agent in a tab — the tile
    /// policy that spreads repeat launches across an even-ish grid instead
    /// of nondeterministically halving whatever pane a previous launch made
    /// active.
    ///
    /// `override_target` is the explicit target chosen from the launcher's
    /// submenu (Split pane / Fullscreen / New tab); it wins over both the
    /// configured `open_in` and the Alt-click inversion. Pass `None` for the
    /// plain-click/Alt-click path.
    ///
    /// The argv comes only from config (built-in defaults or user Lua), never
    /// from pane text, and the action is always user-initiated — so unlike
    /// Resume/Attach this is deliberately not gated by
    /// `agent_ui.enable_control_actions`.
    pub fn launch_agent(
        &mut self,
        entry: &AgentLauncherEntry,
        override_target: Option<AgentLaunchTarget>,
        invert_target: bool,
    ) {
        if entry.argv.is_empty() {
            return;
        }
        let forced_local = self.agent_launch_forced_local();
        // Passing cwd explicitly is required: `Mux::resolve_cwd` would
        // otherwise inherit the pane directory, ignore project-root mode, and
        // drop the cwd entirely whenever the target domain differs.
        let domain = self.agent_launch_domain(entry, forced_local);
        let cwd = self.agent_launch_cwd(&domain, forced_local);
        let placement = self.agent_launch_placement(invert_target, override_target);
        self.spawn_agent(
            SpawnCommand {
                label: Some(format!("{} agent", entry.label)),
                args: Some(entry.argv.clone()),
                cwd,
                domain,
                ..Default::default()
            },
            placement,
        );
    }

    /// Resolve `agent_ui.launcher.open_in` (plus the Alt-click inversion or
    /// an explicit submenu override) into a concrete `AgentPlacement`,
    /// using the active tab's current panes for tiling geometry.
    fn agent_launch_placement(
        &self,
        invert_target: bool,
        override_target: Option<AgentLaunchTarget>,
    ) -> agent_launch::AgentPlacement {
        let launcher = &self.config.agent_ui.launcher;
        let target =
            agent_launch::resolve_launch_target(launcher.open_in, invert_target, override_target);

        let mux = Mux::get();
        let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) else {
            return agent_launch::AgentPlacement::NewTab;
        };
        let Some(active_pane) = tab.get_active_pane() else {
            return agent_launch::AgentPlacement::NewTab;
        };

        // A pane half narrower than this or shorter than this is not worth
        // tiling into; the launch falls back to a new tab instead.
        const MIN_TILE_COLUMNS: usize = 40;
        const MIN_TILE_ROWS: usize = 12;
        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;

        let eligible_panes: Vec<agent_launch::PaneGeom> = tab
            .iter_panes_ignoring_zoom()
            .into_iter()
            // Neither utility pane is a place to put an agent: the worktree
            // picker is already a narrow column, and splitting the insight pane
            // would hide half of the very list you launched from.
            .filter(|pos| !is_worktree_pane(&pos.pane))
            .map(|pos| agent_launch::PaneGeom {
                pane_id: pos.pane.pane_id(),
                index: pos.index,
                pixel_width: pos.pixel_width,
                pixel_height: pos.pixel_height,
            })
            .collect();

        agent_launch::agent_placement(
            target,
            launcher.tile,
            match launcher.split_direction {
                AgentSplitDirection::Horizontal => SplitDirection::Horizontal,
                AgentSplitDirection::Vertical => SplitDirection::Vertical,
            },
            launcher.split_size_percent,
            launcher.max_panes_per_tab,
            active_pane.pane_id(),
            &eligible_panes,
            cell_width * MIN_TILE_COLUMNS,
            cell_height * MIN_TILE_ROWS,
        )
    }

    /// How long a session scan stays fresh.
    ///
    /// Long enough that reopening the submenu to pick a different row is free,
    /// short enough that a session finished in another window shows up without
    /// restarting the terminal.
    const SESSION_SCAN_TTL: Duration = Duration::from_secs(10);

    /// Start a background scan for resumable sessions unless one is already in
    /// flight or the cached answer is still fresh.
    ///
    /// Statting every transcript and head-reading the newest few is filesystem
    /// work, so it happens on a worker thread and is applied back on the GUI
    /// thread. Called from the click that opens the submenu, never from paint.
    pub fn kick_agent_session_scan(&mut self) {
        if self.agent_session_scan_pending {
            return;
        }
        let fresh = self
            .agent_session_cache
            .as_ref()
            .is_some_and(|(scanned_at, _)| scanned_at.elapsed() < Self::SESSION_SCAN_TTL);
        if fresh {
            return;
        }
        let limit = self.agent_resume_menu_limit();
        if limit == 0 {
            return;
        }
        let Some(home) = dirs_next::home_dir() else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };

        self.agent_session_scan_pending = true;
        let future = promise::spawn::spawn_into_new_thread(move || {
            let sessions = crate::agent_herd::sessions::collect_recent_sessions(&home, limit);
            // The scan thread must not touch the mux or any GUI state, so the
            // result is applied back on the GUI thread.
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                term_window.agent_session_scan_pending = false;
                term_window.agent_session_cache = Some((Instant::now(), Arc::new(sessions)));
            })));
            Ok::<(), anyhow::Error>(())
        });
        promise::spawn::spawn(async move {
            if let Err(err) = future.await {
                log::error!("agent session scan failed: {err:#}");
            }
        })
        .detach();
    }

    /// Resume the session at `index` of the last scan.
    ///
    /// The session's own directory is used rather than the active pane's, and
    /// the project-root toggle deliberately does not apply: a resumed session
    /// has to come back up where it left off or its relative paths break.
    ///
    /// Like `launch_agent` and unlike the toolbelt's Resume button, this is not
    /// gated by `agent_ui.enable_control_actions`. That gate exists because the
    /// toolbelt takes its session id from pane text, which an agent's own output
    /// can forge. Here the argv comes from config, the id comes from a
    /// filesystem enumeration under the user's own agent state directories and
    /// is charset-checked before it can reach argv, and the user picked the row.
    pub fn resume_agent_session(&mut self, index: usize, target: Option<AgentLaunchTarget>) {
        let Some(session) = self
            .agent_session_cache
            .as_ref()
            .and_then(|(_, sessions)| sessions.get(index))
            .cloned()
        else {
            // The scan was replaced between paint and click; nothing to do.
            return;
        };
        let Some(adapter) = self.agent_adapter_config_by_id(Some(&session.adapter_id)) else {
            return;
        };
        let label = adapter_label(&adapter, &session.adapter_id);
        let values = AgentActionTemplateValues {
            session_id: Some(session.session_id.clone()),
            cwd: Some(session.cwd.clone()),
            home: dirs_next::home_dir(),
            attach_url: None,
        };
        let Some(argv) = resolve_agent_resume_command(&adapter, &values) else {
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Agent resume".to_string(),
                message: format!("{label} has no resume command, or it is not on PATH"),
                url: None,
                timeout: Some(Duration::from_millis(2600)),
            });
            return;
        };

        let forced_local = self.agent_launch_forced_local();
        let domain = match self
            .agent_launcher_entries()
            .iter()
            .find(|entry| entry.adapter_id == session.adapter_id)
        {
            Some(entry) => self.agent_launch_domain(entry, forced_local),
            // The agent is resumable but not installed as a launcher entry;
            // fall back to the active pane's domain rather than refusing.
            None => SpawnTabDomain::CurrentPaneDomain,
        };
        let cwd = self.translate_cwd_for_domain(session.cwd.clone(), &domain);
        let placement = self.agent_launch_placement(false, target);
        self.spawn_agent(
            SpawnCommand {
                label: Some(format!("{label} resume")),
                args: Some(argv),
                cwd,
                domain,
                ..Default::default()
            },
            placement,
        );
    }

    /// Launch the agent with the given adapter id, if it is still installed.
    pub fn launch_agent_by_id(
        &mut self,
        adapter_id: &str,
        target: Option<AgentLaunchTarget>,
        invert_target: bool,
    ) {
        let entry = self
            .agent_launcher_entries()
            .iter()
            .find(|entry| entry.adapter_id == adapter_id)
            .cloned();
        if let Some(entry) = entry {
            self.launch_agent(&entry, target, invert_target);
        }
    }

    /// Flip and persist the sticky project-root launch preference.
    pub fn toggle_agent_launcher_project_root(&mut self) {
        self.agent_launcher_project_root = !self.agent_launcher_project_root;
        crate::termwindow::tgz_ui_state::save_agent_launcher_project_root(
            self.agent_launcher_project_root,
        );
    }

    pub fn agent_launcher_project_root_enabled(&self) -> bool {
        self.agent_launcher_project_root
    }

    /// Adapters whose `process_names` match the pane's foreground process.
    ///
    /// Separated from the title path, which it previously shared: bundling both
    /// into one Option let a weak title hit on an earlier adapter mask a correct
    /// process match on a later one.
    fn process_agent_candidate(&self, process: Option<&str>) -> Vec<AgentIdentityCandidate> {
        let Some(process) = process.map(|process| basename(process).to_ascii_lowercase()) else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for (id, adapter) in self.merged_agent_adapters().iter().cloned() {
            if !adapter.enabled {
                continue;
            }
            if adapter
                .process_names
                .iter()
                .any(|name| basename(name).eq_ignore_ascii_case(&process))
            {
                candidates.push(AgentIdentityCandidate {
                    kind: adapter_kind_from_id(&id, &adapter),
                    adapter_id: Some(id),
                    evidence: AgentEvidence::Process,
                    signals: 1,
                });
            }
        }
        candidates
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
        // The insight pane prints adapter names, status words and agent chrome
        // by definition, so visible-evidence detection would badge it as an
        // agent and then list it as one of the agents it is listing.
        // The insight pane no longer exists as a separate mux pane.

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
        let merged_adapters = self.merged_agent_adapters();
        let mut candidates: Vec<AgentIdentityCandidate> = Vec::new();
        if let Some(kind) = user_var(&vars, "agent.kind") {
            let resolved = AgentKind::from_hint_with_adapters(kind, &merged_adapters)
                .unwrap_or_else(|| AgentKind::from_user_var(kind));
            let adapter_id = explicit_adapter_id
                .clone()
                .or_else(|| resolved.config_key().map(ToString::to_string));
            candidates.push(AgentIdentityCandidate {
                signals: if explicit_adapter_id.is_some() { 2 } else { 1 },
                adapter_id,
                kind: resolved,
                evidence: AgentEvidence::UserVar,
            });
        }
        if self.config.agent_ui.detect_processes {
            candidates.extend(self.process_agent_candidate(foreground_process.as_deref()));
            candidates.extend(title_agent_candidates(
                &pane_title,
                merged_adapters.iter().cloned(),
            ));
        }
        if has_agent_metadata_evidence(&vars) {
            candidates.push(AgentIdentityCandidate {
                adapter_id: None,
                kind: AgentKind::Unknown("Agent".to_string()),
                evidence: AgentEvidence::Metadata,
                signals: 1,
            });
        }
        let explicit_status = user_var(&vars, "agent.status");
        let strongest = candidates.iter().map(|c| c.evidence).min();
        if should_load_visible_agent_text(
            strongest,
            self.config.agent_ui.detect_processes,
            explicit_status,
        ) {
            visible_text = self.visible_agent_text(pane);
            visible_text_loaded = true;
            let ambiguous = ambiguous_agent_patterns(&merged_adapters);
            candidates.extend(visible_agent_candidates(
                &visible_text,
                &merged_adapters,
                &ambiguous,
                self.config.agent_ui.visible_identity_signals as u32,
            ));
        }
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
        let trust_visible = self.config.agent_ui.trust_visible_evidence;
        let mut fresh_identity = resolve_agent_identity(candidates);
        // Sticky detection: a pane previously detected as an agent should not
        // vanish because the identifying banner scrolled out of the visible
        // region, nor because the agent rewrote the pane title to name its
        // current task. Reusing only the *identity* (not the whole cached
        // state) keeps status, model and telemetry live.
        let sticky_identity = sticky_agent_identity(
            previous_entry.as_ref(),
            &cache_key,
            Instant::now(),
            trust_visible,
        );

        // Identity hysteresis: an established badge only changes on stronger
        // evidence, or on two consecutive detections agreeing. Otherwise a pane
        // that retitles itself every turn re-rolls its identity from whatever
        // brand token the new title happens to contain.
        let established = previous_entry
            .as_ref()
            .and_then(|entry| entry.state.as_ref())
            .and_then(|state| state.adapter_id.as_deref().map(|id| (id, state.evidence)));
        let mut pending_switch = previous_entry
            .as_ref()
            .and_then(|entry| entry.pending_switch.clone());
        if let Some(candidate) = fresh_identity.clone() {
            let candidate_id = candidate.adapter_id.as_deref();
            let disagrees = established
                .map(|(id, _)| Some(id) != candidate_id)
                .unwrap_or(false);
            let agreements = match (&mut pending_switch, candidate_id) {
                (Some((pending_id, count)), Some(id)) if pending_id == id => {
                    *count += 1;
                    *count
                }
                (_, Some(id)) if disagrees => {
                    pending_switch = Some((id.to_string(), 1));
                    1
                }
                _ => {
                    pending_switch = None;
                    1
                }
            };
            if !agent_identity_switch_allowed(
                established,
                candidate_id,
                candidate.evidence,
                agreements,
            ) {
                // Hold the established identity for this frame; the counter
                // above decides whether the next one flips it. Substituted
                // explicitly rather than left to the sticky path, which would
                // return nothing for a title-derived identity whose title just
                // changed — blinking the badge off is worse than flipping it.
                fresh_identity = previous_entry
                    .as_ref()
                    .and_then(|entry| entry.state.as_ref())
                    .map(|state| AgentIdentityCandidate {
                        adapter_id: state.adapter_id.clone(),
                        kind: state.kind.clone(),
                        evidence: state.evidence,
                        signals: 1,
                    });
            } else if !disagrees {
                pending_switch = None;
            }
        }

        let evidence = fresh_identity
            .as_ref()
            .map(|candidate| candidate.evidence)
            .or(sticky_identity.as_ref().map(|sticky| sticky.evidence));
        // `agent.enable_control_actions` is the per-pane *opt-in*, checked in
        // `agent_control_actions_allowed`; it is deliberately not evidence, so a
        // pane cannot vouch for its own identity by asking for permission.
        let evidence_trusted = evidence.is_some_and(|evidence| evidence.is_trusted(trust_visible));
        let trusted_controls = evidence_trusted
            || sticky_identity
                .as_ref()
                .is_some_and(|sticky| sticky.trusted_controls);
        let control_actions_allowed = agent_control_actions_allowed(
            self.config.agent_ui.enable_control_actions,
            trusted_controls,
            &vars,
        );
        let evidence = evidence.unwrap_or(AgentEvidence::Metadata);

        let Some((adapter_id, kind)) = fresh_identity
            .map(|candidate| (candidate.adapter_id, candidate.kind))
            .or_else(|| sticky_identity.map(|sticky| (sticky.adapter_id, sticky.kind)))
        else {
            self.agent_detection_cache.borrow_mut().insert(
                pane.pane_id(),
                AgentDetectionCacheEntry {
                    key: cache_key,
                    state: None,
                    last_wait_notification: previous_wait_notification,
                    detected_at: Instant::now(),
                    status_at: Instant::now(),
                    pending_switch: None,
                },
            );
            return None;
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
                    status_at: Instant::now(),
                    pending_switch: None,
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
                    status_at: Instant::now(),
                    pending_switch: None,
                },
            );
            return None;
        }

        let cwd = pane_working_dir(pane);
        let previous_state = previous_entry
            .as_ref()
            .and_then(|entry| entry.state.as_ref());
        let fresh_status = if explicit_status.is_some() {
            AgentStatus::from_hint(explicit_status)
        } else if visible_text_loaded {
            infer_agent_status_from_visible_text(&visible_text, adapter.as_ref())
        } else {
            AgentStatus::Unknown
        };
        let now = Instant::now();
        let status = stabilize_agent_status(
            fresh_status.clone(),
            previous_state.map(|state| state.status.clone()),
            previous_entry.as_ref().map(|entry| entry.status_at),
            now,
            AGENT_RUNNING_GRACE,
        );
        // Only a *fresh* observation refreshes the grace window; carrying the
        // old timestamp forward is what makes the grace expire.
        let status_at = if fresh_status == AgentStatus::Unknown {
            previous_entry
                .as_ref()
                .map(|entry| entry.status_at)
                .unwrap_or(now)
        } else {
            now
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
            evidence,
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
                status_at,
                pending_switch,
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

    /// Every row the expanded sidebar list will draw, in order.
    ///
    /// One row per tab, plus an indented child row per pane for tabs that are
    /// split and expanded. This is the single source of truth for the list:
    /// the paint pass, the wheel-scroll clamp and the scrollbar all measure
    /// from it, so none of them can disagree about how long the list is.
    pub(crate) fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let query = self
            .sidebar_search
            .as_ref()
            .map(|state| state.query.as_str());
        let tabs: Vec<SidebarTabInput> = self
            .tab_bar
            .items()
            .iter()
            .filter_map(|entry| {
                let TabBarItem::Tab { tab_idx, active } = entry.item else {
                    return None;
                };
                let (title, metadata) = self.sidebar_tab_labels(tab_idx, &entry.title);
                Some(SidebarTabInput {
                    tab_idx,
                    active,
                    title,
                    metadata,
                    panes: self.sidebar_pane_rows_for_tab_idx(tab_idx),
                })
            })
            .collect();

        assemble_sidebar_rows(tabs, &self.sidebar_expanded_tabs, query)
    }

    /// Number of rows the list scrolls over.
    ///
    /// The collapsed rail draws icons per tab and has no pane children, so it
    /// counts tabs; the expanded list counts every row.
    fn sidebar_row_count(&self) -> usize {
        let collapsed = self.config.sidebar_auto_hide && !self.sidebar_auto_hide_open;
        let rows = self.sidebar_rows();
        if collapsed {
            rows.iter()
                .filter(|row| matches!(row, SidebarRow::Tab { .. }))
                .count()
        } else {
            rows.len()
        }
    }

    fn sidebar_query_matches(&self, title: &str, metadata: &[String], query: &str) -> bool {
        query_matches(title, metadata, query)
    }

    /// Rows reserved below the tab list for the bottom buttons.
    ///
    /// Expanded: "+ New Tab" plus the shared Worktree/agent row. Collapsed:
    /// the "+" rail icon plus the agent launcher slot when an agent is
    /// installed. Kept in one place so the paint pass, the wheel-scroll
    /// clamp and the scrollbar cannot disagree.
    fn sidebar_bottom_button_rows(&self) -> f32 {
        let collapsed = self.config.sidebar_auto_hide && !self.sidebar_auto_hide_open;
        let ssh_present = !self.ssh_quick_launch_entries().is_empty();
        if collapsed {
            let mut rows = 1.;
            if self.agent_launcher_default().is_some() {
                rows += 1.;
            }
            if ssh_present {
                rows += 1.;
            }
            rows
        } else if self.sidebar_width() > 180 {
            // worktree/agent shared row + new tab, plus a dedicated SSH row when
            // the SSH quick-launch has any usable entries.
            if ssh_present {
                3.
            } else {
                2.
            }
        } else {
            // Narrow expanded: only the new-tab row, plus an SSH row when present.
            if ssh_present {
                2.
            } else {
                1.
            }
        }
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
        let bottom_button_rows = self.sidebar_bottom_button_rows();
        let new_tab_y = top + height - INSET - row_height;
        let list_height =
            (new_tab_y - GAP - (bottom_button_rows - 1.) * (row_height + GAP) - list_top).max(0.);
        ((list_height + GAP) / (row_height + GAP)).floor() as usize
    }

    pub(crate) fn scroll_sidebar_tabs(&mut self, wheel_delta: isize) -> bool {
        let total = self.sidebar_row_count();
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

        let total = self.sidebar_row_count();
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
        let bottom_button_rows = self.sidebar_bottom_button_rows();
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
            .filter(|pane| !self.is_sidebar_utility_pane(pane))
            .cloned()
            .or_else(|| {
                panes
                    .iter()
                    .find(|pos| !self.is_sidebar_utility_pane(&pos.pane))
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

    /// Panes of `tab_idx` as sidebar child rows, in split order.
    ///
    /// Unlike `sidebar_primary_pane_for_tab_idx`, worktree panes are kept: a
    /// tab row summarizes one pane and should ignore the transient file
    /// picker, but the pane list is the place where every pane in the tab has
    /// to be reachable — including the picker, so it can be closed by hand if
    /// it ever outlives its fzf process.
    fn sidebar_pane_rows_for_tab_idx(&self, tab_idx: usize) -> Vec<SidebarRow> {
        let Some(tab) = Mux::get()
            .get_window(self.mux_window_id)
            .and_then(|window| window.get_by_idx(tab_idx).cloned())
        else {
            return Vec::new();
        };

        tab.iter_panes_ignoring_zoom()
            .iter()
            .map(|pos| SidebarRow::Pane {
                pane_id: pos.pane.pane_id(),
                active: pos.is_active,
                label: self.sidebar_pane_label(&pos.pane),
                is_remote: self.pane_looks_remote_cached(&pos.pane),
            })
            .collect()
    }

    /// A pane that describes the tab's *tooling* rather than its work.
    ///
    /// Neither one has a working directory, a branch or a command worth
    /// showing, so neither may stand in for a tab in the sidebar.
    fn is_sidebar_utility_pane(&self, pane: &Arc<dyn Pane>) -> bool {
        is_worktree_pane(pane)
    }

    /// Short human label for a pane row: the agent name when one is detected,
    /// otherwise the foreground command, otherwise the pane title.
    fn sidebar_pane_label(&self, pane: &Arc<dyn Pane>) -> String {
        if is_worktree_pane(pane) {
            return "Worktree".to_string();
        }
        if self.config.agent_ui.enabled {
            if let Some(agent) = self.detect_agent_pane(pane) {
                return agent.kind.label().to_string();
            }
        }
        if let Some(command) = pane
            .get_foreground_process_name(CachePolicy::AllowStale)
            .map(|name| basename(&name))
            .filter(|name| !name.is_empty())
        {
            return command;
        }
        let title = pane.get_title();
        if title.trim().is_empty() {
            format!("pane {}", pane.pane_id())
        } else {
            title
        }
    }

    /// Remote check for pane rows, which run every frame.
    ///
    /// `pane_looks_remote` costs a `FetchImmediate` process lookup and must
    /// never be on the paint path. The domain check alone is free and catches
    /// the case the row marker exists for: a pane belonging to a wezterm SSH
    /// domain sitting next to a local agent.
    fn pane_looks_remote_cached(&self, pane: &Arc<dyn Pane>) -> bool {
        Mux::get()
            .get_domain(pane.domain_id())
            .is_some_and(|domain| domain.downcast_ref::<mux::ssh::RemoteSshDomain>().is_some())
    }

    fn sidebar_agent_for_tab_idx(&self, tab_idx: usize) -> Option<AgentPaneState> {
        if !self.config.agent_ui.enabled || !self.config.agent_ui.show_sidebar_badges {
            return None;
        }
        let pane = self.sidebar_primary_pane_for_tab_idx(tab_idx)?;
        self.detect_agent_pane(&pane)
    }

    /// Phase to draw this agent's status dot at, or `None` when it must not
    /// animate.
    ///
    /// Registering a next-frame deadline is what keeps the pulse going, so the
    /// early returns are also the cost control: with nothing working, nothing is
    /// scheduled and the window falls back to event-driven repaints. Unfocused
    /// windows return `None` rather than freezing at whatever brightness the
    /// last frame happened to catch (the frame scheduler ignores unfocused
    /// windows anyway).
    fn agent_dot_pulse(&self, agent: &AgentPaneState) -> Option<f32> {
        if !self.config.agent_ui.pulse_working_dot || self.focused.is_none() {
            return None;
        }
        if !matches!(agent.status, AgentStatus::Running | AgentStatus::Streaming) {
            return None;
        }
        let period = Duration::from_millis(self.config.agent_ui.pulse_period_ms.clamp(400, 6000));
        self.update_next_frame_time(Some(Instant::now() + AGENT_PULSE_FRAME_INTERVAL));
        Some(agent_pulse_phase(self.created.elapsed(), period))
    }

    fn sidebar_primary_pane_for_tab_idx(&self, tab_idx: usize) -> Option<Arc<dyn Pane>> {
        let tab = Mux::get()
            .get_window(self.mux_window_id)
            .and_then(|window| window.get_by_idx(tab_idx).cloned())?;
        let active_pane = tab.get_active_pane();
        let panes = tab.iter_panes_ignoring_zoom();
        active_pane
            .as_ref()
            .filter(|pane| !self.is_sidebar_utility_pane(pane))
            .cloned()
            .or_else(|| {
                panes
                    .iter()
                    .find(|pos| !self.is_sidebar_utility_pane(&pos.pane))
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
        let kind = agent.kind.label();
        // Ordered widest -> narrowest. The render region hard-truncates to whole
        // cells with no ellipsis, so instead of letting "Claude agent opus" clip
        // to "Claude agent o", we pick the widest whole variant that fits.
        let mut label_fallbacks: Vec<String> = Vec::new();
        match &agent.model {
            Some(model) => {
                label_fallbacks.push(format!("{} agent {}{}", kind, model, status));
                label_fallbacks.push(format!("{} agent {}", kind, model));
                if !status.is_empty() {
                    label_fallbacks.push(format!("{} agent{}", kind, status));
                }
                label_fallbacks.push(format!("{} agent", kind));
            }
            None => {
                label_fallbacks.push(format!("{} agent{}", kind, status));
                label_fallbacks.push(format!("{} agent", kind));
            }
        }
        label_fallbacks.push(kind.to_string());
        let label = label_fallbacks[0].clone();

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
        trim_agent_toolbelt_buttons(&mut visible_buttons, max_button_area);
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
        let accent = agent_status_dot_accent(
            &agent.status,
            adapter_color(adapter.as_ref(), &agent.kind),
            bg,
            None,
        );
        // The dot breathes while the agent works; the rest of the strip keeps
        // the steady accent so button text and borders do not shimmer.
        let dot_accent =
            agent_status_dot_accent(&agent.status, accent, bg, self.agent_dot_pulse(&agent));

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
            dot_accent,
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
            let cols = sidebar_text_cols(pixel_width, cell_width);
            if cols == 0 {
                return Ok(());
            }
            let text = truncate_to_cols(text, cols);
            let mut attrs = CellAttributes::default();
            attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
            if bold {
                attrs.set_intensity(Intensity::Bold);
            }
            let mut line = Line::from_text(text, &attrs, 1, None);
            line.resize(cols, SEQ_ZERO);
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
        // Whole cells that fit; pick the widest fallback that fits so the model
        // name never clips mid-word (e.g. "Claude agent opus" -> "Claude agent").
        let label_cols = (label_w / cell_w_f).max(0.) as usize;
        let fitted_label = label_fallbacks
            .iter()
            .find(|candidate| candidate.chars().count() <= label_cols)
            .cloned()
            .unwrap_or_else(|| label.clone());
        if label_w >= (cell_width * 6) as f32 {
            render_text(
                self,
                layers,
                &fitted_label,
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

        // Thin border stroke so the menu stands out against overlapping rows.
        let border = lerp_rgba(bg, fg, 0.12);
        let border_w = (1. * dpi_scale).max(1.);
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y, menu_w, border_w),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y + menu_h - border_w, menu_w, border_w),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y + border_w, border_w, menu_h - border_w * 2.),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(
                menu_x + menu_w - border_w,
                menu_y + border_w,
                border_w,
                menu_h - border_w * 2.,
            ),
            border,
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
            let cols = sidebar_text_cols(pixel_width, cell_width as usize);
            if cols == 0 {
                return Ok(());
            }
            let text = truncate_to_cols(text, cols);
            let mut attrs = CellAttributes::default();
            attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
            let mut line = Line::from_text(text, &attrs, 1, None);
            line.resize(cols, SEQ_ZERO);
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

    /// Dropdown opened from the sidebar agent launch button: one row per
    /// installed agent (each expandable into a Split pane / Fullscreen / New
    /// tab submenu for that one launch), plus the sticky project-root
    /// toggle.
    pub fn paint_agent_launch_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.agent_launch_menu.clone() else {
            return Ok(());
        };
        let entries = self.agent_launcher_entries();
        // Don't bail when entries is empty: the "Agent insight" and
        // "Resume session" rows are useful even with no adapters configured,
        // so the dropdown stays open and shows them.

        const TARGET_ROWS: [(&str, AgentLaunchTarget); 3] = [
            ("Split pane", AgentLaunchTarget::SplitPane),
            ("Fullscreen", AgentLaunchTarget::Zoomed),
            ("New tab", AgentLaunchTarget::NewTab),
        ];

        let mut rows: Vec<SidebarDropdownRow> = Vec::new();
        for entry in entries.iter() {
            let expanded =
                menu.expanded.as_ref() == Some(&ExpandedMenuRow::Agent(entry.adapter_id.clone()));
            rows.push(SidebarDropdownRow {
                label: entry.label.clone(),
                dot_color: Some(entry.color),
                checkbox: None,
                divider_above: false,
                indent: false,
                trailing_chevron: true,
                item_type: UIItemType::SidebarAgentMenuItem {
                    adapter_id: entry.adapter_id.clone(),
                },
            });
            if expanded {
                for (label, target) in TARGET_ROWS {
                    rows.push(SidebarDropdownRow {
                        label: label.to_string(),
                        dot_color: None,
                        checkbox: None,
                        divider_above: false,
                        indent: true,
                        trailing_chevron: false,
                        item_type: UIItemType::SidebarAgentMenuTarget {
                            adapter_id: entry.adapter_id.clone(),
                            target,
                        },
                    });
                }
            }
        }
        rows.push(SidebarDropdownRow {
            label: "Project root".to_string(),
            dot_color: None,
            checkbox: Some(self.agent_launcher_project_root),
            divider_above: true,
            indent: false,
            trailing_chevron: false,
            item_type: UIItemType::SidebarAgentMenuProjectRootToggle,
        });
        // Below the launch actions: this one inspects rather than launches.
        rows.push(SidebarDropdownRow {
            label: "Agent insight".to_string(),
            dot_color: None,
            checkbox: None,
            divider_above: false,
            indent: false,
            trailing_chevron: false,
            item_type: UIItemType::SidebarAgentMenuHerd,
        });

        // Past sessions. Unlike the agent rows above, which launch something
        // new, these continue something that already exists.
        let resume_expanded = menu.expanded.as_ref() == Some(&ExpandedMenuRow::ResumeSessions);
        if self.agent_resume_menu_limit() > 0 {
            rows.push(SidebarDropdownRow {
                label: "Resume session".to_string(),
                dot_color: None,
                checkbox: None,
                divider_above: false,
                indent: false,
                trailing_chevron: true,
                item_type: UIItemType::SidebarAgentMenuResume,
            });
            if resume_expanded {
                rows.extend(self.agent_resume_session_rows());
            }
        }

        // The session labels are prose, not one-word commands, so the submenu
        // needs a wider panel than the launch rows do.
        let menu_w = if resume_expanded {
            AGENT_RESUME_MENU_W
        } else {
            AGENT_LAUNCH_MENU_W
        };
        self.paint_sidebar_dropdown(layers, menu.x as f32, menu.y as f32, menu_w, false, &rows)
    }

    /// How many past sessions the resume submenu may offer.
    fn agent_resume_menu_limit(&self) -> usize {
        self.config
            .agent_ui
            .launcher
            .resume_menu_sessions
            .min(MAX_RESUME_MENU_SESSIONS) as usize
    }

    /// Rows for the expanded resume submenu: the sessions, or why there are
    /// none yet.
    ///
    /// Never scans here — painting must not touch the filesystem. The scan is
    /// kicked off by the click that expands the row and lands in
    /// `agent_session_cache`, so a first open shows progress for a frame or two.
    fn agent_resume_session_rows(&self) -> Vec<SidebarDropdownRow> {
        let placeholder = |label: &str| SidebarDropdownRow {
            label: label.to_string(),
            dot_color: None,
            checkbox: None,
            divider_above: false,
            indent: true,
            trailing_chevron: false,
            // Deliberately not a resume row: there is nothing to click, and a
            // hit-testable placeholder would resume an index that does not
            // exist.
            item_type: UIItemType::SidebarAgentMenuResume,
        };

        let Some((_, sessions)) = self.agent_session_cache.as_ref() else {
            return vec![placeholder("Scanning…")];
        };
        if sessions.is_empty() {
            return vec![placeholder(if self.agent_session_scan_pending {
                "Scanning…"
            } else {
                "No past sessions"
            })];
        }

        let adapters = self.merged_agent_adapters();
        sessions
            .iter()
            .enumerate()
            .map(|(index, session)| SidebarDropdownRow {
                label: session.menu_label(),
                dot_color: adapters
                    .iter()
                    .find(|(id, _)| id == &session.adapter_id)
                    .and_then(|(_, adapter)| adapter.color.as_deref())
                    .and_then(parse_adapter_color),
                checkbox: None,
                divider_above: false,
                indent: true,
                trailing_chevron: false,
                item_type: UIItemType::SidebarAgentMenuResumeSession { index },
            })
            .collect()
    }

    /// Dropdown opened by the chevron beside the sidebar new-tab button: the
    /// shells and domains available on this machine.
    pub fn paint_new_tab_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.new_tab_menu.clone() else {
            return Ok(());
        };
        let entries = self.new_tab_menu_entries();
        if entries.is_empty() {
            self.new_tab_menu = None;
            return Ok(());
        }

        let mut previous_group = None;
        let rows: Vec<SidebarDropdownRow> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let divider_above = previous_group.is_some_and(|group| group != entry.group);
                previous_group = Some(entry.group);
                SidebarDropdownRow {
                    label: entry.label.clone(),
                    dot_color: None,
                    checkbox: None,
                    divider_above,
                    indent: false,
                    trailing_chevron: false,
                    item_type: UIItemType::SidebarNewTabMenuItem { index },
                }
            })
            .collect();

        self.paint_sidebar_dropdown(
            layers,
            menu.x as f32,
            menu.y as f32,
            AGENT_LAUNCH_MENU_W,
            false,
            &rows,
        )
    }

    /// Right-click submenu on a tab's × (close) button. Three entries:
    /// close the tabs above (sidebar) / to the left (tab bar), below / to the
    /// right, or all others. The clicked tab is always preserved.
    pub fn paint_close_tab_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.close_tab_menu.clone() else {
            return Ok(());
        };
        if !self.config.tab_close_context_menu {
            self.close_tab_menu = None;
            return Ok(());
        }

        let (above_label, below_label) = match menu.source {
            CloseTabSource::Sidebar => ("Close Tabs Above", "Close Tabs Below"),
            CloseTabSource::TabBar => ("Close Tabs to the Left", "Close Tabs to the Right"),
        };

        let rows: Vec<SidebarDropdownRow> = vec![
            SidebarDropdownRow {
                label: above_label.to_string(),
                dot_color: None,
                checkbox: None,
                divider_above: false,
                indent: false,
                trailing_chevron: false,
                item_type: UIItemType::CloseTabMenuItem {
                    source: menu.source,
                    action: CloseTabMenuAction::CloseAbove,
                },
            },
            SidebarDropdownRow {
                label: below_label.to_string(),
                dot_color: None,
                checkbox: None,
                divider_above: false,
                indent: false,
                trailing_chevron: false,
                item_type: UIItemType::CloseTabMenuItem {
                    source: menu.source,
                    action: CloseTabMenuAction::CloseBelow,
                },
            },
            SidebarDropdownRow {
                label: "Close All Other Tabs".to_string(),
                dot_color: None,
                checkbox: None,
                divider_above: false,
                indent: false,
                trailing_chevron: false,
                item_type: UIItemType::CloseTabMenuItem {
                    source: menu.source,
                    action: CloseTabMenuAction::CloseAllOther,
                },
            },
        ];

        // Tab-bar source anchors downward (× sits at the top of the window);
        // sidebar source anchors upward (× sits in the vertical list near
        // other buttons at the bottom of the sidebar column).
        let downward = matches!(menu.source, CloseTabSource::TabBar);

        // Size the panel to the longest row label rather than the fixed
        // agent-launch width: cell width and the boxy sentence labels ("Close
        // Tabs to the Right") need ~24 columns, which `AGENT_LAUNCH_MENU_W`
        // clips to ~16 and drops the final word. Work in pre-DPI logical
        // pixels to match the `paint_sidebar_dropdown` `width` parameter.
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_w_logical = self.render_metrics.cell_size.width as f32 / dpi_scale;
        let longest_cols = [above_label, below_label, "Close All Other Tabs"]
            .iter()
            .map(|s| unicode_column_width(s, None))
            .max()
            .unwrap_or(0) as f32;
        // Two `row_text_inset` gutters (12 logical px each, scaled to device
        // pixels to match `paint_sidebar_dropdown`'s internal `dpi_scale`)
        // plus a one-cell loose margin for rounding.
        let needed_w =
            longest_cols * cell_w_logical + 2. * 12. * dpi_scale + cell_w_logical * dpi_scale;
        let menu_w = needed_w.max(CLOSE_TAB_MENU_MIN_W);

        self.paint_sidebar_dropdown(
            layers,
            menu.x as f32,
            menu.y as f32,
            menu_w,
            downward,
            &rows,
        )
    }

    /// Sidebar SSH quick-launch dropdown. One row per pre-registered
    /// `SshDomain` whose transport is usable (installed binary for mosh/et).
    /// Clicking a row spawns the connection into a new tab — `WezTerm`/`Ssh`
    /// through the mux domain, `Mosh`/`Et` as a plain shell command. There is
    /// no expandable submenu: every row is a single click, mirroring the
    /// new-tab dropdown's flat list.
    pub fn paint_ssh_launch_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.ssh_launch_menu.clone() else {
            return Ok(());
        };
        let entries = self.ssh_quick_launch_entries();
        if entries.is_empty() {
            self.ssh_launch_menu = None;
            return Ok(());
        }

        let rows: Vec<SidebarDropdownRow> = entries
            .iter()
            .map(|entry| {
                let badge = match entry.transport {
                    config::SshTransport::Mosh => "mosh",
                    config::SshTransport::Et => "et",
                    config::SshTransport::Custom => "custom",
                    config::SshTransport::WezTerm => "mux",
                    config::SshTransport::Ssh => "ssh",
                };
                let label = format!("{}  · {}", entry.label, badge);
                SidebarDropdownRow {
                    label,
                    dot_color: None,
                    checkbox: None,
                    divider_above: false,
                    indent: false,
                    trailing_chevron: false,
                    item_type: UIItemType::SidebarSshMenuItem {
                        domain_name: entry.domain_name.clone(),
                    },
                }
            })
            .collect();

        // The button anchors at the bottom of the sidebar, so the dropdown
        // grows upward — same as the agent launch menu.
        self.paint_sidebar_dropdown(
            layers,
            menu.x as f32,
            menu.y as f32,
            AGENT_LAUNCH_MENU_W,
            false,
            &rows,
        )
    }

    /// Downward chevron centered on `(cx, cy)`, drawn as stacked rounded bars
    /// of decreasing width. Hand-drawn rather than a `▾` glyph, which is not
    /// guaranteed to be in the user's font.
    fn draw_sidebar_chevron(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        cx: f32,
        cy: f32,
        dpi_scale: f32,
        color: LinearRgba,
    ) -> anyhow::Result<()> {
        let bar_h = (1.6 * dpi_scale).max(1.5);
        let widest = 9. * dpi_scale;
        const STEPS: usize = 4;
        // Bars shrink from `widest` to a point, stacked downward, so the
        // silhouette reads as a triangle at any DPI.
        for step in 0..STEPS {
            let t = step as f32 / STEPS as f32;
            let w = widest * (1. - t);
            if w < 1. {
                break;
            }
            let y = cy - (widest * 0.25) + step as f32 * bar_h;
            self.sidebar_rounded_fill(
                layers,
                2,
                euclid::rect(cx - w * 0.5, y, w, bar_h),
                bar_h * 0.5,
                color,
            )?;
        }
        Ok(())
    }

    /// Shared renderer for the sidebar's small anchored dropdowns.
    ///
    /// Rows may carry an adapter-colored dot, a checkbox, or neither, and may
    /// request a divider above them. Geometry matches `paint_agent_copy_menu`
    /// so all three menus look identical.
    ///
    /// `downward` controls which side of the anchor the menu opens toward.
    /// Sidebar buttons near the bottom of the window pass `false` so the menu
    /// opens upward; tab-bar close buttons near the top pass `true` so the
    /// menu opens downward.
    fn paint_sidebar_dropdown(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        anchor_x: f32,
        anchor_y: f32,
        width: f32,
        downward: bool,
        rows: &[SidebarDropdownRow],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let cell_width = self.render_metrics.cell_size.width;
        let cell_height = self.render_metrics.cell_size.height;
        let cell_h_f = cell_height as f32;
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let row_vpad = 6. * dpi_scale;
        let row_h = (cell_h_f + 2. * row_vpad).max(AGENT_COPY_MENU_ROW_H * dpi_scale);
        let menu_pad = 8. * dpi_scale;
        let menu_radius = (RADIUS * dpi_scale).min(row_h * 0.5);
        let row_radius = (5. * dpi_scale).min(row_h * 0.5);
        let row_inset = 4. * dpi_scale;
        let row_text_inset = 12. * dpi_scale;
        let divider_h = (1. * dpi_scale).max(1.);
        let divider_gap = 4. * dpi_scale;
        let divider_band = divider_gap * 2. + divider_h;
        // Never wider than the window itself; a 420px submenu on a narrow
        // window would otherwise be clamped to a negative x below.
        let menu_w = (width * dpi_scale)
            .min(self.dimensions.pixel_width as f32 - AGENT_TOOLBELT_GAP * 2.)
            .max(AGENT_LAUNCH_MENU_W);
        let divider_count = rows.iter().filter(|row| row.divider_above).count();
        let menu_h = rows.len() as f32 * row_h + divider_count as f32 * divider_band + menu_pad;

        let max_x = (self.dimensions.pixel_width as f32 - menu_w - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let max_y = (self.dimensions.pixel_height as f32 - menu_h - AGENT_TOOLBELT_GAP)
            .max(AGENT_TOOLBELT_GAP);
        let menu_x = anchor_x.clamp(AGENT_TOOLBELT_GAP, max_x);
        // Open in the requested direction; if that would run off-screen,
        // flip to the other side so the menu stays visible.
        let menu_y = if downward {
            let down_y = anchor_y + AGENT_TOOLBELT_GAP;
            if down_y + menu_h > self.dimensions.pixel_height as f32 - AGENT_TOOLBELT_GAP {
                (anchor_y - menu_h - AGENT_TOOLBELT_GAP).clamp(AGENT_TOOLBELT_GAP, max_y)
            } else {
                down_y.clamp(AGENT_TOOLBELT_GAP, max_y)
            }
        } else {
            let up_y = anchor_y - menu_h - AGENT_TOOLBELT_GAP;
            if up_y < AGENT_TOOLBELT_GAP {
                (anchor_y + AGENT_TOOLBELT_GAP).clamp(AGENT_TOOLBELT_GAP, max_y)
            } else {
                up_y.clamp(AGENT_TOOLBELT_GAP, max_y)
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

        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(menu_x, menu_y, menu_w, menu_h),
            menu_radius,
            bg,
        )?;

        // Thin border stroke so the menu stands out against overlapping rows.
        let border = lerp_rgba(bg, fg, 0.12);
        let border_w = (1. * dpi_scale).max(1.);
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y, menu_w, border_w),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y + menu_h - border_w, menu_w, border_w),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(menu_x, menu_y + border_w, border_w, menu_h - border_w * 2.),
            border,
        )?;
        self.filled_rectangle(
            layers,
            2,
            euclid::rect(
                menu_x + menu_w - border_w,
                menu_y + border_w,
                border_w,
                menu_h - border_w * 2.,
            ),
            border,
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
            let cols = sidebar_text_cols(pixel_width, cell_width as usize);
            if cols == 0 {
                return Ok(());
            }
            let text = truncate_to_cols(text, cols);
            let mut attrs = CellAttributes::default();
            attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
            let mut line = Line::from_text(text, &attrs, 1, None);
            line.resize(cols, SEQ_ZERO);
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
        let dot_size = (cell_h_f * 0.42).clamp(5., 10.);

        let box_size = (cell_h_f * 0.62).clamp(9., 16.);
        let mut row_y = menu_y + menu_pad * 0.5;

        for row in rows {
            if row.divider_above {
                let divider_y = row_y + divider_gap;
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(
                        menu_x + row_text_inset,
                        divider_y,
                        (menu_w - row_text_inset * 2.).max(1.),
                        divider_h,
                    ),
                    lerp_rgba(bg, fg, 0.20),
                )?;
                row_y = divider_y + divider_h + divider_gap;
            }

            let item_type = row.item_type.clone();
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

            // Leading marker: an adapter-colored dot, a checkbox, or nothing.
            let mut text_x = menu_x + row_text_inset;
            if row.indent {
                text_x += cell_width as f32;
            }
            if let Some(color) = row.dot_color {
                self.sidebar_rounded_fill(
                    layers,
                    2,
                    euclid::rect(text_x, row_y + (row_h - dot_size) * 0.5, dot_size, dot_size),
                    dot_size * 0.5,
                    color,
                )?;
                text_x += dot_size + ACTION_ICON_GAP;
            } else if let Some(checked) = row.checkbox {
                // Hand-drawn checkbox rather than a ☑ glyph, which is not
                // guaranteed to exist in the user's font.
                let box_y = row_y + (row_h - box_size) * 0.5;
                let box_radius = (2. * dpi_scale).min(box_size * 0.5);
                self.sidebar_rounded_fill(
                    layers,
                    2,
                    euclid::rect(text_x, box_y, box_size, box_size),
                    box_radius,
                    lerp_rgba(bg, fg, if hovered { 0.70 } else { 0.45 }),
                )?;
                if !checked {
                    // Punch the interior back out to leave an outline-only box.
                    let inset = (1.5 * dpi_scale).min(box_size * 0.4);
                    self.sidebar_rounded_fill(
                        layers,
                        2,
                        euclid::rect(
                            text_x + inset,
                            box_y + inset,
                            (box_size - inset * 2.).max(1.),
                            (box_size - inset * 2.).max(1.),
                        ),
                        (box_radius - inset * 0.5).max(0.),
                        row_bg,
                    )?;
                }
                text_x += box_size + ACTION_ICON_GAP;
            }

            let chevron_w = if row.trailing_chevron {
                cell_width as f32 + CHEVRON_GAP
            } else {
                0.
            };
            render_text(
                self,
                layers,
                &row.label,
                text_x,
                row_y + (row_h - cell_h_f) * 0.5,
                (menu_x + menu_w - row_text_inset - chevron_w - text_x).max(1.),
                contrast_label_color(row_bg),
                row_bg,
            )?;
            if row.trailing_chevron {
                render_text(
                    self,
                    layers,
                    "›",
                    menu_x + menu_w - row_text_inset - cell_width as f32,
                    row_y + (row_h - cell_h_f) * 0.5,
                    cell_width as f32,
                    contrast_label_color(row_bg),
                    row_bg,
                )?;
            }
            self.ui_items.push(UIItem {
                x: (menu_x + row_inset) as usize,
                y: row_y as usize,
                width: (menu_w - row_inset * 2.) as usize,
                height: row_h.ceil() as usize,
                item_type,
            });
            row_y += row_h;
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
        // The top row reserves a square (row_height) at the content column's
        // left for the auto-hide toggle; the search box begins just past it.
        // Tab labels share that same left edge so the "Search tabs..." box and
        // the "1:" / "2:" labels line up in one column (toggle sits top-left).
        let toggle_side = row_height as f32;
        // Only the search box (which shares the search row with the toggle)
        // indents past the toggle; tab labels and "+ New Tab" keep the plain
        // content-column left edge so they line up with the Worktree row
        // instead of being shoved a full toggle-width to the right.
        let list_indent = toggle_side + GAP;
        let text_x = content_x + PAD_X + ACTIVE_TEXT_GAP;
        // Title width runs from the label column to the (reduced) close reserve
        // on the right.
        let text_w = (content_w
            - PAD_X * 2.
            - ACTIVE_TEXT_GAP
            - sidebar_close_text_reserve(cell_width as f32))
        .max(0.);
        let palette = self.palette().clone();
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        // Takes the text and its pixel budget, and derives the cell count from
        // that budget itself. Callers cannot pass a mismatched `cols` because
        // there is no `cols` parameter: handing `render_screen_line` more cells
        // than its pixel width holds is what made labels overhang, since it
        // does not clip a glyph that starts inside the region.
        let render_text = |this: &mut Self,
                           layers: &mut TripleLayerQuadAllocator,
                           text: &str,
                           attrs: &CellAttributes,
                           x: f32,
                           y: f32,
                           pixel_width: f32,
                           fg: LinearRgba,
                           default_bg: LinearRgba|
         -> anyhow::Result<()> {
            let cols = sidebar_text_cols(pixel_width, cell_width);
            if cols == 0 {
                return Ok(());
            }
            let mut line = Line::from_text(truncate_to_cols(text, cols), attrs, 1, None);
            line.resize(cols, SEQ_ZERO);
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
                    // Rail icons draw over a rounded pill fill; a default-bg
                    // glyph cell must stay transparent so the pill shows
                    // through. With `false`, render_screen_line resolves the
                    // default bg to palette.background and paints it opaque,
                    // leaving a dark cell-sized band inside the pill (visible
                    // once the DPI-scaled rail grew wider than the glyph cell).
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

        // Hand-drawn "sidebar panel" icon for the auto-hide toggle: a rounded
        // rectangle outline with a divider a third of the way in, i.e. a window
        // split into a narrow sidebar column plus a content area. Drawn from the
        // same rounded-fill primitives as the Worktree folder icon so it stays
        // crisp at any DPI — the previous Nerd Font "columns" glyph rendered
        // blurry and mis-centered inside the small button. `bg` is the button's
        // own fill, used to hollow out the frame's interior.
        let draw_toggle_icon = |this: &mut Self,
                                layers: &mut TripleLayerQuadAllocator,
                                rect: RectF,
                                fg: LinearRgba,
                                bg: LinearRgba|
         -> anyhow::Result<()> {
            let base = (cell_height as f32).min(rect.size.height);
            let iw = (base * 0.82).max(6.);
            let ih = (base * 0.66).max(5.);
            let ix = rect.min_x() + (rect.size.width - iw) * 0.5;
            let iy = rect.min_y() + (rect.size.height - ih) * 0.5;
            let stroke = (1.5 * dpi_scale).max(1.);
            let radius = (2.5 * dpi_scale).min(ih * 0.5);
            // Outer frame, then punch the interior with the button background to
            // leave a `stroke`-wide rounded border.
            this.sidebar_rounded_fill(layers, 2, euclid::rect(ix, iy, iw, ih), radius, fg)?;
            this.sidebar_rounded_fill(
                layers,
                2,
                euclid::rect(
                    ix + stroke,
                    iy + stroke,
                    (iw - 2. * stroke).max(1.),
                    (ih - 2. * stroke).max(1.),
                ),
                (radius - stroke).max(0.5),
                bg,
            )?;
            // Sidebar divider ~1/3 in from the left, spanning the inner height.
            let divider_x = ix + (iw * 0.34).max(stroke * 2.);
            this.filled_rectangle(
                layers,
                2,
                euclid::rect(divider_x, iy + stroke, stroke, (ih - 2. * stroke).max(1.)),
                fg,
            )?;
            Ok(())
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

            let rail_side = sidebar_rail_icon_side(width as f32, cell_height as f32, dpi_scale);
            let rail_radius = (RADIUS * dpi_scale).min(rail_side * 0.5);
            let rail_x = left + (width as f32 - rail_side) * 0.5;
            let row_stride = rail_side + GAP;
            // Auto-hide toggle occupies the first rail slot so it stays
            // reachable to turn auto-hide back off from the collapsed rail.
            let toggle_top = top + INSET;
            let toggle_rect = euclid::rect(rail_x, toggle_top, rail_side, rail_side);
            let toggle_bg = self.paint_sidebar_autohide_toggle(
                layers,
                toggle_rect,
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                hover_fill,
                pressed_fill,
            )?;
            let toggle_hovered = hovered_item.as_ref() == Some(&UIItemType::SidebarAutoHideToggle);
            let toggle_fg = if toggle_hovered {
                hover_fg
            } else if self.config.sidebar_auto_hide {
                accent
            } else {
                inactive_fg.mul_alpha(0.75)
            };
            draw_toggle_icon(self, layers, toggle_rect, toggle_fg, toggle_bg)?;
            let list_top = toggle_top + row_stride;
            let new_tab_y = top + height - INSET - rail_side;
            // The launcher takes the slot directly above "+", so the tab list
            // must give up that stride or it would paint over the button.
            let rail_launcher_entry = self.agent_launcher_default();
            // SSH quick-launch rail slot sits directly above "+"; the agent
            // rail slot (if any) sits one more stride above that. When neither
            // is present, list_bottom collapses to the new-tab row.
            let ssh_rail_present = !self.ssh_quick_launch_entries().is_empty();
            let ssh_rail_y = if ssh_rail_present {
                Some(new_tab_y - row_stride)
            } else {
                None
            };
            let rail_launch_y = rail_launcher_entry
                .as_ref()
                .map(|_| ssh_rail_y.unwrap_or(new_tab_y) - row_stride);
            let list_bottom = rail_launch_y.or(ssh_rail_y).unwrap_or(new_tab_y);
            let list_height = (list_bottom - GAP - list_top).max(0.);
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
                render_text(
                    self,
                    layers,
                    &symbol,
                    &symbol_attrs,
                    rail_x + (rail_side - symbol_pixel_width) * 0.5,
                    rail_y + tab_offset + (rail_side - cell_height as f32) * 0.5,
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

            // SSH quick-launch rail slot, directly above "+". A flat glyph
            // pair (`>_`) instead of a per-adapter badge: this row opens the
            // dropdown rather than launching a single default connection.
            if let Some(ssh_y) = ssh_rail_y {
                let ssh_type = UIItemType::SidebarSshLaunchButton;
                let ssh_hovered = hovered_item.as_ref() == Some(&ssh_type);
                let ssh_pressed =
                    ssh_hovered && left_pressed && self.pressed_ui_item.as_ref() == Some(&ssh_type);
                let ssh_bg = if ssh_pressed {
                    pressed_fill
                } else if ssh_hovered {
                    hover_fill
                } else {
                    search_fill
                };
                let ssh_offset = if ssh_pressed { 1. } else { 0. };
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(rail_x, ssh_y + ssh_offset, rail_side, rail_side),
                    rail_radius,
                    ssh_bg,
                )?;
                let mut symbol = ">_".to_string();
                let mut symbol_w = 2. * cell_width as f32;
                if symbol_w + 4. > rail_side {
                    symbol = symbol.chars().take(1).collect();
                    symbol_w = cell_width as f32;
                }
                render_text(
                    self,
                    layers,
                    &symbol,
                    &CellAttributes::default(),
                    rail_x + (rail_side - symbol_w) * 0.5,
                    ssh_y + ssh_offset + (rail_side - cell_height as f32) * 0.5,
                    symbol_w,
                    if ssh_hovered {
                        hover_fg
                    } else {
                        inactive_fg.mul_alpha(0.86)
                    },
                    ssh_bg,
                )?;
                self.ui_items.push(UIItem {
                    x: left as usize,
                    y: ssh_y as usize,
                    width,
                    height: rail_side as usize,
                    item_type: ssh_type,
                });
            }

            if let (Some(entry), Some(launch_y)) = (rail_launcher_entry, rail_launch_y) {
                let launch_type = UIItemType::SidebarAgentLaunchButton;
                let launch_hovered = hovered_item.as_ref() == Some(&launch_type);
                let launch_pressed = launch_hovered
                    && left_pressed
                    && self.pressed_ui_item.as_ref() == Some(&launch_type);
                let launch_bg = if launch_pressed {
                    pressed_fill
                } else if launch_hovered {
                    hover_fill
                } else {
                    search_fill
                };
                let launch_offset = if launch_pressed { 1. } else { 0. };
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(rail_x, launch_y + launch_offset, rail_side, rail_side),
                    rail_radius,
                    launch_bg,
                )?;
                // Same 2-char badge the tab icons use, dropping to 1 char when
                // the rail is too narrow for both glyphs.
                let mut symbol = entry.short_label.clone();
                let symbol_w = symbol.chars().count().max(1) as f32 * cell_width as f32;
                let symbol_w = if symbol_w + 4. > rail_side {
                    symbol = symbol.chars().take(1).collect();
                    cell_width as f32
                } else {
                    symbol_w
                };
                render_text(
                    self,
                    layers,
                    &symbol,
                    &CellAttributes::default(),
                    rail_x + (rail_side - symbol_w) * 0.5,
                    launch_y + launch_offset + (rail_side - cell_height as f32) * 0.5,
                    symbol_w,
                    if launch_hovered {
                        entry.color
                    } else {
                        entry.color.mul_alpha(0.88)
                    },
                    launch_bg,
                )?;
                self.ui_items.push(UIItem {
                    x: left as usize,
                    y: launch_y as usize,
                    width,
                    height: rail_side as usize,
                    item_type: launch_type,
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
                euclid::rect(rail_x, new_tab_y + new_tab_offset, rail_side, rail_side),
                rail_radius,
                new_tab_bg,
            )?;
            render_text(
                self,
                layers,
                "+",
                &CellAttributes::default(),
                rail_x + (rail_side - cell_width as f32) * 0.5,
                new_tab_y + new_tab_offset + (rail_side - cell_height as f32) * 0.5,
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

            // No room for a side-by-side chevron on the rail, so it takes the
            // icon's bottom-right corner. Pushed after the "+" item because
            // hit testing walks ui_items in reverse: last pushed wins the
            // overlap.
            if self.config.new_tab_menu.enabled && !self.new_tab_menu_entries().is_empty() {
                let chevron_type = UIItemType::SidebarNewTabMenuButton;
                let chevron_hovered = hovered_item.as_ref() == Some(&chevron_type);
                let corner = rail_side * 0.42;
                let corner_x = rail_x + rail_side - corner;
                let corner_y = new_tab_y + new_tab_offset + rail_side - corner;
                self.draw_sidebar_chevron(
                    layers,
                    corner_x + corner * 0.5,
                    corner_y + corner * 0.5,
                    dpi_scale,
                    if chevron_hovered {
                        hover_fg
                    } else {
                        inactive_fg.mul_alpha(0.75)
                    },
                )?;
                self.ui_items.push(UIItem {
                    x: corner_x as usize,
                    y: corner_y as usize,
                    width: corner as usize,
                    height: corner as usize,
                    item_type: chevron_type,
                });
            }

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
            // Auto-hide toggle sits top-left as a square; the search box fills
            // the rest of the row to its right, its left edge flush with the
            // tab-label column below (content_x + list_indent). Keeping toggle
            // and search on one row means the tab list starts right below with
            // no leftover gap up top.
            let toggle_rect = euclid::rect(content_x, y, toggle_side, toggle_side);
            let toggle_bg = self.paint_sidebar_autohide_toggle(
                layers,
                toggle_rect,
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                hover_fill,
                pressed_fill,
            )?;
            let toggle_hovered = hovered_item.as_ref() == Some(&UIItemType::SidebarAutoHideToggle);
            let toggle_fg = if toggle_hovered {
                hover_fg
            } else if self.config.sidebar_auto_hide {
                accent
            } else {
                inactive_fg.mul_alpha(0.75)
            };
            draw_toggle_icon(self, layers, toggle_rect, toggle_fg, toggle_bg)?;

            let search_x = content_x + list_indent;
            // Extend the search box to the tab-row pill's right edge
            // (item_x + item_w) so it lines up with the highlighted tab rows
            // below, which span the full item width rather than stopping short
            // at the scrollbar gutter / resize grip.
            let search_w = (item_x + item_w - search_x).max(1.);
            let search_text_x = search_x + PAD_X + ACTIVE_TEXT_GAP;
            // Measured from the text origin to the pill's inner right edge. The
            // old budget was `search_w - PAD_X * 2` while the origin was inset
            // by `PAD_X + ACTIVE_TEXT_GAP`, so the text's right bound landed
            // 3px from the pill edge instead of the intended inset.
            let search_text_w = (search_x + search_w - PAD_X - search_text_x).max(0.);

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
            let search_rect =
                euclid::rect(search_x, y + search_offset, search_w, row_height as f32);
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

            let search_cols = sidebar_text_cols(search_text_w, cell_width);
            // A typed query keeps its *tail*, so the caret you are typing at
            // stays visible. The placeholder steps down through shorter forms
            // and is dropped entirely rather than shown as a truncated stub.
            let search_text = match &self.sidebar_search {
                Some(state) if state.query.is_empty() => Some("|".to_string()),
                Some(state) => {
                    let full = format!("{}|", state.query);
                    Some(truncate_to_cols_from_end(&full, search_cols).to_string())
                }
                None => fit_label(&SEARCH_PLACEHOLDER_LABELS, search_cols).map(str::to_string),
            };
            let search_fg = if focused || search_hovered {
                hover_fg
            } else {
                inactive_fg.mul_alpha(0.62)
            };
            if let Some(search_text) = &search_text {
                render_text(
                    self,
                    layers,
                    search_text,
                    &CellAttributes::default(),
                    search_text_x,
                    y + search_offset + (row_height as f32 - cell_height as f32) * 0.5,
                    search_text_w,
                    search_fg,
                    search_bg,
                )?;
            }
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
            let toggle_bg = self.paint_sidebar_autohide_toggle(
                layers,
                toggle_rect,
                dpi_scale,
                &hovered_item,
                left_pressed,
                surface,
                inactive_fg,
                hover_fill,
                pressed_fill,
            )?;
            let toggle_hovered = hovered_item.as_ref() == Some(&UIItemType::SidebarAutoHideToggle);
            let toggle_fg = if toggle_hovered {
                hover_fg
            } else if self.config.sidebar_auto_hide {
                accent
            } else {
                inactive_fg.mul_alpha(0.75)
            };
            draw_toggle_icon(self, layers, toggle_rect, toggle_fg, toggle_bg)?;
            y += toggle_side + GAP;
        }

        let rows = self.sidebar_rows();

        let tab_list_top = y;
        // row_height (self.sidebar_row_height()) already derives its height
        // directly from the DPI-scaled cell height, so rows already enclose
        // the glyphs; only the corner radius (an unscaled px constant) needs
        // dpi_scale applied.
        let tab_row_radius = (RADIUS * dpi_scale).min(row_height as f32 * 0.5);
        let bottom_button_rows = self.sidebar_bottom_button_rows();
        let new_tab_y = top + height - INSET - row_height as f32;
        let tab_list_bottom =
            new_tab_y - GAP - (bottom_button_rows - 1.) * (row_height as f32 + GAP);
        let tab_list_height = (tab_list_bottom - tab_list_top).max(0.);
        let row_stride = row_height as f32 + GAP;
        let visible_rows = ((tab_list_height + GAP) / row_stride).floor().max(0.) as usize;
        let max_offset = rows.len().saturating_sub(visible_rows);
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

        let total_tabs = rows.len();
        for row in rows
            .into_iter()
            .skip(self.sidebar_scroll_offset)
            .take(visible_rows)
        {
            let (tab_idx, active, title, metadata, pane_count, expanded) = match row {
                SidebarRow::Tab {
                    tab_idx,
                    active,
                    title,
                    metadata,
                    pane_count,
                    expanded,
                } => (tab_idx, active, title, metadata, pane_count, expanded),
                SidebarRow::Pane {
                    pane_id,
                    active,
                    label,
                    is_remote,
                } => {
                    let row_type = UIItemType::SidebarPaneRow { pane_id };
                    let close_type = UIItemType::SidebarPaneClose { pane_id };
                    let row_hovered = hovered_item.as_ref() == Some(&row_type);
                    let close_hovered = hovered_item.as_ref() == Some(&close_type);
                    let row_pressed = left_pressed
                        && row_hovered
                        && self.pressed_ui_item.as_ref() == Some(&row_type);
                    let close_pressed = left_pressed
                        && close_hovered
                        && self.pressed_ui_item.as_ref() == Some(&close_type);
                    // Child rows sit a step in from their tab so the hierarchy
                    // reads without needing tree lines.
                    let indent = PANE_ROW_INDENT;
                    let row_x = item_x + indent;
                    let row_w = (item_w - indent).max(1.);
                    let row_bg = if active {
                        active_fill
                    } else if row_pressed {
                        pressed_fill
                    } else if row_hovered || close_hovered {
                        hover_fill
                    } else {
                        surface
                    };
                    let row_offset = if row_pressed { 1. } else { 0. };
                    if active || row_hovered || close_hovered {
                        self.sidebar_rounded_fill(
                            layers,
                            1,
                            euclid::rect(row_x, y + row_offset, row_w, row_height as f32),
                            tab_row_radius,
                            row_bg,
                        )?;
                    }
                    if active {
                        let rail_h = (row_height as f32 * 0.45).max(cell_height as f32 * 0.5);
                        let rail_y = y + row_offset + (row_height as f32 - rail_h) * 0.5;
                        let rail_x = match self.config.sidebar_position {
                            SidebarPosition::Left => row_x + 2.,
                            SidebarPosition::Right => row_x + row_w - ACTIVE_RAIL_W - 2.,
                        };
                        self.sidebar_rounded_fill(
                            layers,
                            2,
                            euclid::rect(rail_x, rail_y, ACTIVE_RAIL_W, rail_h),
                            ACTIVE_RAIL_W * 0.5,
                            accent,
                        )?;
                    }

                    // A pane on another host next to a local one is the whole
                    // point of the force-local launch, so say so rather than
                    // leaving two identical-looking rows.
                    let label = if is_remote {
                        format!("{} (remote)", label)
                    } else {
                        label
                    };
                    let label_x = content_x + indent + PAD_X + ACTIVE_TEXT_GAP;
                    let label_w = (content_w
                        - indent
                        - PAD_X * 2.
                        - ACTIVE_TEXT_GAP
                        - sidebar_close_text_reserve(cell_width as f32))
                    .max(0.);
                    render_text(
                        self,
                        layers,
                        &label,
                        &CellAttributes::default(),
                        label_x,
                        y + row_offset + (row_height as f32 - cell_height as f32) * 0.5,
                        label_w,
                        if active || row_hovered || close_hovered {
                            inactive_fg
                        } else {
                            inactive_fg.mul_alpha(0.66)
                        },
                        row_bg,
                    )?;
                    self.ui_items.push(UIItem {
                        x: (content_x + indent) as usize,
                        y: y as usize,
                        width: (content_w - indent - CLOSE_ZONE_W).max(0.) as usize,
                        height: row_height,
                        item_type: row_type,
                    });

                    let close_x = content_x + content_w - CLOSE_ZONE_W;
                    // Only drawn on hover: a persistent × on every pane row
                    // would crowd an already indented line.
                    if row_hovered || close_hovered {
                        render_text(
                            self,
                            layers,
                            "×",
                            &CellAttributes::default(),
                            close_x + CLOSE_ZONE_W - cell_width as f32 - CLOSE_GLYPH_INSET,
                            y + row_offset
                                + (row_height as f32 - cell_height as f32) * 0.5
                                + if close_pressed { 1. } else { 0. },
                            cell_width as f32,
                            if close_hovered {
                                hover_fg
                            } else {
                                inactive_fg.mul_alpha(0.70)
                            },
                            LinearRgba::default(),
                        )?;
                    }
                    self.ui_items.push(UIItem {
                        x: close_x as usize,
                        y: y as usize,
                        width: CLOSE_ZONE_W as usize,
                        height: row_height,
                        item_type: close_type,
                    });

                    y += row_height as f32 + GAP;
                    continue;
                }
            };
            let tab_type = UIItemType::SidebarTab { tab_idx, active };
            let close_type = UIItemType::SidebarCloseTab(tab_idx);
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

            // Split tabs get a chevron that shows or hides their pane rows.
            // Single-pane tabs keep the layout they always had — there is
            // nothing to expand, so nothing is drawn and nothing shifts.
            let expand_type = UIItemType::SidebarTabExpand { tab_idx };
            let expand_hovered = hovered_item.as_ref() == Some(&expand_type);
            let agent = self.sidebar_agent_for_tab_idx(tab_idx);
            // One composition for the whole row: the title's origin *and* its
            // width both step past the chevron and the status dot. Deriving
            // only the width from them is what painted the leading "N: " index
            // on top of the chevron and the dot.
            let cols = sidebar_row_columns(
                text_x,
                text_w,
                cell_width as f32,
                cell_height as f32,
                pane_count > 1,
                agent.is_some(),
            );
            if pane_count > 1 {
                render_text(
                    self,
                    layers,
                    if expanded { "⌄" } else { "›" },
                    &CellAttributes::default(),
                    cols.chevron_x,
                    y + row_offset + (row_height as f32 - cell_height as f32) * 0.5,
                    cell_width as f32,
                    if expand_hovered {
                        hover_fg
                    } else {
                        inactive_fg.mul_alpha(0.70)
                    },
                    row_bg,
                )?;
            }

            if let Some(agent) = &agent {
                let badge_size = sidebar_status_dot_size(cell_height as f32);
                let badge_y = y + row_offset + (row_height as f32 - badge_size) * 0.5;
                let badge_base = if active {
                    accent
                } else {
                    inactive_fg.mul_alpha(0.58)
                };
                let badge_color = agent_status_dot_accent(
                    &agent.status,
                    badge_base,
                    row_bg,
                    self.agent_dot_pulse(agent),
                );
                self.sidebar_pill_fill(
                    layers,
                    2,
                    euclid::rect(cols.badge_x, badge_y, badge_size, badge_size),
                    badge_size * 0.5,
                    badge_color,
                )?;
            }

            let display_title = if title.trim_start().starts_with(&format!("{}:", tab_idx + 1)) {
                title
            } else {
                format!("{}: {}", tab_idx + 1, title)
            };
            let metadata_text = metadata.join(" · ");
            let show_metadata = !metadata_text.is_empty()
                && self.sidebar_metadata_rows_enabled()
                && (active || tab_hovered || close_hovered);
            let (primary_offset, metadata_offset) =
                sidebar_row_text_offsets(row_height as f32, cell_height as f32, show_metadata);
            let primary_y = y + row_offset + primary_offset;

            render_text(
                self,
                layers,
                &display_title,
                &CellAttributes::default(),
                cols.text_x,
                primary_y,
                cols.text_w,
                if active || tab_hovered || close_hovered {
                    inactive_fg
                } else {
                    inactive_fg.mul_alpha(0.78)
                },
                row_bg,
            )?;
            if let Some(metadata_offset) = metadata_offset {
                render_text(
                    self,
                    layers,
                    &metadata_text,
                    &CellAttributes::default(),
                    cols.text_x,
                    y + row_offset + metadata_offset,
                    cols.text_w,
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
            // Pushed after the tab row so hit-testing, which takes the last
            // matching item, resolves clicks on the chevron to the chevron and
            // not to the tab underneath it.
            if pane_count > 1 {
                self.ui_items.push(UIItem {
                    x: cols.chevron_x as usize,
                    y: y as usize,
                    width: cols.chevron_w as usize,
                    height: row_height,
                    item_type: expand_type,
                });
            }

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
                        close_x + CLOSE_ZONE_W - close_button_side - 3.,
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
            let close_glyph_x = close_x + CLOSE_ZONE_W - cell_width as f32 - CLOSE_GLYPH_INSET;
            let close_glyph_offset = if close_pressed { 1. } else { 0. };
            render_text(
                self,
                layers,
                "×",
                &CellAttributes::default(),
                close_glyph_x,
                y + row_offset
                    + (row_height as f32 - cell_height as f32) * 0.5
                    + close_glyph_offset,
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

        let ssh_rail_present = !self.ssh_quick_launch_entries().is_empty();
        // SSH row sits directly above the new-tab row; the worktree/agent row
        // moves one stride higher when the SSH row is present.
        let ssh_row_y = new_tab_y - row_height as f32 - GAP;
        let worktree_y = if ssh_rail_present {
            ssh_row_y - row_height as f32 - GAP
        } else {
            ssh_row_y
        };
        if ssh_rail_present {
            let ssh_type = UIItemType::SidebarSshLaunchButton;
            let ssh_hovered = hovered_item.as_ref() == Some(&ssh_type);
            let ssh_pressed =
                ssh_hovered && left_pressed && self.pressed_ui_item.as_ref() == Some(&ssh_type);
            let ssh_bg = if ssh_pressed {
                pressed_fill
            } else if ssh_hovered {
                hover_fill
            } else {
                search_fill
            };
            let ssh_offset = if ssh_pressed { 1. } else { 0. };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(item_x, ssh_row_y + ssh_offset, item_w, row_height as f32),
                RADIUS,
                ssh_bg,
            )?;
            let ssh_label_w = (item_w - PAD_X * 2. - ACTIVE_TEXT_GAP).max(0.);
            if let Some(ssh_label) =
                fit_label(&SSH_ROW_LABELS, sidebar_text_cols(ssh_label_w, cell_width))
            {
                render_text(
                    self,
                    layers,
                    ssh_label,
                    &CellAttributes::default(),
                    text_x,
                    ssh_row_y + ssh_offset + (row_height as f32 - cell_height as f32) * 0.5,
                    ssh_label_w,
                    if ssh_hovered {
                        hover_fg
                    } else {
                        inactive_fg.mul_alpha(0.86)
                    },
                    ssh_bg,
                )?;
            }
            self.ui_items.push(UIItem {
                x: content_x as usize,
                y: ssh_row_y as usize,
                width: item_w as usize,
                height: row_height,
                item_type: ssh_type,
            });
        }
        if width > 180 {
            // The row is shared: Worktree on the left, the agent launcher on
            // the right. With no agent installed the worktree button keeps the
            // full width it had before the launcher existed.
            let launcher_entry = self.agent_launcher_default();
            let dot_size = sidebar_status_dot_size(cell_height as f32);
            // Always reserve the agent launcher area even when no adapter is
            // installed: the button opens a dropdown whose "Agent insight" and
            // "Resume session" rows are useful without any adapter.
            let bottom_row =
                sidebar_bottom_row_layout(item_x, item_w, content_x, content_w, dot_size, true);

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
                    bottom_row.worktree_fill_x,
                    worktree_y + worktree_offset,
                    bottom_row.worktree_fill_w,
                    row_height as f32,
                ),
                RADIUS * dpi_scale,
                worktree_bg,
            )?;
            let folder_color = if worktree_hovered {
                hover_fg.mul_alpha(0.90)
            } else {
                inactive_fg.mul_alpha(0.70)
            };
            let icon_x = bottom_row.worktree_icon_x;
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
            // Widest label that actually fits its half, measured — not chosen by
            // a width threshold, which cannot know the cell width, the adapter's
            // configured label length or whether the scrollbar gutter is present.
            // Nothing fits at all -> the folder icon speaks for itself.
            let worktree_cols = sidebar_text_cols(bottom_row.worktree_text_w, cell_width);
            if let Some(worktree_label) = fit_label(&WORKTREE_LABELS, worktree_cols) {
                render_text(
                    self,
                    layers,
                    worktree_label,
                    &CellAttributes::default(),
                    bottom_row.worktree_text_x,
                    worktree_y + worktree_offset + (row_height as f32 - cell_height as f32) * 0.5,
                    bottom_row.worktree_text_w,
                    if worktree_hovered {
                        hover_fg
                    } else {
                        inactive_fg.mul_alpha(0.86)
                    },
                    worktree_bg,
                )?;
            }
            self.ui_items.push(UIItem {
                x: content_x as usize,
                y: worktree_y as usize,
                width: (bottom_row.worktree_fill_x + bottom_row.worktree_fill_w - content_x).max(1.)
                    as usize,
                height: row_height,
                item_type: worktree_type,
            });

            {
                let agent_type = UIItemType::SidebarAgentLaunchButton;
                let agent_hovered = hovered_item.as_ref() == Some(&agent_type);
                let agent_pressed = agent_hovered
                    && left_pressed
                    && self.pressed_ui_item.as_ref() == Some(&agent_type);
                let agent_bg = if agent_pressed {
                    pressed_fill
                } else if agent_hovered {
                    hover_fill
                } else {
                    search_fill
                };
                let agent_offset = if agent_pressed { 1. } else { 0. };
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(
                        bottom_row.agent_fill_x,
                        worktree_y + agent_offset,
                        bottom_row.agent_fill_w,
                        row_height as f32,
                    ),
                    RADIUS * dpi_scale,
                    agent_bg,
                )?;

                // Adapter-colored dot (or a neutral dot when no adapter).
                let dot_y = worktree_y + agent_offset + (row_height as f32 - dot_size) * 0.5;
                let dot_color = if let Some(ref entry) = launcher_entry {
                    if agent_hovered {
                        entry.color
                    } else {
                        entry.color.mul_alpha(0.88)
                    }
                } else {
                    inactive_fg.mul_alpha(0.5)
                };
                self.sidebar_rounded_fill(
                    layers,
                    2,
                    euclid::rect(bottom_row.agent_dot_x, dot_y, dot_size, dot_size),
                    dot_size * 0.5,
                    dot_color,
                )?;

                // Label: adapter label when available, generic "Agents" otherwise.
                let agent_label_opt = if let Some(ref entry) = launcher_entry {
                    let agent_label_rungs = [entry.label.as_str(), entry.short_label.as_str()];
                    let agent_cols = sidebar_text_cols(bottom_row.agent_text_w, cell_width);
                    fit_label(&agent_label_rungs, agent_cols).map(|s| s.to_string())
                } else {
                    let agent_cols = sidebar_text_cols(bottom_row.agent_text_w, cell_width);
                    fit_label(&["Agents", "Agent"], agent_cols).map(|s| s.to_string())
                };
                if let Some(agent_label) = agent_label_opt {
                    render_text(
                        self,
                        layers,
                        &agent_label,
                        &CellAttributes::default(),
                        bottom_row.agent_text_x,
                        worktree_y + agent_offset + (row_height as f32 - cell_height as f32) * 0.5,
                        bottom_row.agent_text_w,
                        if agent_hovered {
                            hover_fg
                        } else {
                            inactive_fg.mul_alpha(0.86)
                        },
                        agent_bg,
                    )?;
                }
                // Hit region matches the drawn pill (which reaches the row's
                // right edge), not the narrower content column.
                self.ui_items.push(UIItem {
                    x: bottom_row.agent_fill_x as usize,
                    y: worktree_y as usize,
                    width: bottom_row.agent_fill_w as usize,
                    height: row_height,
                    item_type: agent_type,
                });
            }
        }

        // Windows-Terminal-style split button: the label opens a tab, the
        // chevron on the right opens the shell/domain picker. Right-clicking
        // the label still opens WezTerm's full launcher overlay.
        let show_chevron =
            self.config.new_tab_menu.enabled && !self.new_tab_menu_entries().is_empty();
        let chevron_w = if show_chevron {
            (row_height as f32 * 0.85).clamp(20., 34.)
        } else {
            0.
        };
        let new_tab_fill_w = (item_w - chevron_w).max(1.);
        let chevron_x = item_x + new_tab_fill_w;
        // Both hit regions key off the drawn geometry. Deriving them from the
        // content column instead put the chevron's hit box a full button width
        // left of the glyph: the fills span the whole item width, while the
        // content column stops short of the resize grip + scrollbar gutter.
        let new_tab_hit_w = (chevron_x - content_x).max(1.);

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
                new_tab_fill_w,
                row_height as f32,
            ),
            RADIUS,
            new_tab_bg,
        )?;
        let new_tab_text_w = (new_tab_hit_w - PAD_X * 2. - ACTIVE_TEXT_GAP).max(0.);
        if let Some(new_tab_label) = fit_label(
            &NEW_TAB_LABELS,
            sidebar_text_cols(new_tab_text_w, cell_width),
        ) {
            render_text(
                self,
                layers,
                new_tab_label,
                &CellAttributes::default(),
                text_x,
                new_tab_y + new_tab_offset + (row_height as f32 - cell_height as f32) * 0.5,
                new_tab_text_w,
                if new_tab_hovered {
                    hover_fg
                } else {
                    inactive_fg
                },
                new_tab_bg,
            )?;
        }
        self.ui_items.push(UIItem {
            x: content_x as usize,
            y: new_tab_y as usize,
            width: new_tab_hit_w as usize,
            height: row_height,
            item_type: new_tab_type,
        });

        if show_chevron {
            let chevron_type = UIItemType::SidebarNewTabMenuButton;
            let chevron_hovered = hovered_item.as_ref() == Some(&chevron_type);
            let chevron_pressed = chevron_hovered
                && left_pressed
                && self.pressed_ui_item.as_ref() == Some(&chevron_type);
            let chevron_bg = if chevron_pressed {
                pressed_fill
            } else if chevron_hovered {
                hover_fill
            } else {
                search_fill
            };
            let chevron_offset = if chevron_pressed { 1. } else { 0. };
            self.sidebar_rounded_fill(
                layers,
                1,
                euclid::rect(
                    chevron_x,
                    new_tab_y + chevron_offset,
                    chevron_w,
                    row_height as f32,
                ),
                RADIUS,
                chevron_bg,
            )?;
            self.draw_sidebar_chevron(
                layers,
                chevron_x + chevron_w * 0.5,
                new_tab_y + chevron_offset + row_height as f32 * 0.5,
                dpi_scale,
                if chevron_hovered {
                    hover_fg
                } else {
                    inactive_fg.mul_alpha(0.86)
                },
            )?;
            self.ui_items.push(UIItem {
                x: chevron_x as usize,
                y: new_tab_y as usize,
                width: chevron_w as usize,
                height: row_height,
                item_type: chevron_type,
            });
        }

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

        // Agent herd section: rendered below the tab list.
        if self.config.agent_ui.section.enabled {
            self.update_agent_herd_state();
            self.paint_agent_herd_section(
                layers,
                width as f32,
                left as f32,
                top as f32,
                height as f32,
            )?;
        }

        Ok(())
    }

    /// Populate `AgentHerdState` from the per-pane agent detection cache.
    /// Converts the older `AgentPaneState` detections into the new
    /// `HerdAgent` model so the agent section renders with live data.
    fn update_agent_herd_state(&mut self) {
        self.kick_agent_herd_scan();
        let mux = Mux::get();
        let Some(window) = mux.get_window(self.mux_window_id) else {
            return;
        };
        let mut agents = Vec::new();
        let mut session_ids = HashSet::new();

        for tab_idx in 0..window.len() {
            let agent = match self.sidebar_agent_for_tab_idx(tab_idx) {
                Some(a) => a,
                None => continue,
            };
            let pane_id = self
                .sidebar_primary_pane_for_tab_idx(tab_idx)
                .map(|pane| pane.pane_id());

            let vendor = match agent.kind {
                AgentKind::Claude => AgentVendor::Claude,
                AgentKind::Codex => AgentVendor::Codex,
                AgentKind::Gemini => AgentVendor::Gemini,
                AgentKind::OpenCode => AgentVendor::OpenCode,
                AgentKind::Copilot => AgentVendor::Copilot,
                AgentKind::Cursor => AgentVendor::Cursor,
                AgentKind::Amp => AgentVendor::Amp,
                AgentKind::Unknown(_) => {
                    AgentVendor::Custom(agent.adapter_id.clone().unwrap_or_default())
                }
            };

            let status = herd_status_from_agent(agent.status);

            let name = agent.model.as_deref().unwrap_or(vendor.label()).to_string();

            let project_root = agent
                .cwd
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf());

            agents.push(HerdAgent {
                provider: agent.adapter_id.clone().unwrap_or_default(),
                vendor,
                name,
                status,
                model: agent.model,
                cwd: agent.cwd,
                project_root,
                pane_id,
                session_id: agent.session_id,
                blocked_reason: None,
                git_branch: None,
                pid: None,
                started_at: None,
                status_changed_at: None,
                activity: None,
                subagents: Vec::new(),
            });
            if let Some(session_id) = agents.last().and_then(|agent| agent.session_id.clone()) {
                session_ids.insert(session_id);
            }
        }

        if let Some((_, sessions)) = self.agent_herd_session_cache.as_ref() {
            for session in sessions.iter() {
                if !session.session_id.is_empty() && session_ids.contains(&session.session_id) {
                    continue;
                }
                agents.push(herd_agent_from_vendor_session(session));
            }
        }

        let mut state = self.agent_herd_state.borrow_mut();
        state.agents = agents;
    }

    /// Refresh vendor session files without blocking paint on filesystem I/O.
    fn kick_agent_herd_scan(&mut self) {
        let ttl = Duration::from_millis(self.config.agent_ui.section.refresh_ms.clamp(100, 10000));
        if self.agent_herd_scan_pending
            || self
                .agent_herd_session_cache
                .as_ref()
                .is_some_and(|(scanned_at, _)| scanned_at.elapsed() < ttl)
        {
            return;
        }
        let Some(home) = dirs_next::home_dir() else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };

        self.agent_herd_scan_pending = true;
        let future = promise::spawn::spawn_into_new_thread(move || {
            let sessions = crate::agent_herd::default_registry().collect_all(&home);
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                term_window.agent_herd_scan_pending = false;
                term_window.agent_herd_session_cache = Some((Instant::now(), Arc::new(sessions)));
            })));
            Ok::<(), anyhow::Error>(())
        });
        promise::spawn::spawn(async move {
            if let Err(err) = future.await {
                log::error!("agent herd session scan failed: {err:#}");
            }
        })
        .detach();
    }

    /// Render a single line of text as GPU quads.
    fn paint_text(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        text: &str,
        x: f32,
        y: f32,
        pixel_width: f32,
        fg: LinearRgba,
        bg: LinearRgba,
        bold: bool,
    ) -> anyhow::Result<()> {
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let cols = (pixel_width / cell_width).floor().max(0.0) as usize;
        if cols == 0 || text.is_empty() {
            return Ok(());
        }
        let text: String = text
            .chars()
            .take_while(|c| {
                let w = unicode_column_width(&c.to_string(), None);
                if w > cols {
                    return false;
                }
                true
            })
            .collect();
        let mut attrs = CellAttributes::default();
        attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
        attrs.set_background(ColorAttribute::TrueColorWithDefaultFallback(bg.to_srgb()));
        if bold {
            attrs.set_intensity(Intensity::Bold);
        }
        let line = Line::from_text(&text, &attrs, 1, None);
        let palette = self.palette().clone();
        let white_space = self
            .render_state
            .as_ref()
            .map(|s| s.util_sprites.white_space.texture_coords())
            .unwrap_or_default();
        let filled_box = self
            .render_state
            .as_ref()
            .map(|s| s.util_sprites.filled_box.texture_coords())
            .unwrap_or_default();
        let config = &self.config;
        let render_metrics = self.render_metrics;
        let dimensions_dpi = self.dimensions.dpi as u32;
        let experimental_pixel_positioning = config.experimental_pixel_positioning;
        self.render_screen_line(
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
                    dpi: dimensions_dpi,
                    pixel_height: cell_height as usize,
                    pixel_width: pixel_width as usize,
                    reverse_video: false,
                },
                config,
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
                default_bg: bg,
                style: None,
                font: None,
                use_pixel_positioning: experimental_pixel_positioning,
                render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;
        Ok(())
    }

    /// Paint the agent herd section at the bottom of the sidebar.
    /// Renders a collapsible header with agent count, then each agent
    /// as a row with a status dot, vendor glyph, name, project, and
    /// status hint. Clicking a row expands inline detail.
    fn paint_agent_herd_section(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        sidebar_w: f32,
        sidebar_left: f32,
        sidebar_top: f32,
        sidebar_h: f32,
    ) -> anyhow::Result<()> {
        let state = self.agent_herd_state.borrow();
        let collapsed = state.collapsed;
        let agents = state.agents.clone();
        let expanded = state.expanded;
        drop(state);

        let dpi = (self.dimensions.dpi as f32 / 96.0).clamp(1.0, 2.5);
        let cell_h = self.render_metrics.cell_size.height as f32;
        let base_row_h = cell_h + 4.0 * dpi;
        let header_h = cell_h + 8.0 * dpi;
        let pad = 8.0 * dpi;

        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_else(TabBarColors::default);
        let bg = opaque(colors.background().to_linear());
        let fg = srgb8_to_linear(200, 200, 200);
        let dim = srgb8_to_linear(130, 130, 130);

        let section_x = sidebar_left;
        let section_w = sidebar_w;

        // Count agents.
        let agent_count = agents.len();

        // Anchor section to bottom while keeping header visible when collapsed.
        let content_h = if collapsed {
            0.0
        } else {
            agents
                .iter()
                .enumerate()
                .map(|(idx, _)| {
                    let detail_h = if expanded == Some(idx) {
                        base_row_h * 2.0
                    } else {
                        0.0
                    };
                    base_row_h + detail_h + 2.0 * dpi
                })
                .sum()
        };
        let section_bottom = sidebar_top + sidebar_h - pad;
        let header_y = (section_bottom - header_h - content_h).max(sidebar_top);
        let header_rect = euclid::rect(section_x, header_y, section_w, header_h);
        self.filled_rectangle(layers, 0, header_rect, bg)?;

        // Chevron toggle.
        let chevron = if collapsed { "▸" } else { "▾" };
        self.paint_text(
            layers,
            chevron,
            section_x + pad,
            header_y + (header_h - cell_h) * 0.5,
            cell_h,
            fg,
            bg,
            false,
        )?;

        // "Agents · N" label.
        let label = format!("Agents · {agent_count}");
        self.paint_text(
            layers,
            &label,
            section_x + pad + cell_h + 4.0 * dpi,
            header_y + (header_h - cell_h) * 0.5,
            section_w - 2.0 * pad - cell_h,
            fg,
            bg,
            true,
        )?;

        // Push UIItem for the header click.
        self.ui_items.push(UIItem {
            x: section_x as usize,
            y: header_y as usize,
            width: section_w as usize,
            height: header_h as usize,
            item_type: UIItemType::SidebarAgentSectionHeader,
        });

        // Agent rows below header.
        if collapsed {
            return Ok(());
        }

        let mut y = header_y + header_h;
        for (idx, agent) in agents.iter().enumerate() {
            let is_expanded = expanded == Some(idx);
            let row_h = base_row_h;
            let detail_h = if is_expanded { row_h * 2.0 } else { 0.0 };

            if y + row_h + detail_h > section_bottom {
                break;
            }

            // Row background.
            let row_rect = euclid::rect(section_x, y, section_w, row_h);
            self.filled_rectangle(layers, 0, row_rect, bg)?;

            // Status dot.
            let dot_x = section_x + pad + 8.0 * dpi;
            let dot_y = y + row_h * 0.5;
            let dot_color = agent.vendor_dot_color();
            let dot_rect = euclid::rect(dot_x - 4.0 * dpi, dot_y - 4.0 * dpi, 8.0 * dpi, 8.0 * dpi);
            self.filled_rectangle(
                layers,
                1,
                dot_rect,
                srgb8_to_linear(dot_color.0, dot_color.1, dot_color.2),
            )?;

            // Vendor glyph + name.
            let name_x = section_x + pad + 20.0 * dpi;
            let name = format!("{} {}", agent.vendor_glyph(), agent.name);
            self.paint_text(
                layers,
                &name,
                name_x,
                y + (row_h - cell_h) * 0.5,
                section_w - 2.0 * pad - 40.0 * dpi,
                fg,
                bg,
                true,
            )?;

            // Project label (right-aligned, dim).
            let project = agent.project_label();
            let project_w = (project.len() as f32) * cell_h * 0.6;
            self.paint_text(
                layers,
                &project,
                section_x + section_w - pad - project_w,
                y + (row_h - cell_h) * 0.5,
                project_w,
                dim,
                bg,
                false,
            )?;

            // Push UIItem for the row click.
            self.ui_items.push(UIItem {
                x: section_x as usize,
                y: y as usize,
                width: section_w as usize,
                height: row_h as usize,
                item_type: UIItemType::SidebarAgentRow { index: idx },
            });

            // Expanded detail view.
            if is_expanded {
                let detail_y = y + row_h;
                let detail_rect = euclid::rect(section_x, detail_y, section_w, detail_h);
                self.filled_rectangle(layers, 0, detail_rect, bg)?;

                let detail_x = section_x + pad + 20.0 * dpi;
                let mut detail_line_y = detail_y + pad;

                // Status + model line.
                let status_label = agent.status.label();
                let model_text = agent.model.as_deref().unwrap_or("");
                let status_line = if model_text.is_empty() {
                    status_label.to_string()
                } else {
                    format!("{} · {}", status_label, model_text)
                };
                self.paint_text(
                    layers,
                    &status_line,
                    detail_x,
                    detail_line_y,
                    section_w - 2.0 * pad - 40.0 * dpi,
                    fg,
                    bg,
                    false,
                )?;
                detail_line_y += cell_h + 2.0 * dpi;

                // Project root line.
                if let Some(ref root) = agent.project_root {
                    let root_text = format!("📁 {}", root.display());
                    self.paint_text(
                        layers,
                        &root_text,
                        detail_x,
                        detail_line_y,
                        section_w - 2.0 * pad - 40.0 * dpi,
                        dim,
                        bg,
                        false,
                    )?;
                    detail_line_y += cell_h + 2.0 * dpi;
                }

                // Activity hint.
                if let Some(ref activity) = agent.activity {
                    let activity_text = activity
                        .current
                        .as_ref()
                        .map(|e| e.display_text())
                        .unwrap_or_default();
                    if !activity_text.is_empty() {
                        self.paint_text(
                            layers,
                            &activity_text,
                            detail_x,
                            detail_line_y,
                            section_w - 2.0 * pad - 40.0 * dpi,
                            dim,
                            bg,
                            false,
                        )?;
                    }
                }

                // Push UIItem for the detail expand click.
                self.ui_items.push(UIItem {
                    x: section_x as usize,
                    y: detail_y as usize,
                    width: section_w as usize,
                    height: detail_h as usize,
                    item_type: UIItemType::SidebarAgentDetailExpand { index: idx },
                });

                // Push UIItem for the focus pane click.
                self.ui_items.push(UIItem {
                    x: (section_x + section_w - pad - 40.0 * dpi) as usize,
                    y: (detail_y + pad) as usize,
                    width: 40,
                    height: cell_h as usize,
                    item_type: UIItemType::SidebarAgentFocusPane { index: idx },
                });
            }

            y += row_h + detail_h + 2.0 * dpi;
        }

        Ok(())
    }

    /// Paint the sidebar auto-hide toggle button into `rect` and register its
    /// hit region. The icon (a "rail + panel" glyph) is filled/accented when
    /// auto-hide is ON and dimmed when OFF, so the button reflects current state.
    /// Paints the auto-hide toggle's background box and registers its hit
    /// region. Returns the background color it used so the caller can draw a
    /// centered glyph on top that blends against the same fill. The glyph
    /// itself is drawn at the call site (where the `render_text` closure is in
    /// scope); this method owns only the box.
    fn paint_sidebar_autohide_toggle(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        rect: RectF,
        dpi_scale: f32,
        hovered_item: &Option<UIItemType>,
        left_pressed: bool,
        surface: LinearRgba,
        inactive_fg: LinearRgba,
        hover_fill: LinearRgba,
        pressed_fill: LinearRgba,
    ) -> anyhow::Result<LinearRgba> {
        let item_type = UIItemType::SidebarAutoHideToggle;
        let hovered = hovered_item.as_ref() == Some(&item_type);
        let pressed = left_pressed && hovered && self.pressed_ui_item.as_ref() == Some(&item_type);

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

        self.ui_items.push(UIItem {
            x: rect.min_x() as usize,
            y: rect.min_y() as usize,
            width: rect.size.width as usize,
            height: rect.size.height as usize,
            item_type,
        });
        Ok(bg)
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

        // Overlap adjacent pieces by up to 1px so fractional-pixel edges (and
        // the corner sprite's floored `r as isize` cell) never leave a hairline
        // of the layer beneath showing through as a seam. On the collapsed rail
        // the layer under an opaque tab pill is the dark window, so an
        // uncovered seam reads as a dark line straight across the icon box.
        // Colors here are opaque, so overlapping is idempotent (no double-blend).
        let o = 1.0_f32.min(r);
        // Full-width middle band, extended to overlap both end caps vertically.
        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x, y + r - o, w, (h - 2.0 * (r - o)).max(0.0)),
            color,
        )?;
        // Top + bottom caps, widened to overlap the corner discs horizontally.
        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x + r - o, y, (w - 2.0 * (r - o)).max(0.0), r),
            color,
        )?;
        self.filled_rectangle(
            layers,
            layer_num,
            euclid::rect(x + r - o, y + h - r, (w - 2.0 * (r - o)).max(0.0), r),
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

    fn pane_row(pane_id: PaneId) -> SidebarRow {
        SidebarRow::Pane {
            pane_id,
            active: false,
            label: format!("pane {pane_id}"),
            is_remote: false,
        }
    }

    fn tab_input(tab_idx: usize, title: &str, pane_ids: &[PaneId]) -> SidebarTabInput {
        SidebarTabInput {
            tab_idx,
            active: false,
            title: title.to_string(),
            metadata: Vec::new(),
            panes: pane_ids.iter().copied().map(pane_row).collect(),
        }
    }

    fn expanded(tabs: &[usize]) -> HashSet<usize> {
        tabs.iter().copied().collect()
    }

    fn pane_ids(rows: &[SidebarRow]) -> Vec<PaneId> {
        rows.iter()
            .filter_map(|row| match row {
                SidebarRow::Pane { pane_id, .. } => Some(*pane_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn vendor_session_conversion_preserves_identity_and_project() {
        let session = VendorSession {
            pid: 42,
            vendor: AgentVendor::Codex,
            session_id: "session-42".to_string(),
            cwd: PathBuf::from("/tmp/project/src"),
            project_root: Some(PathBuf::from("/tmp/project")),
            name: Some("build agent".to_string()),
            status: HerdStatus::Blocked,
            blocked_reason: Some("permission".to_string()),
            started_at: None,
            status_changed_at: None,
            subagents: Vec::new(),
        };

        let agent = herd_agent_from_vendor_session(&session);
        assert_eq!(agent.name, "build agent");
        assert_eq!(agent.provider, "codex");
        assert_eq!(agent.vendor, AgentVendor::Codex);
        assert_eq!(agent.status, HerdStatus::Blocked);
        assert_eq!(agent.session_id.as_deref(), Some("session-42"));
        assert_eq!(agent.project_root, Some(PathBuf::from("/tmp/project")));
    }

    #[test]
    fn single_pane_tabs_have_no_pane_rows() {
        // Nothing to expand, so the list looks exactly as it did before pane
        // rows existed even if the tab somehow ends up in the expanded set.
        let rows = assemble_sidebar_rows(vec![tab_input(0, "shell", &[1])], &expanded(&[0]), None);

        assert_eq!(rows.len(), 1);
        assert!(pane_ids(&rows).is_empty());
        match &rows[0] {
            SidebarRow::Tab {
                pane_count,
                expanded,
                ..
            } => {
                assert_eq!(*pane_count, 1);
                assert!(!expanded);
            }
            other => panic!("expected a tab row, got {:?}", other),
        }
    }

    #[test]
    fn split_tabs_emit_pane_rows_only_when_expanded() {
        let tabs = || vec![tab_input(0, "shell", &[1, 2])];

        let collapsed = assemble_sidebar_rows(tabs(), &expanded(&[]), None);
        assert_eq!(collapsed.len(), 1);
        assert!(pane_ids(&collapsed).is_empty());

        let open = assemble_sidebar_rows(tabs(), &expanded(&[0]), None);
        assert_eq!(open.len(), 3);
        assert_eq!(pane_ids(&open), vec![1, 2]);
    }

    #[test]
    fn pane_rows_follow_their_own_tab() {
        // Ordering matters: a pane row belongs directly under its tab, or the
        // indentation would attach it to the wrong parent on screen.
        let rows = assemble_sidebar_rows(
            vec![
                tab_input(0, "first", &[1, 2]),
                tab_input(1, "second", &[3, 4]),
            ],
            &expanded(&[1]),
            None,
        );

        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], SidebarRow::Tab { tab_idx: 0, .. }));
        assert!(matches!(rows[1], SidebarRow::Tab { tab_idx: 1, .. }));
        assert_eq!(pane_ids(&rows), vec![3, 4]);
    }

    #[test]
    fn search_keeps_the_pane_children_of_matching_tabs() {
        let rows = assemble_sidebar_rows(
            vec![
                tab_input(0, "editor", &[1, 2]),
                tab_input(1, "logs", &[3, 4]),
            ],
            &expanded(&[0, 1]),
            Some("edit"),
        );

        // Only the matching tab survives, and it brings its panes with it.
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], SidebarRow::Tab { tab_idx: 0, .. }));
        assert_eq!(pane_ids(&rows), vec![1, 2]);
    }

    #[test]
    fn an_empty_query_filters_nothing() {
        let rows = assemble_sidebar_rows(
            vec![tab_input(0, "editor", &[1]), tab_input(1, "logs", &[2])],
            &expanded(&[]),
            Some(""),
        );

        assert_eq!(rows.len(), 2);
    }

    // Placement policy itself (target resolution, tiling, clamping, the Alt
    // inversion) moved to `agent_launch` and is tested there — see
    // `termwindow::agent_launch::tests`. What's left here is that
    // `agent_launch_placement` wires the config fields through correctly.

    #[test]
    fn agent_launch_target_gained_a_zoomed_variant() {
        // Guards the sidebar's config-to-mux-enum mapping in
        // `agent_launch_placement`: `AgentSplitDirection` still only maps to
        // `SplitDirection::{Horizontal,Vertical}`, independent of the launch
        // target itself.
        assert_eq!(
            match AgentSplitDirection::Horizontal {
                AgentSplitDirection::Horizontal => SplitDirection::Horizontal,
                AgentSplitDirection::Vertical => SplitDirection::Vertical,
            },
            SplitDirection::Horizontal
        );
        assert_eq!(
            match AgentSplitDirection::Vertical {
                AgentSplitDirection::Horizontal => SplitDirection::Horizontal,
                AgentSplitDirection::Vertical => SplitDirection::Vertical,
            },
            SplitDirection::Vertical
        );
        // AgentLaunchTarget::Zoomed exists and is distinct from the other two.
        assert_ne!(AgentLaunchTarget::Zoomed, AgentLaunchTarget::SplitPane);
        assert_ne!(AgentLaunchTarget::Zoomed, AgentLaunchTarget::NewTab);
    }

    #[test]
    fn force_local_only_applies_to_remote_panes() {
        assert!(agent_launch_forced_local(
            AgentRemoteBehavior::ForceLocal,
            true
        ));
        assert!(!agent_launch_forced_local(
            AgentRemoteBehavior::ForceLocal,
            false
        ));
    }

    #[test]
    fn follow_pane_never_forces_local() {
        // The opt-out exists for agents installed on the far side of the ssh
        // connection; it must hold even when the pane is plainly remote.
        assert!(!agent_launch_forced_local(
            AgentRemoteBehavior::FollowPane,
            true
        ));
    }

    /// Unique scratch directory for the project-root walk tests. Avoids a
    /// tempfile dev-dependency for four tests.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join("tgz-launcher-tests").join(format!(
            "{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        // Resolve symlinks (/var -> /private/var on macOS) so the paths the
        // walk returns compare equal to the ones the test built.
        dir.canonicalize().unwrap_or(dir)
    }

    fn markers() -> Vec<String> {
        config::default_project_markers()
    }

    #[test]
    fn command_resolution_returns_an_absolute_path() {
        // An explicit path resolves to itself; a name is looked up. /bin/sh
        // exists on every supported unix and lives on the launchd-minimal PATH
        // a GUI-launched bundle inherits.
        #[cfg(unix)]
        {
            assert_eq!(
                resolve_command_path("/bin/sh"),
                Some(PathBuf::from("/bin/sh"))
            );
            // Not compared to a fixed path: distros ship /usr/bin/sh too, and
            // PATH order decides which one wins.
            let resolved = resolve_command_path("sh").expect("sh is installed");
            assert!(resolved.is_absolute(), "{resolved:?} is not absolute");
            assert_eq!(resolved.file_name().and_then(|n| n.to_str()), Some("sh"));
        }
        assert_eq!(resolve_command_path("  "), None);
        assert_eq!(
            resolve_command_path("tgz-definitely-not-an-installed-agent"),
            None
        );
    }

    #[test]
    fn fallback_dirs_cover_the_claude_code_install_location() {
        // Claude Code installs to ~/.local/bin, which launchd's PATH omits, so
        // the launcher button depends on this fallback list.
        let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let dirs = fallback_command_dirs();
        assert!(dirs.contains(&home.join(".local/bin")));
        assert!(dirs.contains(&home.join(".claude/local")));
    }

    #[test]
    fn project_root_found_in_a_parent_directory() {
        let root = scratch_dir("parent");
        fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("crate/src/inner");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(nearest_project_root(&deep, &markers()), Some(root.clone()));
        // A directory that is itself the root resolves to itself.
        assert_eq!(nearest_project_root(&root, &markers()), Some(root.clone()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_recognizes_non_git_markers() {
        let root = scratch_dir("svn");
        fs::create_dir_all(root.join(".svn")).unwrap();
        let deep = root.join("a/b");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(nearest_project_root(&deep, &markers()), Some(root.clone()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_returns_none_outside_a_project() {
        // The scratch tree has no marker anywhere, and the walk stops at the
        // filesystem root rather than looping.
        let root = scratch_dir("bare");
        let deep = root.join("x/y");
        fs::create_dir_all(&deep).unwrap();

        assert_eq!(nearest_project_root(&deep, &markers()), None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_ignores_empty_and_path_shaped_markers() {
        let root = scratch_dir("bad-markers");
        fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a");
        fs::create_dir_all(&deep).unwrap();

        // A path-shaped marker must not be honored: it would let a config
        // typo match outside the directory being probed.
        let bad = vec!["".to_string(), "  ".to_string(), "../.git".to_string()];
        assert_eq!(nearest_project_root(&deep, &bad), None);
        // Empty marker list short-circuits.
        assert_eq!(nearest_project_root(&deep, &[]), None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_root_walk_crosses_into_a_distro_view_and_back() {
        // Mirrors `project_root_for` for a WSL source: the marker walk runs
        // against the UNC view because Windows cannot stat a Linux path, and
        // the result is mapped back so the spawn gets a path the distro
        // understands. Exercised here through the pure path helpers, since a
        // real \\wsl.localhost share does not exist on this host.
        let probe = wsl_paths::wsl_to_windows("/home/tim/proj/src", "Ubuntu").unwrap();
        assert_eq!(
            probe,
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\tim\proj\src")
        );

        let found_root = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\tim\proj");
        assert_eq!(
            wsl_paths::windows_to_wsl(&found_root.to_string_lossy(), "Ubuntu"),
            Some("/home/tim/proj".to_string())
        );
    }

    #[test]
    fn discovered_shells_are_executable_and_unique() {
        // Runs against whatever this machine actually has; the invariants
        // hold regardless of which shells are installed.
        let shells = discovered_shells();
        let mut seen = HashSet::new();
        for (label, argv) in &shells {
            assert!(!label.is_empty(), "shell entry has a blank label");
            assert_eq!(argv.len(), 1, "{} should be a bare program", label);
            assert!(
                seen.insert(argv.clone()),
                "{} was discovered twice",
                argv[0]
            );
            assert!(
                path_is_executable(Path::new(&argv[0])) || command_exists_on_path(&argv[0]),
                "{} is not executable",
                argv[0]
            );
        }
    }

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
    fn text_cols_never_admits_a_partly_visible_glyph() {
        // render_screen_line does not clip: a glyph starting inside the region
        // is painted whole and overhangs, so only fully-fitting cells count.
        assert_eq!(sidebar_text_cols(27., 14), 1);
        assert_eq!(sidebar_text_cols(28., 14), 2);
        assert_eq!(sidebar_text_cols(13., 14), 0);
        assert_eq!(sidebar_text_cols(0., 14), 0);
        assert_eq!(sidebar_text_cols(-5., 14), 0);
        assert_eq!(sidebar_text_cols(f32::NAN, 14), 0);
        // A zero cell width must not divide by zero.
        assert_eq!(sidebar_text_cols(100., 0), 0);
    }

    #[test]
    fn truncate_to_cols_never_splits_a_wide_grapheme() {
        assert_eq!(truncate_to_cols("日本語", 3), "日");
        assert_eq!(truncate_to_cols("日本語", 4), "日本");
        assert_eq!(truncate_to_cols("日本語", 1), "");
        assert_eq!(truncate_to_cols("Worktree", 4), "Work");
        assert_eq!(truncate_to_cols("Worktree", 0), "");
        // A combining mark stays with the base character it modifies.
        assert_eq!(truncate_to_cols("e\u{301}x", 1), "e\u{301}");
    }

    #[test]
    fn truncate_from_end_keeps_the_caret_visible() {
        // The search field renders "{query}|"; head-truncating it would hide the
        // caret the user is typing at.
        assert_eq!(truncate_to_cols_from_end("a long query|", 6), "query|");
        assert_eq!(truncate_to_cols_from_end("abc", 0), "");
    }

    #[test]
    fn fit_label_picks_the_widest_variant_that_fits() {
        assert_eq!(fit_label(&WORKTREE_LABELS, 9), Some("Worktree"));
        assert_eq!(fit_label(&WORKTREE_LABELS, 8), Some("Worktree"));
        assert_eq!(fit_label(&WORKTREE_LABELS, 7), Some("Tree"));
        assert_eq!(fit_label(&WORKTREE_LABELS, 3), Some("Wt"));
        assert_eq!(fit_label(&WORKTREE_LABELS, 1), None);
        assert_eq!(fit_label(&WORKTREE_LABELS, 0), None);
    }

    /// Content geometry for a Left sidebar of `width` with the scrollbar gutter
    /// shown, mirroring `paint_sidebar`.
    fn bottom_row_for_width(width: f32, cell_height: f32) -> SidebarBottomRowLayout {
        let item_x = INSET;
        let item_w = width - INSET * 2.;
        let content_w = item_w - RESIZE_GRIP_W as f32 - SIDEBAR_SCROLLBAR_GUTTER_W;
        sidebar_bottom_row_layout(
            item_x,
            item_w,
            item_x,
            content_w,
            sidebar_status_dot_size(cell_height),
            true,
        )
    }

    #[test]
    fn worktree_and_agent_labels_fit_at_the_default_width() {
        // Pins the arithmetic behind the default `sidebar_width_px`: at a 14px
        // cell on a 2x display both full words must fit their halves. If this
        // fails, the default width and the label ladder have drifted apart.
        let cell_width = 14usize;
        let layout = bottom_row_for_width(
            config::Config::default_config().sidebar_width_px as f32,
            32.,
        );
        assert_eq!(
            fit_label(
                &WORKTREE_LABELS,
                sidebar_text_cols(layout.worktree_text_w, cell_width)
            ),
            Some("Worktree")
        );
        let claude = default_agent_adapters().remove("claude").unwrap();
        let label = claude.label.clone().unwrap();
        let rungs = [label.as_str(), "Cl"];
        assert_eq!(
            fit_label(&rungs, sidebar_text_cols(layout.agent_text_w, cell_width)),
            Some("Claude")
        );
    }

    #[test]
    fn bottom_row_labels_step_down_on_a_one_x_display() {
        // 1x halves the effective width, and the fixed paddings do not scale, so
        // the halves are genuinely tight — the ladder is what keeps them legible
        // instead of clipping mid-word.
        let layout = bottom_row_for_width(200., 16.);
        let cell_width = 7usize;
        assert!(fit_label(
            &WORKTREE_LABELS,
            sidebar_text_cols(layout.worktree_text_w, cell_width)
        )
        .is_some());
        assert!(fit_label(
            &["Claude", "Cl"],
            sidebar_text_cols(layout.agent_text_w, cell_width)
        )
        .is_some());
    }

    #[test]
    fn bottom_row_halves_never_overlap() {
        let mut width = 180.;
        while width <= 800. {
            for content_offset in [0., 36.] {
                let item_x = INSET;
                let item_w = width - INSET * 2.;
                let content_x = item_x + content_offset;
                let content_w = item_w - content_offset - RESIZE_GRIP_W as f32;
                let layout =
                    sidebar_bottom_row_layout(item_x, item_w, content_x, content_w, 10., true);
                assert!(
                    layout.worktree_fill_x + layout.worktree_fill_w <= layout.agent_fill_x + 0.001,
                    "fills overlap at width {width}"
                );
                assert!(
                    layout.worktree_text_x + layout.worktree_text_w <= layout.agent_fill_x + 0.001,
                    "worktree text runs into the agent half at width {width}"
                );
                assert!(
                    layout.agent_text_x + layout.agent_text_w <= item_x + item_w + 0.001,
                    "agent text runs past the row at width {width}"
                );
                assert!(layout.agent_dot_x >= layout.agent_fill_x);
            }
            width += 4.;
        }
    }

    #[test]
    fn bottom_row_gives_worktree_everything_without_an_agent() {
        let layout = sidebar_bottom_row_layout(8., 264., 8., 228., 10., false);
        assert_eq!(layout.worktree_fill_x, 8.);
        assert_eq!(layout.worktree_fill_w, 264.);
        assert_eq!(layout.agent_fill_w, 0.);
        assert!(layout.worktree_text_w > 0.);
    }

    #[test]
    fn bottom_row_fills_reach_the_row_edges() {
        let layout = sidebar_bottom_row_layout(8., 384., 8., 348., 10., true);
        assert_eq!(layout.worktree_fill_x, 8.);
        assert_eq!(layout.agent_fill_x + layout.agent_fill_w, 8. + 384.);
    }

    #[test]
    fn tab_row_title_starts_after_the_chevron_and_the_agent_badge() {
        // The reported collision: the title (which carries the leading "N: "
        // index) was drawn at the chevron's own x, so the number landed on top
        // of the chevron and the status dot.
        let cols = sidebar_row_columns(100., 200., 14., 32., true, true);
        assert_eq!(cols.chevron_x, 100.);
        assert_eq!(cols.text_x, 100. + cols.chevron_w + cols.badge_w);
        assert!(cols.text_x > cols.badge_x + cols.badge_w - 0.001);
    }

    #[test]
    fn tab_row_decorations_never_overlap() {
        for (chevron, badge) in [(false, false), (true, false), (false, true), (true, true)] {
            for cell_height in [15., 24., 32., 48.] {
                let cols = sidebar_row_columns(40., 180., 14., cell_height, chevron, badge);
                if chevron {
                    assert!(cols.chevron_x + cols.chevron_w <= cols.badge_x + 0.001);
                }
                if badge {
                    assert!(cols.badge_x + cols.badge_w <= cols.text_x + 0.001);
                }
                // Origin and width always agree: the text ends where the row's
                // label region ends, whatever precedes it.
                assert!((cols.text_x + cols.text_w - (40. + 180.)).abs() < 0.001);
            }
        }
    }

    #[test]
    fn tab_row_text_width_clamps_to_zero_instead_of_negative() {
        let cols = sidebar_row_columns(0., 4., 14., 32., true, true);
        assert_eq!(cols.text_w, 0.);
        assert_eq!(sidebar_text_cols(cols.text_w, 14), 0);
    }

    #[test]
    fn close_text_reserve_clears_the_close_glyph() {
        for cell_width in [7., 14., 20.] {
            let reserve = sidebar_close_text_reserve(cell_width);
            assert!(
                reserve >= (cell_width + CLOSE_GLYPH_INSET).min(CLOSE_ZONE_W),
                "reserve {reserve} must clear the glyph at cell width {cell_width}"
            );
            assert!(reserve <= CLOSE_ZONE_W);
        }
    }

    #[test]
    fn agent_badge_is_wider_than_its_dot() {
        for cell_height in [15., 24., 32., 48.] {
            assert!(sidebar_agent_badge_w(cell_height) > sidebar_status_dot_size(cell_height));
        }
        // The dot is clamped so it stays a dot at any font size.
        assert_eq!(sidebar_status_dot_size(4.), 5.);
        assert_eq!(sidebar_status_dot_size(200.), 10.);
    }

    #[test]
    fn row_text_offsets_keep_both_lines_inside_the_row() {
        // Heights as `sidebar_row_height` computes them, since that is what the
        // offsets have to fit inside. Metadata rows are only reachable under
        // Comfortable density, whose height always has room for two lines; the
        // single-line densities are checked with metadata off, which is the only
        // way they occur.
        for cell_height in [15f32, 24., 32.] {
            let comfortable_two_line = (cell_height * 2. + 8.).max(44.);
            let comfortable = (cell_height + 10.).max(34.);
            let compact = (cell_height + 6.).max(28.);

            let (primary, metadata) =
                sidebar_row_text_offsets(comfortable_two_line, cell_height, true);
            assert!(primary >= -0.001, "primary line above the row");
            assert!(
                metadata.unwrap() + cell_height <= comfortable_two_line + 0.001,
                "metadata line overflows a {comfortable_two_line}px row at cell {cell_height}"
            );

            for row_height in [comfortable_two_line, comfortable, compact] {
                let (primary, metadata) = sidebar_row_text_offsets(row_height, cell_height, false);
                assert!(primary >= -0.001);
                assert!(
                    primary + cell_height <= row_height + 0.001,
                    "title overflows a {row_height}px row at cell {cell_height}"
                );
                assert!(metadata.is_none());
            }
        }
    }

    #[test]
    fn sidebar_width_scale_is_one_at_retina_and_half_at_one_x() {
        #[cfg(target_os = "macos")]
        {
            assert_eq!(sidebar_width_scale_for_dpi(144.), 1.0);
            assert_eq!(sidebar_width_scale_for_dpi(72.), 0.5);
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(sidebar_width_scale_for_dpi(192.), 1.0);
            assert_eq!(sidebar_width_scale_for_dpi(96.), 0.5);
        }
        // Degenerate DPIs stay inside the clamp.
        assert_eq!(sidebar_width_scale_for_dpi(0.), 0.5);
        assert_eq!(sidebar_width_scale_for_dpi(100_000.), 1.25);
    }

    #[test]
    fn pulse_phase_is_a_smooth_closed_loop() {
        let period = Duration::from_millis(1600);
        assert!(agent_pulse_phase(Duration::ZERO, period) < 0.001);
        assert!((agent_pulse_phase(Duration::from_millis(800), period) - 1.0).abs() < 0.001);
        assert!(agent_pulse_phase(period, period) < 0.001);
        let mut previous = -1.0;
        for step in 0..=8 {
            let phase = agent_pulse_phase(Duration::from_millis(step * 100), period);
            assert!((0.0..=1.0).contains(&phase), "phase {phase} out of range");
            assert!(phase >= previous - 0.001, "not monotonic on the way up");
            previous = phase;
        }
        // A degenerate period must not divide by zero.
        assert_eq!(
            agent_pulse_phase(Duration::from_millis(5), Duration::ZERO),
            0.
        );
    }

    #[test]
    fn pulse_only_dims_never_brightens() {
        let base = LinearRgba(0.8, 0.5, 0.3, 1.0);
        let surface = LinearRgba(0.1, 0.1, 0.1, 1.0);
        assert_eq!(
            agent_status_dot_accent(&AgentStatus::Idle, base, surface, None),
            base
        );
        // Full phase is exactly the static colour, so turning the pulse on never
        // makes a dot brighter than it was.
        let full = agent_status_dot_accent(&AgentStatus::Running, base, surface, Some(1.0));
        assert!((full.0 - base.0).abs() < 0.001);
        let dim = agent_status_dot_accent(&AgentStatus::Running, base, surface, Some(0.0));
        assert!(dim.0 < base.0);
        assert!(dim.0 > surface.0);
    }

    #[test]
    fn waiting_dot_keeps_its_attention_color_while_pulsing() {
        let base = LinearRgba(0.2, 0.4, 0.9, 1.0);
        let surface = LinearRgba(0.1, 0.1, 0.1, 1.0);
        let pulsed =
            agent_status_dot_accent(&AgentStatus::WaitingForInput, base, surface, Some(1.0));
        assert!(
            pulsed.0 > pulsed.2,
            "waiting must stay in the orange family, not revert to the adapter accent"
        );
    }

    #[test]
    fn rail_icon_side_fits_normal_cell_height() {
        // Rail width and font size both modest enough that neither the
        // ceiling nor the cell_height floor kicks in: just the usual
        // margin-trimmed size (width - 8).
        assert_eq!(sidebar_rail_icon_side(40., 20., 1.), 32.);
    }

    #[test]
    fn rail_icon_side_floors_at_cell_height_on_narrow_rail() {
        // Regression for the "l" ascender in the Claude "Cl" badge poking
        // past the rounded pill: a narrow collapsed rail used to shrink the
        // icon box below the glyph's line height.
        let width = 44.;
        let cell_height = 40.;
        let side = sidebar_rail_icon_side(width, cell_height, 1.);
        assert!(
            side >= cell_height,
            "rail icon side {side} must be >= cell_height {cell_height}"
        );
    }

    #[test]
    fn rail_icon_side_never_exceeds_rail_width() {
        // The cell_height floor must not push the box wider than the strip
        // itself, even in an extreme narrow-rail-plus-huge-font case.
        let width = 20.;
        let side = sidebar_rail_icon_side(width, 200., 1.);
        assert!(
            side <= width,
            "rail icon side {side} must fit within rail width {width}"
        );
    }

    #[test]
    fn rail_icon_side_respects_dpi_scaled_ceiling() {
        // Generous rail width shouldn't let the icon grow past the
        // DPI-scaled ceiling.
        let side = sidebar_rail_icon_side(500., 20., 2.);
        assert_eq!(side, 46. * 2.);
    }

    #[test]
    fn compact_tab_symbol_prefers_agent_kind_over_title() {
        assert_eq!(
            compact_tab_symbol(
                "1: bash",
                0,
                Some(&AgentKind::Claude),
                None,
                Some("bash"),
                None
            ),
            "Cl"
        );
        assert_eq!(
            compact_tab_symbol(
                "1: node",
                0,
                Some(&AgentKind::Codex),
                None,
                Some("node"),
                None
            ),
            "Cx"
        );
    }

    #[test]
    fn compact_tab_symbol_detects_worktree_from_title_or_pane_title() {
        assert_eq!(
            compact_tab_symbol("Worktree: foo", 0, None, None, None, None),
            "Wt"
        );
        assert_eq!(
            compact_tab_symbol("1: bash", 0, None, None, Some("bash"), Some("worktree foo")),
            "Wt"
        );
    }

    #[test]
    fn compact_tab_symbol_falls_back_to_known_commands() {
        assert_eq!(
            compact_tab_symbol("1: zsh", 0, None, None, Some("zsh"), None),
            "$"
        );
        assert_eq!(
            compact_tab_symbol("1: vim", 0, None, None, Some("vim"), None),
            "Vi"
        );
        assert_eq!(
            compact_tab_symbol("1: cargo", 0, None, None, Some("cargo"), None),
            "Rs"
        );
    }

    #[test]
    fn compact_tab_color_prefers_agent_kind_over_command() {
        assert_eq!(
            compact_tab_color(
                "1: bash",
                0,
                Some(&AgentKind::Claude),
                None,
                Some("bash"),
                None
            ),
            adapter_color(None, &AgentKind::Claude)
        );
    }

    #[test]
    fn compact_tab_color_detects_worktree_before_command_lookup() {
        assert_eq!(
            compact_tab_color("Worktree: foo", 0, None, None, Some("git"), None),
            LinearRgba(0.50, 0.58, 0.42, 1.0)
        );
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
    fn control_actions_require_both_opt_in_and_trusted_evidence() {
        // The gate is an AND. It was an OR, so trusted evidence alone unlocked
        // process-spawning actions with `enable_control_actions = false` —
        // looser than the documented model and than the config comment promises.
        let vars = HashMap::new();

        assert!(!agent_control_actions_allowed(false, false, &vars));
        assert!(
            !agent_control_actions_allowed(false, true, &vars),
            "trusted evidence alone must not unlock control actions"
        );
        assert!(
            !agent_control_actions_allowed(true, false, &vars),
            "opting in must not unlock control actions on untrusted evidence"
        );
        assert!(agent_control_actions_allowed(true, true, &vars));
    }

    #[test]
    fn explicit_user_var_opts_a_single_pane_in() {
        let mut vars = HashMap::new();
        vars.insert(
            "agent.enable_control_actions".to_string(),
            "true".to_string(),
        );

        // The var is the per-pane opt-in, standing in for the config key. It is
        // not evidence, so it cannot satisfy both halves of the gate by itself.
        assert!(!agent_control_actions_allowed(false, false, &vars));
        assert!(agent_control_actions_allowed(false, true, &vars));
    }

    #[test]
    fn visible_chrome_trust_follows_the_config_switch() {
        assert!(AgentEvidence::Process.is_trusted(false));
        assert!(AgentEvidence::UserVar.is_trusted(false));
        assert!(!AgentEvidence::Metadata.is_trusted(true));
        assert!(!AgentEvidence::VisibleChrome.is_trusted(false));
        assert!(AgentEvidence::VisibleChrome.is_trusted(true));
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
            evidence: AgentEvidence::Process,
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
            infer_agent_status_from_visible_text("Thinking\n✻ Brewing", None),
            AgentStatus::Running
        );
        assert_eq!(
            infer_agent_status_from_visible_text("Done\n\n❯", None),
            AgentStatus::WaitingForInput
        );
        assert_eq!(
            infer_agent_status_from_visible_text("Here is the answer.", None),
            AgentStatus::Unknown
        );
    }

    #[test]
    fn process_detected_agent_loads_visible_text_for_status() {
        // Strong evidence still loads the text when the pane publishes no
        // status, because status inference needs it.
        assert!(should_load_visible_agent_text(
            Some(AgentEvidence::Process),
            true,
            None
        ));
        assert!(!should_load_visible_agent_text(
            Some(AgentEvidence::Process),
            true,
            Some("running")
        ));
        assert!(should_load_visible_agent_text(None, true, None));
        assert!(!should_load_visible_agent_text(
            Some(AgentEvidence::Process),
            false,
            None
        ));
    }

    #[test]
    fn weak_title_evidence_still_loads_visible_text_for_arbitration() {
        // A bare title token must not settle identity on its own: the visible
        // text is loaded so the running agent's chrome can outvote it, even
        // though the pane already published a status.
        assert!(should_load_visible_agent_text(
            Some(AgentEvidence::TitleToken),
            true,
            Some("running")
        ));
        assert!(should_load_visible_agent_text(
            Some(AgentEvidence::TitlePhrase),
            true,
            Some("running")
        ));
        assert!(!should_load_visible_agent_text(
            Some(AgentEvidence::UserVar),
            true,
            Some("running")
        ));
    }

    fn detection_key(process: Option<&str>, title: &str) -> AgentDetectionCacheKey {
        AgentDetectionCacheKey {
            foreground_process: process.map(ToString::to_string),
            pane_title: title.to_string(),
            relevant_user_vars: Vec::new(),
            viewport_top: 0,
            viewport_rows: 40,
            visible_fingerprint: 0,
        }
    }

    fn detection_entry(
        key: AgentDetectionCacheKey,
        state: Option<AgentPaneState>,
    ) -> AgentDetectionCacheEntry {
        AgentDetectionCacheEntry {
            key,
            state,
            last_wait_notification: None,
            detected_at: Instant::now(),
            status_at: Instant::now(),
            pending_switch: None,
        }
    }

    fn aged_detection_entry(
        key: AgentDetectionCacheKey,
        state: Option<AgentPaneState>,
        age: Duration,
    ) -> AgentDetectionCacheEntry {
        let detected_at = Instant::now() - age;
        AgentDetectionCacheEntry {
            key,
            state,
            last_wait_notification: None,
            detected_at,
            status_at: detected_at,
            pending_switch: None,
        }
    }

    fn claude_state() -> AgentPaneState {
        claude_state_with_evidence(AgentEvidence::Process)
    }

    fn claude_state_with_evidence(evidence: AgentEvidence) -> AgentPaneState {
        AgentPaneState {
            adapter_id: Some("claude".to_string()),
            kind: AgentKind::Claude,
            evidence,
            trusted_controls: true,
            status: AgentStatus::Running,
            model: None,
            session_id: None,
            attach_url: None,
            cwd: None,
            input_tokens: None,
            output_tokens: None,
            cost: None,
            actions: AgentActions::default(),
        }
    }

    #[test]
    fn sticky_identity_survives_agent_retitling_its_pane() {
        let previous = detection_entry(
            detection_key(Some("claude"), "claude"),
            Some(claude_state()),
        );
        let sticky = sticky_agent_identity(
            Some(&previous),
            &detection_key(Some("claude"), "✳ fixing the sidebar badge"),
            Instant::now(),
            true,
        )
        .expect("identity should stick across a title rewrite");
        assert_eq!(sticky.adapter_id.as_deref(), Some("claude"));
        assert_eq!(sticky.kind, AgentKind::Claude);
        assert!(sticky.trusted_controls);
    }

    #[test]
    fn sticky_identity_drops_when_the_agent_process_exits() {
        let previous = detection_entry(
            detection_key(Some("claude"), "claude"),
            Some(claude_state()),
        );
        assert!(sticky_agent_identity(
            Some(&previous),
            &detection_key(Some("zsh"), "claude"),
            Instant::now(),
            true
        )
        .is_none());
    }

    #[test]
    fn sticky_title_identity_expires_when_the_title_changes() {
        // A title-derived identity is only ever as good as the title it came
        // from, whatever the process is — including a non-shell one like `node`,
        // which previously bypassed this guard entirely and let one bad frame
        // pin a wrong badge for the life of the process.
        for process in [None, Some("zsh"), Some("node")] {
            let previous = detection_entry(
                detection_key(process, "claude"),
                Some(claude_state_with_evidence(AgentEvidence::TitleToken)),
            );
            assert!(sticky_agent_identity(
                Some(&previous),
                &detection_key(process, "claude"),
                Instant::now(),
                true
            )
            .is_some());
            assert!(sticky_agent_identity(
                Some(&previous),
                &detection_key(process, "~/Documents/tgzterminal"),
                Instant::now(),
                true
            )
            .is_none());
        }
    }

    #[test]
    fn sticky_chrome_identity_survives_a_title_rewrite() {
        // The flapping report: Claude retitles itself every turn, and an
        // identity earned from its on-screen chrome must not be dropped by that.
        let previous = detection_entry(
            detection_key(Some("node"), "Update readme"),
            Some(claude_state_with_evidence(AgentEvidence::VisibleChrome)),
        );
        let sticky = sticky_agent_identity(
            Some(&previous),
            &detection_key(Some("node"), "fix codex import"),
            Instant::now(),
            true,
        )
        .expect("chrome-derived identity should survive a retitle");
        assert_eq!(sticky.adapter_id.as_deref(), Some("claude"));
    }

    #[test]
    fn sticky_visible_identity_expires_after_its_ttl() {
        let key = detection_key(Some("node"), "Update readme");
        let fresh = aged_detection_entry(
            key.clone(),
            Some(claude_state_with_evidence(AgentEvidence::VisibleChrome)),
            Duration::from_secs(5),
        );
        assert!(sticky_agent_identity(Some(&fresh), &key, Instant::now(), true).is_some());

        let stale = aged_detection_entry(
            key.clone(),
            Some(claude_state_with_evidence(AgentEvidence::VisibleChrome)),
            AGENT_STICKY_VISIBLE_TTL + Duration::from_secs(1),
        );
        assert!(sticky_agent_identity(Some(&stale), &key, Instant::now(), true).is_none());
    }

    #[test]
    fn sticky_never_carries_trust_from_untrusted_evidence() {
        let key = detection_key(Some("node"), "Update readme");
        let previous = detection_entry(
            key.clone(),
            Some(claude_state_with_evidence(AgentEvidence::VisibleChrome)),
        );
        let sticky = sticky_agent_identity(Some(&previous), &key, Instant::now(), false)
            .expect("identity still sticks");
        assert!(
            !sticky.trusted_controls,
            "trust must not outlive the class that granted it"
        );
    }

    #[test]
    fn sticky_identity_needs_a_previously_detected_agent() {
        let previous = detection_entry(detection_key(Some("claude"), "claude"), None);
        assert!(sticky_agent_identity(
            Some(&previous),
            &detection_key(Some("claude"), "other"),
            Instant::now(),
            true
        )
        .is_none());
        assert!(sticky_agent_identity(
            None,
            &detection_key(Some("claude"), "claude"),
            Instant::now(),
            true
        )
        .is_none());
    }

    #[test]
    fn sticky_identity_drops_when_agent_user_vars_change() {
        let previous = detection_entry(
            detection_key(Some("claude"), "claude"),
            Some(claude_state()),
        );
        let mut key = detection_key(Some("claude"), "claude");
        key.relevant_user_vars = vec![("agent.kind".to_string(), "codex".to_string())];
        assert!(sticky_agent_identity(Some(&previous), &key, Instant::now(), true).is_none());
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
        trim_agent_toolbelt_buttons(&mut visible, 190.);

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
    fn word_boundary_matching_ignores_agent_patterns_inside_other_words() {
        // The reported false positive: an ssh/shell pane whose output merely
        // contains "example"/"sample" was detected as the Amp agent because its
        // visible pattern "amp" matched as a bare substring.
        assert!(!agent_word_pattern_matches_pre_lowered(
            "run the example script and read the sample output",
            "amp"
        ));
        assert!(!agent_word_pattern_matches_pre_lowered(
            "browsing the codexes in the archive",
            "codex"
        ));
        // Genuine whole-word / phrase matches still fire.
        assert!(agent_word_pattern_matches_pre_lowered(
            "welcome to claude code",
            "claude code"
        ));
        assert!(agent_word_pattern_matches_pre_lowered("amp v0.1", "amp"));
        assert!(agent_word_pattern_matches_pre_lowered(
            "starting codex.",
            "codex"
        ));
        // Regex patterns keep their exact semantics through this path.
        assert!(agent_word_pattern_matches_pre_lowered(
            "claude sonnet 5",
            "re:sonnet\\s+\\d+"
        ));
    }

    #[test]
    fn glue_bytes_block_short_tokens_in_paths_and_entities() {
        // Path separators, hyphens and entity punctuation glue a token to a
        // neighbour; treating them as plain word boundaries is why "amp" fired
        // on "&amp;" and "/opt/amp-tools".
        for haystack in [
            "echo &amp;amp; done",
            "ls /opt/amp-tools",
            "rated at 20 amp-hour capacity",
            "postage stamp",
        ] {
            assert!(
                !agent_word_pattern_matches_pre_lowered(haystack, "amp"),
                "{haystack:?} must not match \"amp\""
            );
        }
        // Trailing punctuation is still a boundary.
        for haystack in ["amp", "starting amp.", "(amp)", "amp:"] {
            assert!(
                agent_word_pattern_matches_pre_lowered(haystack, "amp"),
                "{haystack:?} must match \"amp\""
            );
        }
    }

    fn adapter_list() -> Vec<(String, AgentAdapterConfig)> {
        default_agent_adapters().into_iter().collect()
    }

    #[test]
    fn bare_brand_tokens_do_not_identify_an_agent_from_visible_text() {
        // An ordinary shell pane whose output happens to contain agent brand
        // words. Every one of these used to badge the pane.
        let text = "\
move the cursor to column 3\n\
see the openai docs for codex-style completions\n\
run the example script and read the sample output\n\
gemini is a constellation\n";
        let adapters = adapter_list();
        let ambiguous = ambiguous_agent_patterns(&adapters);
        assert!(
            visible_agent_candidates(text, &adapters, &ambiguous, 2).is_empty(),
            "brand words in ordinary output must not identify an agent"
        );
    }

    #[test]
    fn reading_this_repos_own_source_is_not_an_agent_pane() {
        // Literal fragments of this project's adapter table. Brand phrases alone
        // must not be enough, or `cat`/`grep` on config.rs badges the pane.
        let text = "\
        &[\"claude\", \"claude-code\", \"claude_code\"],\n\
        &[\"claude code\", \"claude\"],\n\
        &[\"claude code\", \"claude team\", \"welcome to claude\"],\n\
            \"auto mode\",\n\
            \"shift+tab\",\n\
            \"ctx:\",\n\
        &[\"codex\", \"openai-codex\", \"openai_codex\"],\n";
        let adapters = adapter_list();
        let ambiguous = ambiguous_agent_patterns(&adapters);
        assert!(
            visible_agent_candidates(text, &adapters, &ambiguous, 2).is_empty(),
            "reading the adapter table must not detect an agent"
        );
    }

    #[test]
    fn running_claude_chrome_identifies_claude() {
        // A running Claude frame with no banner left on screen: process is
        // `node`, title is the current task. Only its own chrome can name it.
        let text = "\
✳ Updating the sidebar renderer… (esc to interrupt)\n\
\n\
  ? for shortcuts                          ctx: 42%\n\
> \n";
        let adapters = adapter_list();
        let ambiguous = ambiguous_agent_patterns(&adapters);
        let candidates = visible_agent_candidates(text, &adapters, &ambiguous, 2);
        let resolved = resolve_agent_identity(candidates).expect("claude should be identified");
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));
        assert_eq!(resolved.evidence, AgentEvidence::VisibleChrome);
    }

    #[test]
    fn visible_identity_requires_chrome_not_just_brand_phrases() {
        let adapters = adapter_list();
        let ambiguous = ambiguous_agent_patterns(&adapters);
        let text = "welcome to claude code\nclaude team plan\n";
        assert!(visible_agent_candidates(text, &adapters, &ambiguous, 2).is_empty());
    }

    #[test]
    fn generic_running_markers_name_no_adapter_but_do_set_running() {
        // "esc to interrupt" is printed by several agents, so it cannot say
        // which one — but it still means something is working.
        let text = "thinking… (esc to interrupt)\n";
        let adapters = adapter_list();
        let ambiguous = ambiguous_agent_patterns(&adapters);
        assert!(
            ambiguous.contains("esc to interrupt"),
            "a marker claimed by several adapters must be ambiguous"
        );
        assert!(visible_agent_candidates(text, &adapters, &ambiguous, 2).is_empty());
        assert_eq!(
            infer_agent_status_from_visible_text(text, None),
            AgentStatus::Running
        );
    }

    fn candidate(id: &str, evidence: AgentEvidence, signals: u32) -> AgentIdentityCandidate {
        AgentIdentityCandidate {
            adapter_id: Some(id.to_string()),
            kind: AgentKind::from_adapter_id(id)
                .unwrap_or_else(|| AgentKind::Unknown(id.to_string())),
            evidence,
            signals,
        }
    }

    #[test]
    fn stronger_evidence_outranks_weaker_regardless_of_order() {
        let resolved = resolve_agent_identity(vec![
            candidate("amp", AgentEvidence::TitleToken, 1),
            candidate("claude", AgentEvidence::Process, 1),
        ])
        .unwrap();
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));

        let resolved = resolve_agent_identity(vec![
            candidate("claude", AgentEvidence::UserVar, 1),
            candidate("codex", AgentEvidence::Process, 1),
        ])
        .unwrap();
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));
    }

    #[test]
    fn title_token_loses_to_multi_signal_visible_chrome() {
        // The pane title is prose the user typed ("run the amp job"); the chrome
        // belongs to the agent actually running there.
        let resolved = resolve_agent_identity(vec![
            candidate("amp", AgentEvidence::TitleToken, 1),
            candidate("claude", AgentEvidence::VisibleChrome, 3),
        ])
        .unwrap();
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));
    }

    #[test]
    fn title_phrase_still_outranks_visible_chrome() {
        let resolved = resolve_agent_identity(vec![
            candidate("claude", AgentEvidence::TitlePhrase, 1),
            candidate("codex", AgentEvidence::VisibleChrome, 3),
        ])
        .unwrap();
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));
    }

    #[test]
    fn ambiguous_weak_evidence_is_not_resolved_alphabetically() {
        // Two equally weak candidates is exactly the false-positive shape, and
        // must not fall back to whichever adapter sorts first.
        let resolved = resolve_agent_identity(vec![
            candidate("amp", AgentEvidence::VisibleChrome, 1),
            candidate("codex", AgentEvidence::VisibleChrome, 1),
        ]);
        assert!(resolved.is_none());
    }

    #[test]
    fn ambiguous_strong_evidence_yields_an_unnamed_agent() {
        let resolved = resolve_agent_identity(vec![
            candidate("claude", AgentEvidence::Process, 1),
            candidate("codex", AgentEvidence::Process, 1),
        ])
        .expect("something is certainly there");
        assert_eq!(resolved.adapter_id, None);
    }

    #[test]
    fn more_agreeing_signals_win_inside_one_class() {
        let resolved = resolve_agent_identity(vec![
            candidate("codex", AgentEvidence::VisibleChrome, 2),
            candidate("claude", AgentEvidence::VisibleChrome, 4),
        ])
        .unwrap();
        assert_eq!(resolved.adapter_id.as_deref(), Some("claude"));
    }

    #[test]
    fn title_candidates_separate_phrases_from_bare_tokens() {
        let adapters = adapter_list();
        let phrase = title_agent_candidates("welcome to claude code", adapters.iter().cloned());
        assert!(phrase
            .iter()
            .any(|c| c.adapter_id.as_deref() == Some("claude")
                && c.evidence == AgentEvidence::TitlePhrase));

        let token = title_agent_candidates("amp", adapters.iter().cloned());
        assert!(token
            .iter()
            .any(|c| c.adapter_id.as_deref() == Some("amp")
                && c.evidence == AgentEvidence::TitleToken));

        // The substring match this path used before fired here.
        assert!(
            title_agent_candidates("run the example script", adapters.iter().cloned()).is_empty()
        );
    }

    #[test]
    fn identity_does_not_flip_on_a_single_disagreeing_frame() {
        // The flapping report: one frame claiming a different agent must not
        // change the badge.
        assert!(!agent_identity_switch_allowed(
            Some(("claude", AgentEvidence::VisibleChrome)),
            Some("codex"),
            AgentEvidence::VisibleChrome,
            1
        ));
    }

    #[test]
    fn identity_switches_after_two_agreeing_frames() {
        assert!(agent_identity_switch_allowed(
            Some(("claude", AgentEvidence::VisibleChrome)),
            Some("codex"),
            AgentEvidence::VisibleChrome,
            2
        ));
    }

    #[test]
    fn stronger_evidence_switches_identity_immediately() {
        assert!(agent_identity_switch_allowed(
            Some(("amp", AgentEvidence::TitleToken)),
            Some("claude"),
            AgentEvidence::Process,
            1
        ));
        // ...and a weaker class cannot, however many times it repeats within one
        // frame's worth of agreement.
        assert!(!agent_identity_switch_allowed(
            Some(("claude", AgentEvidence::Process)),
            Some("amp"),
            AgentEvidence::TitleToken,
            1
        ));
    }

    #[test]
    fn identity_switch_is_free_when_nothing_is_established() {
        assert!(agent_identity_switch_allowed(
            None,
            Some("claude"),
            AgentEvidence::VisibleChrome,
            1
        ));
        assert!(agent_identity_switch_allowed(
            Some(("claude", AgentEvidence::Process)),
            Some("claude"),
            AgentEvidence::VisibleChrome,
            1
        ));
    }

    #[test]
    fn status_running_marker_is_found_outside_the_last_twenty_lines() {
        // The exact Stop-button regression: the marker sits far from the end,
        // with the docked input strip and prompt box trailing it.
        let mut lines = vec!["✳ Working on the sidebar… (esc to interrupt)".to_string()];
        for idx in 0..60 {
            lines.push(format!("  edited file_{idx}.rs"));
        }
        lines.push("╭──────────────╮".to_string());
        lines.push("│ >            │".to_string());
        lines.push("╰──────────────╯".to_string());
        let text = lines.join("\n");
        assert_eq!(
            infer_agent_status_from_visible_text(&text, None),
            AgentStatus::Running
        );
    }

    #[test]
    fn status_uses_adapter_running_patterns() {
        let adapter = AgentAdapterConfig {
            running_patterns: vec!["crunching numbers".to_string()],
            ..Default::default()
        };
        let text = "crunching numbers\nplease hold\n";
        assert_eq!(
            infer_agent_status_from_visible_text(text, Some(&adapter)),
            AgentStatus::Running
        );
        assert_eq!(
            infer_agent_status_from_visible_text(text, None),
            AgentStatus::Unknown
        );
    }

    #[test]
    fn status_spinner_counts_above_the_prompt_box() {
        // Agents redraw the spinner and the prompt independently, so the spinner
        // is not reliably the final line.
        let text = "✻ Thinking\n\n> \n";
        assert_eq!(
            infer_agent_status_from_visible_text(text, None),
            AgentStatus::Running
        );
    }

    #[test]
    fn running_status_is_sticky_for_the_grace_period() {
        let now = Instant::now();
        let grace = Duration::from_secs(3);
        // A frame caught between spinner redraws reads Unknown; the badge and
        // the Stop button must not blink out.
        assert_eq!(
            stabilize_agent_status(
                AgentStatus::Unknown,
                Some(AgentStatus::Running),
                Some(now - Duration::from_millis(500)),
                now,
                grace
            ),
            AgentStatus::Running
        );
        // Past the grace window it is allowed to go quiet.
        assert_eq!(
            stabilize_agent_status(
                AgentStatus::Unknown,
                Some(AgentStatus::Running),
                Some(now - Duration::from_secs(4)),
                now,
                grace
            ),
            AgentStatus::Unknown
        );
        // A fresh reading always wins over the grace.
        assert_eq!(
            stabilize_agent_status(
                AgentStatus::WaitingForInput,
                Some(AgentStatus::Running),
                Some(now),
                now,
                grace
            ),
            AgentStatus::WaitingForInput
        );
    }

    #[test]
    fn toolbelt_trim_drops_input_before_stop_and_copy() {
        let mut visible = vec![
            ("Stop", AgentToolbeltAction::Interrupt, 88.),
            ("Copy", AgentToolbeltAction::CopyMenu, 88.),
            ("Resume", AgentToolbeltAction::Resume, 112.),
            ("Logs", AgentToolbeltAction::OpenLogs, 88.),
            ("Input", AgentToolbeltAction::DockInput, 88.),
        ];
        trim_agent_toolbelt_buttons(&mut visible, 190.);
        let actions: Vec<_> = visible
            .iter()
            .map(|(_, action, _)| action.clone())
            .collect();
        assert_eq!(
            actions,
            vec![
                AgentToolbeltAction::Interrupt,
                AgentToolbeltAction::CopyMenu
            ],
            "Stop and Copy are the last two standing"
        );
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

    /// Regression: this encoder used to keep dots, so every user whose home
    /// directory contains one — `/Users/first.last` — got a project path that
    /// matched nothing and a Claude `Logs` action that silently did nothing.
    #[test]
    fn claude_project_path_dashes_dots_as_well_as_separators() {
        assert_eq!(
            encode_claude_project_path(Path::new("/Users/first.last/Documents/repo")),
            "-Users-first-last-Documents-repo"
        );
        assert_eq!(
            encode_claude_project_path(Path::new("/Users/plain/repo")),
            "-Users-plain-repo"
        );
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
