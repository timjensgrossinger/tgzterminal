//! The token usage popup opened from the Agents section header.
//!
//! A sidebar row has room for one short number; this panel has room for the
//! rest: each live session's tokens, today's and the last week's totals, and
//! a bar per day (a click expands it into a chart). The per-day figures are folded from the transcripts on disk
//! by `agent_herd::usage_history` on a worker thread, so nothing is recorded
//! for it and opening the popup never blocks painting.
//!
//! Row building is pure and tested here; painting stays with the other
//! sidebar dropdowns in `render::sidebar`.

use crate::agent_herd::quota::{self, BandState, GaugeLevel, QuotaSnapshot, VendorQuota};
use crate::agent_herd::usage_history::{self, DayTokens, OpencodeSpend, Spend, UsageHistory};
use crate::agent_herd::vendor::{AgentVendor, SessionRoot};
use crate::termwindow::render::sidebar::herd_scan_is_due;
use crate::termwindow::tgz_usage_relay::RelayStatus;
use chrono::NaiveDate;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use window::WindowOps;

/// Text columns of the popup.
pub(crate) const USAGE_MENU_COLS: usize = 48;
/// Text columns of the popup while its graph is expanded.
pub(crate) const USAGE_MENU_WIDE_COLS: usize = 72;

/// Rows the expanded chart is tall, counted in popup rows.
pub(crate) const CHART_ROWS: usize = 8;

/// Columns a quota bar takes. Fixed, so the bars of every row line up.
pub(crate) const GAUGE_COLS: usize = 12;

/// The narrowest each value column is, rightmost first, as the text that
/// fills it at its widest: the cells of every row line up on these unless
/// one of them is wider still.
pub(crate) const CELL_COLUMN_TEMPLATES: [&[&str]; 2] =
    [&["resets 23h 59m", "888.8k"], &["$888.88", "100%"]];

/// The spans the expanded chart offers.
pub(crate) const GRAPH_SPANS: [usize; 3] = [7, 14, usage_history::HISTORY_DAYS];

/// Columns the one-line graph's bars take at the left of their row.
pub(crate) const GRAPH_BAR_COLS: usize = 18;

/// Sessions listed by name; the rest are folded into the total.
const MAX_SESSION_ROWS: usize = 5;

/// How long a history scan is reused while the popup is open.
const HISTORY_TTL: Duration = Duration::from_secs(20);
/// How long one is reused while it is closed. The background refresh only
/// has to keep the first open instant, not track every turn.
const BACKGROUND_TTL: Duration = Duration::from_secs(5 * 60);
/// How long after launch the first background scan waits, so reading a
/// month of transcripts never competes with startup.
const BACKGROUND_DELAY: Duration = Duration::from_secs(20);
/// A scan that has not reported back after this long is presumed dead. The
/// first scan reads every recent transcript whole, so this is generous.
const HISTORY_WATCHDOG: Duration = Duration::from_secs(180);

/// Where the popup is anchored.
#[derive(Clone, Debug)]
pub struct UsageMenuState {
    pub x: usize,
    pub y: usize,
}

/// The per-day history as last scanned, and the scan in flight if any.
/// One per process: every window shows the same machine-wide figures, and
/// two windows must not each read the transcripts.
#[derive(Default)]
struct HistoryCache {
    scanned: Option<(Instant, Arc<UsageHistory>)>,
    scan_started_at: Option<Instant>,
}

static HISTORY: LazyLock<Mutex<HistoryCache>> = LazyLock::new(Default::default);
static LAUNCHED_AT: LazyLock<Instant> = LazyLock::new(Instant::now);

/// The homes usage is read from, as of the last history scan.
static USAGE_ROOTS: LazyLock<Mutex<Vec<SessionRoot>>> = LazyLock::new(Default::default);

/// The homes usage is read from: this machine's, and those inside every WSL
/// distro that is running, so a distro entered by typing `wsl` in a PowerShell
/// tab counts as well. Reading a stopped distro's share would boot it.
///
/// Blocking (a hidden `wsl.exe`, and the distro's share): worker threads only.
fn find_usage_roots(home: std::path::PathBuf) -> Vec<SessionRoot> {
    let specs: Vec<(String, Option<String>)> = if cfg!(windows) {
        crate::termwindow::wsl_paths::load_distros_with_state()
            .unwrap_or_default()
            .into_iter()
            .filter(|distro| distro.state == config::WSL_STATE_RUNNING)
            .map(|distro| (distro.name, None))
            .collect()
    } else {
        vec![]
    };
    crate::termwindow::render::sidebar::agent_session_roots(home, &specs)
}

/// The homes found by the last history scan. Never blocks; empty before the
/// first one.
pub(crate) fn usage_roots() -> Vec<SessionRoot> {
    USAGE_ROOTS.lock().unwrap().clone()
}

/// Note the launch time. Called once at GUI startup; the background scan
/// counts its delay from here.
pub fn note_launch() {
    LazyLock::force(&LAUNCHED_AT);
}

fn kick_scan_if_due(ttl: Duration) {
    let mut cache = HISTORY.lock().unwrap();
    let now = Instant::now();
    if !herd_scan_is_due(
        cache.scan_started_at,
        cache.scanned.as_ref().map(|(at, _)| *at),
        ttl,
        HISTORY_WATCHDOG,
        now,
    ) {
        return;
    }
    let Some(home) = dirs_next::home_dir() else {
        return;
    };
    cache.scan_started_at = Some(now);
    let spawned = std::thread::Builder::new()
        .name("usage-history".into())
        .spawn(move || {
            let roots = find_usage_roots(home);
            *USAGE_ROOTS.lock().unwrap() = roots.clone();
            let history = usage_history::scan_history(&roots, SystemTime::now());
            let changed = {
                let mut cache = HISTORY.lock().unwrap();
                let changed = cache
                    .scanned
                    .as_ref()
                    .map_or(true, |(_, previous)| **previous != history);
                cache.scanned = Some((Instant::now(), Arc::new(history)));
                cache.scan_started_at = None;
                changed
            };
            if changed {
                // An open popup is showing the old figures, or none yet.
                promise::spawn::spawn_into_main_thread(async {
                    for gui_window in crate::frontend::front_end().gui_windows() {
                        gui_window.window.invalidate();
                    }
                })
                .detach();
            }
        });
    if let Err(err) = spawned {
        log::warn!("failed to start usage history scan: {err:#}");
        cache.scan_started_at = None;
    }
}

/// Keep the history warm while the popup is closed, so opening it shows
/// figures at once. Cheap enough to call on every paint: it only compares
/// timestamps unless a refresh is due.
///
/// A refresh stats each recent transcript and reads only what was appended,
/// so its cost follows how much the agents wrote since the last one, not how
/// many are open.
pub(crate) fn keep_history_warm() {
    if LAUNCHED_AT.elapsed() >= BACKGROUND_DELAY {
        kick_scan_if_due(BACKGROUND_TTL);
    }
}

/// The history for an open popup, refreshed at the popup's shorter interval.
/// Never blocks: returns what the last scan found, or `None` before the
/// first one lands.
pub(crate) fn history_for_menu() -> Option<Arc<UsageHistory>> {
    kick_scan_if_due(HISTORY_TTL);
    HISTORY
        .lock()
        .unwrap()
        .scanned
        .as_ref()
        .map(|(_, history)| Arc::clone(history))
}

/// What clicking a line of the popup, or a control on it, does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageRowAction {
    /// Nothing: the line is for reading, and is not highlighted on hover.
    Inert,
    /// Open the graph into the full chart.
    ExpandGraph,
    /// Fold the chart back into its one-line graph.
    CollapseGraph,
    /// Show this many days in the chart.
    SetGraphSpan(usize),
    /// The pointer is over this day's bar of the chart (oldest is 0). A
    /// click does nothing; hovering names the day.
    ChartDay(usize),
}

/// A quota bar drawn at the right of a row, left of its cells.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageGauge {
    /// Used share, 0..=1.
    pub fraction: f32,
    pub level: GaugeLevel,
}

/// One value in a row's columns.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCell {
    pub text: String,
    /// A note or a column head rather than a figure: drawn dimmer.
    pub dim: bool,
}

impl UsageCell {
    fn value(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dim: false,
        }
    }

    fn note(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dim: true,
        }
    }
}

/// What a row of the popup draws beyond its label, as the dropdown painter
/// takes it.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageLine {
    pub header: bool,
    pub cells: Vec<UsageCell>,
    pub widget: Option<RowWidget>,
}

impl UsageLine {
    /// How many popup rows the line takes.
    pub fn height_rows(&self) -> usize {
        self.widget.as_ref().map_or(1, RowWidget::height_rows)
    }
}

/// A day per bar, oldest first, split by vendor so a bar can be stacked.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageBars {
    pub days: Vec<DayTokens>,
}

/// The expanded chart: bars against a labelled axis, with a date under
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageChart {
    pub days: Vec<DayTokens>,
    /// The top of the axis: a round number at or above the busiest day.
    pub axis_max: u64,
    /// The labelled lines, bottom up: value and its text.
    pub ticks: Vec<(u64, String)>,
    /// Labels under bars: bar index and text.
    pub x_labels: Vec<(usize, String)>,
    /// The bar the pointer is over.
    pub hovered: Option<usize>,
}

/// One control in a row's strip of buttons, right-aligned.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageTab {
    pub label: String,
    pub action: UsageRowAction,
    /// The current choice: drawn filled.
    pub active: bool,
}

/// What a row draws beside, or instead of, its text.
#[derive(Clone, Debug, PartialEq)]
pub enum RowWidget {
    /// A quota bar and its caption at the right.
    Gauge(UsageGauge),
    /// A small bar graph at the left; the label is its caption.
    Bars(UsageBars),
    /// The full chart, [`CHART_ROWS`] rows tall; the label is not drawn.
    Chart(UsageChart),
    /// Buttons at the right.
    Tabs(Vec<UsageTab>),
}

impl RowWidget {
    /// How many popup rows the widget's row takes.
    pub fn height_rows(&self) -> usize {
        match self {
            RowWidget::Chart(_) => CHART_ROWS,
            _ => 1,
        }
    }
}

/// One line of the popup: a label at the left, values in columns at the
/// right.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageRow {
    pub label: String,
    pub divider_above: bool,
    pub action: UsageRowAction,
    pub widget: Option<RowWidget>,
    /// Heads a group: the label in the medium weight.
    pub header: bool,
    /// Nested under the header above it.
    pub indent: bool,
    /// Right-aligned columns at the right of the row, rightmost last.
    pub cells: Vec<UsageCell>,
    /// The vendor a card belongs to; cards are divided from each other.
    pub vendor: Option<AgentVendor>,
    pub trailing_chevron: bool,
}

impl UsageRow {
    fn text(label: impl Into<String>, divider_above: bool) -> Self {
        Self {
            label: label.into(),
            divider_above,
            action: UsageRowAction::Inert,
            widget: None,
            header: false,
            indent: false,
            cells: vec![],
            vendor: None,
            trailing_chevron: false,
        }
    }

    fn header(label: impl Into<String>, divider_above: bool) -> Self {
        Self {
            header: true,
            ..Self::text(label, divider_above)
        }
    }

    fn indented(mut self) -> Self {
        self.indent = true;
        self
    }

    fn with_cells(mut self, cells: Vec<UsageCell>) -> Self {
        self.cells = cells;
        self
    }

    /// What the dropdown painter draws beyond the label, if anything.
    pub fn line(&self) -> Option<UsageLine> {
        (self.header || !self.cells.is_empty() || self.widget.is_some()).then(|| UsageLine {
            header: self.header,
            cells: self.cells.clone(),
            widget: self.widget.clone(),
        })
    }
}

/// A live session as the popup lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LiveSession {
    pub name: String,
    pub tokens: u64,
    /// What the session reports it cost, already formatted (`$0.1250`).
    pub cost: Option<String>,
}

/// How the graph part of the popup is to be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GraphView {
    pub days: usize,
    pub expanded: bool,
    /// The chart bar under the pointer.
    pub hovered: Option<usize>,
}

/// A round number at or above `max`, so the axis reads `0 · 2.5m · 5m`
/// rather than `0 · 2.3m · 4.6m`: 1, 2, 2.5 or 5 times a power of ten.
pub(crate) fn nice_axis_max(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    let mut magnitude: u64 = 1;
    while let Some(next) = magnitude.checked_mul(10).filter(|next| *next <= max) {
        magnitude = next;
    }
    for step in [10u64, 20, 25, 50, 100] {
        let candidate = magnitude.saturating_mul(step) / 10;
        if candidate >= max {
            return candidate;
        }
    }
    magnitude.saturating_mul(10)
}

/// The labelled lines of an axis that tops out at `axis_max`: 0, half and
/// the top.
fn chart_ticks(axis_max: u64) -> Vec<(u64, String)> {
    if axis_max == 0 {
        return vec![(0, "0".to_string())];
    }
    [0, axis_max / 2, axis_max]
        .iter()
        .map(|value| (*value, compact(*value)))
        .collect()
}

/// The day `index` of `count` days ending with `today` (oldest is 0).
fn chart_day(today: NaiveDate, count: usize, index: usize) -> NaiveDate {
    today - chrono::Duration::days(count.saturating_sub(1 + index) as i64)
}

/// Labels under the bars of `count` days ending with `today`: every
/// weekday for a week, else the first, middle and last day.
pub(crate) fn axis_labels(today: NaiveDate, count: usize) -> Vec<(usize, String)> {
    if count == 0 {
        return vec![];
    }
    let label = |index: usize| {
        if index == count - 1 {
            "Today".to_string()
        } else if count <= 7 {
            chart_day(today, count, index).format("%a").to_string()
        } else {
            chart_day(today, count, index).format("%b %-d").to_string()
        }
    };
    let shown: Vec<usize> = if count <= 7 {
        (0..count).collect()
    } else {
        vec![0, (count - 1) / 2, count - 1]
    };
    shown
        .into_iter()
        .map(|index| (index, label(index)))
        .collect()
}

/// Where each of `count` bars sits in a `width`-wide strip: its left edge
/// and its width. Bars are at most `max_bar` wide and `gap` apart, centred
/// in the strip; when they do not fit, the gap shrinks first, then the
/// bars, never below one pixel each.
pub(crate) fn bar_slots(count: usize, width: f32, gap: f32, max_bar: f32) -> Vec<(f32, f32)> {
    if count == 0 || width <= 0. {
        return vec![];
    }
    let n = count as f32;
    let mut gap = gap.max(0.);
    let mut bar = ((width - gap * (n - 1.)) / n).min(max_bar);
    if bar < 2. {
        gap = (gap * 0.5).min(1.);
        bar = ((width - gap * (n - 1.)) / n).min(max_bar).max(1.);
    }
    let used = bar * n + gap * (n - 1.);
    let left = ((width - used) * 0.5).max(0.);
    (0..count)
        .map(|index| (left + index as f32 * (bar + gap), bar))
        .collect()
}

/// A day's tokens per vendor, in the order a bar stacks them from its foot.
pub(crate) fn day_vendors(day: &DayTokens) -> [(AgentVendor, u64); 5] {
    [
        (AgentVendor::Claude, day.claude),
        (AgentVendor::Codex, day.codex),
        (AgentVendor::OpenCode, day.opencode),
        (AgentVendor::Gemini, day.gemini),
        (AgentVendor::Copilot, day.copilot),
    ]
}

/// The line under the chart, as a label and its cells: the hovered day with
/// its vendors and total, else the span's average and its busiest day.
pub(crate) fn chart_detail(
    days: &[DayTokens],
    today: NaiveDate,
    hovered: Option<usize>,
) -> (String, Vec<UsageCell>) {
    let count = days.len();
    if let Some(index) = hovered.filter(|index| *index < count) {
        let tokens = days[index];
        let label = chart_day(today, count, index)
            .format("%a %-d %b")
            .to_string();
        // Split only when several vendors are in it; otherwise the total says
        // it all.
        let parts: Vec<(AgentVendor, u64)> = day_vendors(&tokens)
            .iter()
            .filter(|(_, tokens)| *tokens > 0)
            .cloned()
            .collect();
        let mut cells = vec![];
        if parts.len() > 1 {
            for (vendor, tokens) in parts {
                cells.push(UsageCell::note(format!(
                    "{} {}",
                    vendor.label(),
                    compact(tokens)
                )));
            }
        }
        cells.push(UsageCell::value(compact(tokens.total())));
        return (label, cells);
    }
    let total: u64 = days.iter().map(DayTokens::total).sum();
    if total == 0 {
        return ("No tokens in this span".to_string(), vec![]);
    }
    let (peak_at, peak) = days
        .iter()
        .map(DayTokens::total)
        .enumerate()
        .max_by_key(|(index, total)| (*total, *index))
        .unwrap_or((0, 0));
    (
        format!("Average {} per day", compact(total / count as u64)),
        vec![UsageCell::note(format!(
            "Peak {} on {}",
            compact(peak),
            chart_day(today, count, peak_at).format("%b %-d")
        ))],
    )
}

/// Token counts as the sidebar writes them: `950`, `19.5k`, `1.2m`.
fn compact(value: u64) -> String {
    match value {
        value if value >= 1_000_000 => format!("{:.1}m", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

/// The token part of the popup's rows.
///
/// `sessions` are the live agents; `history` is `None` until the first scan
/// lands.
pub(crate) fn usage_rows(
    sessions: &[LiveSession],
    history: Option<&UsageHistory>,
    today: NaiveDate,
    graph: GraphView,
) -> Vec<UsageRow> {
    let mut rows = vec![];

    let mut sessions: Vec<&LiveSession> = sessions.iter().collect();
    sessions.sort_by(|a, b| b.tokens.cmp(&a.tokens));
    let live_total: u64 = sessions.iter().map(|session| session.tokens).sum();
    rows.push(
        UsageRow::header("Open sessions", false)
            .with_cells(vec![UsageCell::value(compact(live_total))]),
    );
    for session in sessions.iter().take(MAX_SESSION_ROWS) {
        let mut cells = vec![];
        if let Some(cost) = &session.cost {
            cells.push(UsageCell::value(short_cost(cost)));
        }
        cells.push(UsageCell::value(compact(session.tokens)));
        rows.push(
            UsageRow::text(session.name.clone(), false)
                .indented()
                .with_cells(cells),
        );
    }
    if sessions.len() > MAX_SESSION_ROWS {
        let rest: u64 = sessions[MAX_SESSION_ROWS..]
            .iter()
            .map(|session| session.tokens)
            .sum();
        rows.push(
            UsageRow::text(format!("{} more", sessions.len() - MAX_SESSION_ROWS), false)
                .indented()
                .with_cells(vec![UsageCell::value(compact(rest))]),
        );
    }

    let Some(history) = history else {
        rows.push(UsageRow::text("Reading transcripts…", true));
        return rows;
    };
    rows.push(UsageRow::header("All tools", true).with_cells(vec![UsageCell::note("Tokens")]));
    rows.push(
        UsageRow::text("Today", false)
            .indented()
            .with_cells(vec![UsageCell::value(compact(history.total(today, 1)))]),
    );
    rows.push(
        UsageRow::text("Last 7 days", false)
            .indented()
            .with_cells(vec![UsageCell::value(compact(history.total(today, 7)))]),
    );
    let days = history.series_split(today, graph.days);
    let total: u64 = days.iter().map(DayTokens::total).sum();
    if graph.expanded {
        let tabs = GRAPH_SPANS
            .iter()
            .map(|span| UsageTab {
                label: format!("{span}d"),
                action: UsageRowAction::SetGraphSpan(*span),
                active: *span == graph.days,
            })
            .chain(std::iter::once(UsageTab {
                // nf-fa-chevron_up: the bundled Symbols Nerd Font has it on
                // every platform.
                label: "\u{f077}".to_string(),
                action: UsageRowAction::CollapseGraph,
                active: false,
            }))
            .collect();
        rows.push(UsageRow {
            widget: Some(RowWidget::Tabs(tabs)),
            ..UsageRow::header("Tokens per day", true)
        });
        let axis_max = nice_axis_max(days.iter().map(DayTokens::total).max().unwrap_or(0));
        let hovered = graph.hovered.filter(|index| *index < days.len());
        let (detail, detail_cells) = chart_detail(&days, today, hovered);
        rows.push(UsageRow {
            widget: Some(RowWidget::Chart(UsageChart {
                axis_max,
                ticks: chart_ticks(axis_max),
                x_labels: axis_labels(today, days.len()),
                hovered,
                days,
            })),
            ..UsageRow::text(String::new(), false)
        });
        rows.push(UsageRow::text(detail, false).with_cells(detail_cells));
        return rows;
    }
    // The bars take the left of the row, the span and its total the right.
    rows.push(UsageRow {
        action: UsageRowAction::ExpandGraph,
        widget: Some(RowWidget::Bars(UsageBars { days })),
        trailing_chevron: true,
        ..UsageRow::text(String::new(), true).with_cells(vec![
            UsageCell::note(format!("{} days", graph.days)),
            UsageCell::value(compact(total)),
        ])
    });
    rows
}

/// A cost a vendor reported (`$0.1250`) at the popup's precision (`$0.13`).
fn short_cost(cost: &str) -> String {
    cost.trim_start_matches('$')
        .parse::<f64>()
        .map(money)
        .unwrap_or_else(|_| cost.to_string())
}

/// `$0.42`, `$12.30`, `$140`.
fn money(usd: f64) -> String {
    if usd >= 100. {
        format!("${usd:.0}")
    } else {
        format!("${usd:.2}")
    }
}

/// A spend as its cost and tokens columns.
fn spend_cells(spend: &Spend) -> Vec<UsageCell> {
    vec![
        UsageCell::value(money(spend.cost_usd())),
        UsageCell::value(compact(spend.tokens)),
    ]
}

/// What the quota part of the popup is built from.
pub(crate) struct QuotaInputs<'a> {
    pub snapshot: &'a QuotaSnapshot,
    /// `None` until the first herd scan has looked.
    pub relay: Option<RelayStatus>,
    /// A Claude agent is in the herd: worth explaining why there is no
    /// Claude quota where it cannot be had.
    pub claude_active: bool,
    /// Vendors in the herd with no figures anywhere in the popup, each worth
    /// a line saying so rather than a popup that seems to have missed them.
    pub without_figures: Vec<AgentVendor>,
    pub opencode: Option<OpencodeSpend>,
    pub now: SystemTime,
}

/// A window's bar and its two columns: `47%` and `resets 2h 13m`, or an
/// empty percent and `reset` once the window has passed.
fn band_gauge(band: &quota::QuotaBand, now: SystemTime) -> (UsageGauge, Vec<UsageCell>) {
    let percent = band.used_percent.round().max(0.);
    let (cells, level) = match band.state(now) {
        BandState::Live {
            remaining: Some(remaining),
        } => (
            vec![
                UsageCell::value(format!("{percent:.0}%")),
                UsageCell::note(format!("resets {}", quota::countdown_text(remaining))),
            ],
            quota::gauge_level(band.used_percent),
        ),
        BandState::Live { remaining: None } => (
            vec![
                UsageCell::value(format!("{percent:.0}%")),
                UsageCell::note(""),
            ],
            quota::gauge_level(band.used_percent),
        ),
        BandState::Reset => (
            vec![UsageCell::value(""), UsageCell::note("reset")],
            GaugeLevel::Ok,
        ),
    };
    (
        UsageGauge {
            fraction: band.fraction(now),
            level,
        },
        cells,
    )
}

fn vendor_card(quota: &VendorQuota, now: SystemTime) -> Vec<UsageRow> {
    let mut title = quota.vendor.label().to_string();
    if let Some(plan) = &quota.plan {
        title.push(' ');
        title.push_str(&quota::plan_label(plan));
    }
    let mut rows = vec![UsageRow {
        vendor: Some(quota.vendor.clone()),
        ..UsageRow::header(title, false).with_cells(vec![UsageCell::note(format!(
            "updated {}",
            quota::freshness_text(quota.sampled_at, now)
        ))])
    }];
    for band in &quota.bands {
        let (gauge, cells) = band_gauge(band, now);
        rows.push(UsageRow {
            widget: Some(RowWidget::Gauge(gauge)),
            ..UsageRow::text(band.window.label(), false)
                .indented()
                .with_cells(cells)
        });
    }
    if let Some(cost) = quota.last_cost_usd {
        rows.push(
            UsageRow::text("Last session cost", false)
                .indented()
                .with_cells(vec![UsageCell::value(money(cost))]),
        );
    }
    rows
}

/// The vendors of `active`, deduplicated, that nothing in the popup has a
/// figure for. Claude, Codex and OpenCode are not among them: they always
/// have a store to read, and fill it on their first reply.
pub(crate) fn vendors_without_figures(
    active: &[AgentVendor],
    sessions_with_tokens: &[AgentVendor],
    history: Option<&UsageHistory>,
) -> Vec<AgentVendor> {
    let tokens_in_history = |vendor: &AgentVendor| {
        history.is_some_and(|history| {
            history.days.values().any(|day| {
                day_vendors(day)
                    .iter()
                    .any(|(seen, tokens)| seen == vendor && *tokens > 0)
            })
        })
    };
    let mut vendors = vec![];
    for vendor in active {
        if !matches!(
            vendor,
            AgentVendor::Claude | AgentVendor::Codex | AgentVendor::OpenCode
        ) && !sessions_with_tokens.contains(vendor)
            && !tokens_in_history(vendor)
            && !vendors.contains(vendor)
        {
            vendors.push(vendor.clone());
        }
    }
    vendors
}

/// The subscription part of the popup: a card per vendor with its windows,
/// then OpenCode's spend. Empty when there is nothing to say.
pub(crate) fn quota_rows(inputs: &QuotaInputs) -> Vec<UsageRow> {
    let mut rows = vec![];
    match (inputs.snapshot.vendor(&AgentVendor::Claude), inputs.relay) {
        (Some(claude), _) => rows.extend(vendor_card(claude, inputs.now)),
        (None, Some(RelayStatus::Connected)) => rows.push(UsageRow {
            vendor: Some(AgentVendor::Claude),
            ..UsageRow::header("Claude", false)
                .with_cells(vec![UsageCell::note("limits after its next reply")])
        }),
        (None, Some(RelayStatus::Unsupported)) if inputs.claude_active => rows.push(UsageRow {
            vendor: Some(AgentVendor::Claude),
            ..UsageRow::header("Claude", false)
                .with_cells(vec![UsageCell::note("limits need Git Bash")])
        }),
        _ => {}
    }
    for vendor in inputs
        .snapshot
        .vendors
        .iter()
        .filter(|quota| quota.vendor != AgentVendor::Claude)
    {
        rows.extend(vendor_card(vendor, inputs.now));
    }
    if let Some(spend) = inputs
        .opencode
        .filter(|spend| spend.week.cost_micros > 0 || spend.week.tokens > 0)
    {
        rows.push(UsageRow {
            vendor: Some(AgentVendor::OpenCode),
            ..UsageRow::header("OpenCode", false)
                .with_cells(vec![UsageCell::note("Cost"), UsageCell::note("Tokens")])
        });
        rows.push(
            UsageRow::text("Today", false)
                .indented()
                .with_cells(spend_cells(&spend.today)),
        );
        rows.push(
            UsageRow::text("Last 7 days", false)
                .indented()
                .with_cells(spend_cells(&spend.week)),
        );
    }
    for vendor in &inputs.without_figures {
        let note = match vendor {
            AgentVendor::Copilot => "totals when a session ends",
            _ => "no usage data on disk",
        };
        rows.push(UsageRow {
            vendor: Some(vendor.clone()),
            ..UsageRow::header(vendor.label(), false).with_cells(vec![UsageCell::note(note)])
        });
    }
    // A divider between vendor cards, none above the first.
    let mut previous_vendor = false;
    for row in rows.iter_mut() {
        if row.vendor.is_some() {
            row.divider_above = previous_vendor;
            previous_vendor = true;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn claude_day(claude: u64) -> DayTokens {
        DayTokens {
            claude,
            ..Default::default()
        }
    }

    fn history() -> UsageHistory {
        UsageHistory {
            days: vec![
                (day("2026-10-01"), claude_day(500_000)),
                (
                    day("2026-10-06"),
                    DayTokens {
                        claude: 600_000,
                        codex: 400_000,
                        ..Default::default()
                    },
                ),
                (day("2026-10-07"), claude_day(250_000)),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        }
    }

    /// A row as one line: `# ` before a header, two spaces before an
    /// indented row, then the label and each cell after a `|`.
    fn line(row: &UsageRow) -> String {
        let mut text = String::new();
        if row.header {
            text.push_str("# ");
        }
        if row.indent {
            text.push_str("  ");
        }
        text.push_str(&row.label);
        for cell in &row.cells {
            text.push_str(" | ");
            text.push_str(&cell.text);
        }
        text
    }

    fn lines(rows: &[UsageRow]) -> Vec<String> {
        rows.iter().map(line).collect()
    }

    fn live(name: &str, tokens: u64) -> LiveSession {
        LiveSession {
            name: name.to_string(),
            tokens,
            cost: None,
        }
    }

    fn compact_graph(days: usize) -> GraphView {
        GraphView {
            days,
            expanded: false,
            hovered: None,
        }
    }

    #[test]
    fn rows_list_sessions_then_totals_then_the_graph() {
        let sessions = vec![live("small", 19_500), live("big", 508_400)];
        let rows = usage_rows(
            &sessions,
            Some(&history()),
            day("2026-10-07"),
            compact_graph(7),
        );
        assert_eq!(
            lines(&rows),
            vec![
                "# Open sessions | 527.9k",
                "  big | 508.4k",
                "  small | 19.5k",
                "# All tools | Tokens",
                "  Today | 250.0k",
                "  Last 7 days | 1.8m",
                " | 7 days | 1.8m",
            ]
        );
        // Column heads and notes are dim, figures are not.
        assert!(rows[3].cells[0].dim);
        assert!(!rows[4].cells[0].dim);
        // Dividers open the totals and the graph; only the graph is clickable.
        assert_eq!(
            rows.iter().map(|r| r.divider_above).collect::<Vec<_>>(),
            vec![false, false, false, true, false, false, true]
        );
        let graph = rows.last().unwrap();
        assert_eq!(graph.action, UsageRowAction::ExpandGraph);
        assert!(graph.trailing_chevron);
        assert!(rows[..6].iter().all(|r| r.action == UsageRowAction::Inert));
        let Some(RowWidget::Bars(bars)) = &graph.widget else {
            panic!("the graph row carries its bars");
        };
        assert_eq!(bars.days.len(), 7);
        assert_eq!(bars.days[5].codex, 400_000);
        assert_eq!(bars.days[6], claude_day(250_000));
    }

    #[test]
    fn rows_say_so_while_the_history_is_still_being_read() {
        let rows = usage_rows(&[], None, day("2026-10-07"), compact_graph(7));
        assert_eq!(
            lines(&rows),
            vec!["# Open sessions | 0", "Reading transcripts…"]
        );
    }

    #[test]
    fn sessions_past_the_cap_are_folded_into_one_row() {
        let sessions: Vec<LiveSession> =
            (1..=7).map(|n| live(&format!("s{n}"), n * 1_000)).collect();
        let rows = usage_rows(&sessions, None, day("2026-10-07"), compact_graph(7));
        assert_eq!(line(&rows[1]), "  s7 | 7.0k");
        assert_eq!(line(&rows[6]), "  2 more | 3.0k");
    }

    #[test]
    fn the_graph_row_names_its_span_and_total() {
        let rows = usage_rows(&[], Some(&history()), day("2026-10-07"), compact_graph(14));
        assert_eq!(line(rows.last().unwrap()), " | 14 days | 1.8m");
    }

    #[test]
    fn no_row_joins_its_parts_with_a_middle_dot() {
        let sessions = vec![LiveSession {
            cost: Some("$0.1250".to_string()),
            ..live("oc", 19_500)
        }];
        let today = day("2026-10-07");
        let mut rows = usage_rows(&sessions, Some(&history()), today, compact_graph(7));
        for hovered in [None, Some(5)] {
            let graph = GraphView {
                days: 7,
                expanded: true,
                hovered,
            };
            rows.extend(usage_rows(&sessions, Some(&history()), today, graph));
        }
        let snap = snapshot_with(vec![claude_quota()]);
        rows.extend(quota_rows(&inputs(&snap, Some(RelayStatus::Connected))));
        for text in lines(&rows) {
            assert!(!text.contains('·'), "{text}");
        }
    }

    #[test]
    fn the_expanded_graph_is_a_header_a_chart_and_a_detail_line() {
        let today = day("2026-10-07");
        let graph = GraphView {
            days: 7,
            expanded: true,
            hovered: None,
        };
        let rows = usage_rows(&[], Some(&history()), today, graph);
        let tail = &rows[rows.len() - 3..];
        assert_eq!(line(&tail[0]), "# Tokens per day");
        let Some(RowWidget::Tabs(tabs)) = &tail[0].widget else {
            panic!("the header carries the span tabs");
        };
        assert_eq!(
            tabs.iter().map(|tab| tab.action).collect::<Vec<_>>(),
            vec![
                UsageRowAction::SetGraphSpan(7),
                UsageRowAction::SetGraphSpan(14),
                UsageRowAction::SetGraphSpan(usage_history::HISTORY_DAYS),
                UsageRowAction::CollapseGraph,
            ]
        );
        assert_eq!(
            tabs.iter().map(|tab| tab.active).collect::<Vec<_>>(),
            vec![true, false, false, false]
        );
        let Some(RowWidget::Chart(chart)) = &tail[1].widget else {
            panic!("the chart row carries the chart");
        };
        assert_eq!(tail[1].widget.as_ref().unwrap().height_rows(), CHART_ROWS);
        assert_eq!(chart.days.len(), 7);
        assert_eq!(chart.axis_max, 1_000_000);
        assert_eq!(
            chart.ticks,
            vec![
                (0, "0".to_string()),
                (500_000, "500.0k".to_string()),
                (1_000_000, "1.0m".to_string()),
            ]
        );
        assert_eq!(chart.x_labels.last().unwrap(), &(6, "Today".to_string()));
        assert_eq!(
            line(&tail[2]),
            "Average 250.0k per day | Peak 1.0m on Oct 6"
        );

        let hovered = GraphView {
            hovered: Some(5),
            ..graph
        };
        let rows = usage_rows(&[], Some(&history()), today, hovered);
        assert_eq!(
            line(rows.last().unwrap()),
            "Tue 6 Oct | Claude 600.0k | Codex 400.0k | 1.0m"
        );
    }

    #[test]
    fn the_axis_tops_out_at_a_round_number() {
        assert_eq!(nice_axis_max(0), 0);
        assert_eq!(nice_axis_max(1), 1);
        assert_eq!(nice_axis_max(7), 10);
        assert_eq!(nice_axis_max(10), 10);
        assert_eq!(nice_axis_max(11), 20);
        assert_eq!(nice_axis_max(2_300_000), 2_500_000);
        assert_eq!(nice_axis_max(4_600_000), 5_000_000);
        assert_eq!(nice_axis_max(5_100_000), 10_000_000);
        assert!(nice_axis_max(u64::MAX) >= u64::MAX / 10);
    }

    #[test]
    fn axis_labels_name_each_weekday_or_three_dates() {
        let today = day("2026-10-07");
        let week = axis_labels(today, 7);
        assert_eq!(week.len(), 7);
        assert_eq!(week[0], (0, "Thu".to_string()));
        assert_eq!(week[6], (6, "Today".to_string()));
        assert_eq!(
            axis_labels(today, 30),
            vec![
                (0, "Sep 8".to_string()),
                (14, "Sep 22".to_string()),
                (29, "Today".to_string()),
            ]
        );
        assert!(axis_labels(today, 0).is_empty());
    }

    #[test]
    fn bars_fit_their_strip() {
        let slots = bar_slots(7, 700., 8., 30.);
        assert_eq!(slots.len(), 7);
        assert!(slots.iter().all(|(_, w)| *w == 30.));
        // Centred: the same room either side.
        let (first, _) = slots[0];
        let (last, w) = slots[6];
        assert!((first - (700. - (last + w))).abs() < 0.01);

        for (count, width) in [(30, 60.), (30, 20.), (14, 100.), (1, 5.)] {
            let slots = bar_slots(count, width, 4., 40.);
            assert_eq!(slots.len(), count);
            assert!(slots.iter().all(|(x, w)| *w >= 1. && *x >= 0.));
            let (x, w) = slots[count - 1];
            if count as f32 * 2. <= width {
                assert!(x + w <= width + 0.01, "{} bars in {}px", count, width);
            }
        }
        assert!(bar_slots(0, 100., 4., 40.).is_empty());
    }

    #[test]
    fn a_quiet_span_says_so() {
        assert_eq!(
            chart_detail(&[DayTokens::default(); 7], day("2026-10-07"), None),
            ("No tokens in this span".to_string(), vec![])
        );
    }

    #[test]
    fn a_session_with_a_cost_shows_it_beside_its_tokens() {
        let sessions = vec![LiveSession {
            cost: Some("$0.1250".to_string()),
            ..live("oc", 19_500)
        }];
        let rows = usage_rows(&sessions, None, day("2026-10-07"), compact_graph(7));
        assert_eq!(line(&rows[1]), "  oc | $0.12 | 19.5k");
    }

    use crate::agent_herd::quota::{QuotaBand, QuotaWindow};
    use std::time::Duration;

    const NOW: u64 = 1_790_000_000;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn snapshot_with(vendors: Vec<VendorQuota>) -> QuotaSnapshot {
        QuotaSnapshot { vendors }
    }

    fn claude_quota() -> VendorQuota {
        VendorQuota {
            vendor: AgentVendor::Claude,
            plan: None,
            bands: vec![
                QuotaBand {
                    window: QuotaWindow::FiveHour,
                    used_percent: 47.,
                    resets_at: Some(at(NOW + 2 * 3600 + 13 * 60)),
                },
                QuotaBand {
                    window: QuotaWindow::Weekly,
                    used_percent: 91.,
                    resets_at: Some(at(NOW - 5)),
                },
            ],
            last_cost_usd: Some(3.414),
            sampled_at: at(NOW - 120),
        }
    }

    fn inputs<'a>(snapshot: &'a QuotaSnapshot, relay: Option<RelayStatus>) -> QuotaInputs<'a> {
        QuotaInputs {
            snapshot,
            relay,
            claude_active: true,
            without_figures: vec![],
            opencode: None,
            now: at(NOW),
        }
    }

    #[test]
    fn a_vendor_card_has_a_bar_per_window() {
        let codex = VendorQuota {
            vendor: AgentVendor::Codex,
            plan: Some("plus".to_string()),
            bands: vec![QuotaBand {
                window: QuotaWindow::FiveHour,
                used_percent: 12.,
                resets_at: None,
            }],
            last_cost_usd: None,
            sampled_at: at(NOW),
        };
        let snap = snapshot_with(vec![claude_quota(), codex]);
        let rows = quota_rows(&inputs(&snap, Some(RelayStatus::Connected)));
        assert_eq!(
            lines(&rows),
            vec![
                "# Claude | updated 2m ago",
                "  5h window | 47% | resets 2h 13m",
                // Past its reset the weekly bar empties instead of showing old news.
                "  Weekly |  | reset",
                "  Last session cost | $3.41",
                "# Codex Plus | updated now",
                "  5h window | 12% | ",
            ]
        );
        let gauges: Vec<_> = rows
            .iter()
            .map(|r| match &r.widget {
                Some(RowWidget::Gauge(gauge)) => Some(gauge.clone()),
                _ => None,
            })
            .collect();
        assert!((gauges[1].as_ref().unwrap().fraction - 0.47).abs() < 1e-6);
        assert_eq!(gauges[2].as_ref().unwrap().fraction, 0.);
        // Every bar has the same two columns after it, so the bars line up.
        assert!(rows
            .iter()
            .filter(|r| r.widget.is_some())
            .all(|r| r.cells.len() == 2));
        // A divider between the two cards only.
        assert_eq!(
            rows.iter().map(|r| r.divider_above).collect::<Vec<_>>(),
            vec![false, false, false, false, true, false]
        );
    }

    #[test]
    fn claude_without_figures_awaits_the_relay() {
        let empty = QuotaSnapshot::default();
        // Turned off, or no Claude Code here: nothing to say.
        assert!(quota_rows(&inputs(&empty, Some(RelayStatus::Disconnected))).is_empty());

        let rows = quota_rows(&inputs(&empty, Some(RelayStatus::Connected)));
        assert_eq!(lines(&rows), vec!["# Claude | limits after its next reply"]);
        assert_eq!(rows[0].action, UsageRowAction::Inert);

        let rows = quota_rows(&inputs(&empty, Some(RelayStatus::Unsupported)));
        assert_eq!(lines(&rows), vec!["# Claude | limits need Git Bash"]);
        // Before the first look, and on Windows without a Claude agent: nothing.
        assert!(quota_rows(&inputs(&empty, None)).is_empty());
        let quiet = QuotaInputs {
            claude_active: false,
            ..inputs(&empty, Some(RelayStatus::Unsupported))
        };
        assert!(quota_rows(&quiet).is_empty());
    }

    #[test]
    fn opencode_spend_is_listed_when_there_is_any() {
        let empty = QuotaSnapshot::default();
        let spend = OpencodeSpend {
            today: Spend {
                cost_micros: 1_230_000,
                tokens: 48_200,
            },
            week: Spend {
                cost_micros: 140_400_000,
                tokens: 2_100_000,
            },
        };
        let with = QuotaInputs {
            opencode: Some(spend),
            ..inputs(&empty, None)
        };
        assert_eq!(
            lines(&quota_rows(&with)),
            vec![
                "# OpenCode | Cost | Tokens",
                "  Today | $1.23 | 48.2k",
                "  Last 7 days | $140 | 2.1m",
            ]
        );
        let none = QuotaInputs {
            opencode: Some(OpencodeSpend::default()),
            ..inputs(&empty, None)
        };
        assert!(quota_rows(&none).is_empty());
    }

    #[test]
    fn a_vendor_nothing_can_be_read_for_says_so() {
        use AgentVendor::*;
        let active = [Claude, Antigravity, Antigravity, Gemini, Copilot, Cursor];
        let history = UsageHistory {
            days: vec![(
                day("2026-10-07"),
                DayTokens {
                    gemini: 5,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let without = vendors_without_figures(&active, &[Cursor], Some(&history));
        assert_eq!(without, vec![Antigravity, Copilot]);

        let empty = QuotaSnapshot::default();
        let with = QuotaInputs {
            without_figures: without,
            ..inputs(&empty, None)
        };
        assert_eq!(
            lines(&quota_rows(&with)),
            vec![
                "# Antigravity | no usage data on disk",
                "# Copilot | totals when a session ends",
            ]
        );
    }

    #[test]
    fn the_chart_detail_names_every_vendor_of_a_shared_day() {
        let days = [DayTokens {
            claude: 1_000,
            gemini: 2_000,
            ..Default::default()
        }];
        let (label, cells) = chart_detail(&days, day("2026-10-07"), Some(0));
        assert_eq!(label, "Wed 7 Oct");
        assert_eq!(
            cells.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["Claude 1.0k", "Gemini 2.0k", "3.0k"]
        );
    }
}
