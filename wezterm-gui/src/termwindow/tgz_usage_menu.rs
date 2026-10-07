//! The token usage popup opened from the Agents section header.
//!
//! A sidebar row has room for one short number; this panel has room for the
//! rest: each live session's tokens, today's and the last week's totals, and
//! a bar per day. The per-day figures are folded from the transcripts on disk
//! by `agent_herd::usage_history` on a worker thread, so nothing is recorded
//! for it and opening the popup never blocks painting.
//!
//! Row building is pure and tested here; painting stays with the other
//! sidebar dropdowns in `render::sidebar`.

use crate::agent_herd::usage_history::{self, UsageHistory};
use crate::termwindow::render::sidebar::herd_scan_is_due;
use chrono::NaiveDate;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use window::WindowOps;

/// Text columns of the popup: wide enough for the longest graph plus its
/// caption.
pub(crate) const USAGE_MENU_COLS: usize = 48;

/// The spans the graph row steps through when clicked.
const GRAPH_SPANS: [usize; 3] = [7, 14, usage_history::HISTORY_DAYS];

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
            let history = usage_history::scan_history(&home, SystemTime::now());
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

/// One line of the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageRow {
    pub label: String,
    pub divider_above: bool,
    /// Clicking this row changes how many days the graph covers.
    pub cycles_graph: bool,
}

/// The graph span that follows `days`.
pub(crate) fn next_graph_span(days: usize) -> usize {
    let at = GRAPH_SPANS.iter().position(|span| *span == days);
    GRAPH_SPANS[at.map_or(0, |at| (at + 1) % GRAPH_SPANS.len())]
}

/// Token counts as the sidebar writes them: `950`, `19.5k`, `1.2m`.
fn compact(value: u64) -> String {
    match value {
        value if value >= 1_000_000 => format!("{:.1}m", value as f64 / 1_000_000.0),
        value if value >= 1_000 => format!("{:.1}k", value as f64 / 1_000.0),
        value => value.to_string(),
    }
}

/// `left` and `right` at opposite ends of a `cols`-wide line. `left` gives
/// way when the two do not fit.
fn spread(left: &str, right: &str, cols: usize) -> String {
    let right_len = right.chars().count();
    let room = cols.saturating_sub(right_len + 1);
    let left_len = left.chars().count();
    let left: String = if left_len > room {
        let kept: String = left.chars().take(room.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        left.to_string()
    };
    let gap = cols.saturating_sub(left.chars().count() + right_len);
    format!("{left}{}{right}", " ".repeat(gap))
}

/// The popup's rows.
///
/// `sessions` are the live agents as `(name, tokens)`; `history` is `None`
/// until the first scan lands.
pub(crate) fn usage_rows(
    sessions: &[(String, u64)],
    history: Option<&UsageHistory>,
    today: NaiveDate,
    graph_days: usize,
    cols: usize,
) -> Vec<UsageRow> {
    let row = |label: String, divider_above: bool| UsageRow {
        label,
        divider_above,
        cycles_graph: false,
    };
    let mut rows = vec![];

    let mut sessions: Vec<&(String, u64)> = sessions.iter().collect();
    sessions.sort_by(|a, b| b.1.cmp(&a.1));
    let live_total: u64 = sessions.iter().map(|(_, tokens)| tokens).sum();
    rows.push(row(
        spread(
            &format!("Open sessions · {}", sessions.len()),
            &compact(live_total),
            cols,
        ),
        false,
    ));
    for (name, tokens) in sessions.iter().take(MAX_SESSION_ROWS) {
        rows.push(row(
            spread(&format!("  {name}"), &compact(*tokens), cols),
            false,
        ));
    }
    if sessions.len() > MAX_SESSION_ROWS {
        let rest: u64 = sessions[MAX_SESSION_ROWS..].iter().map(|(_, t)| t).sum();
        rows.push(row(
            spread(
                &format!("  {} more", sessions.len() - MAX_SESSION_ROWS),
                &compact(rest),
                cols,
            ),
            false,
        ));
    }

    let Some(history) = history else {
        rows.push(row("Reading transcripts…".to_string(), true));
        return rows;
    };
    rows.push(row(
        spread("Today", &compact(history.total(today, 1)), cols),
        true,
    ));
    rows.push(row(
        spread("Last 7 days", &compact(history.total(today, 7)), cols),
        false,
    ));
    let series = history.series(today, graph_days);
    let bars = usage_history::sparkline(&series);
    // The bars are the point of this row, so the caption is what shortens
    // when the two do not fit.
    let room = cols.saturating_sub(bars.chars().count() + 1);
    let caption = vec![
        format!("{graph_days} days · {}", compact(series.iter().sum())),
        format!("{graph_days} days"),
        format!("{graph_days}d"),
    ]
    .into_iter()
    .find(|caption| caption.chars().count() <= room)
    .unwrap_or_default();
    rows.push(UsageRow {
        label: spread(&bars, &caption, cols),
        divider_above: true,
        cycles_graph: true,
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn history() -> UsageHistory {
        UsageHistory {
            days: vec![
                (day("2026-10-01"), 500_000),
                (day("2026-10-06"), 1_000_000),
                (day("2026-10-07"), 250_000),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn labels(rows: &[UsageRow]) -> Vec<&str> {
        rows.iter().map(|row| row.label.as_str()).collect()
    }

    #[test]
    fn spread_pins_both_ends_and_shortens_the_left() {
        assert_eq!(spread("Today", "1.2m", 12), "Today   1.2m");
        assert_eq!(spread("a long session name", "19.5k", 12), "a lon… 19.5k");
    }

    #[test]
    fn rows_list_sessions_then_totals_then_the_graph() {
        let sessions = vec![("small".to_string(), 19_500), ("big".to_string(), 508_400)];
        let rows = usage_rows(&sessions, Some(&history()), day("2026-10-07"), 7, 30);
        assert_eq!(
            labels(&rows),
            vec![
                "Open sessions · 2       527.9k",
                "  big                   508.4k",
                "  small                  19.5k",
                "Today                   250.0k",
                "Last 7 days               1.8m",
                "▅▁▁▁▁█▃          7 days · 1.8m",
            ]
        );
        // Dividers open the totals and the graph; only the graph is clickable.
        assert_eq!(
            rows.iter().map(|r| r.divider_above).collect::<Vec<_>>(),
            vec![false, false, false, true, false, true]
        );
        assert_eq!(
            rows.iter().map(|r| r.cycles_graph).collect::<Vec<_>>(),
            vec![false, false, false, false, false, true]
        );
        assert!(rows.iter().all(|r| r.label.chars().count() == 30));
    }

    #[test]
    fn rows_say_so_while_the_history_is_still_being_read() {
        let rows = usage_rows(&[], None, day("2026-10-07"), 7, 30);
        assert_eq!(
            labels(&rows),
            vec!["Open sessions · 0            0", "Reading transcripts…"]
        );
    }

    #[test]
    fn sessions_past_the_cap_are_folded_into_one_row() {
        let sessions: Vec<(String, u64)> = (1..=7).map(|n| (format!("s{n}"), n * 1_000)).collect();
        let rows = usage_rows(&sessions, None, day("2026-10-07"), 7, 30);
        assert_eq!(rows[1].label, "  s7                      7.0k");
        assert_eq!(rows[6].label, "  2 more                  3.0k");
    }

    #[test]
    fn the_longest_graph_fits_the_popup() {
        let rows = usage_rows(
            &[],
            Some(&history()),
            day("2026-10-07"),
            usage_history::HISTORY_DAYS,
            USAGE_MENU_COLS,
        );
        let graph = rows.last().unwrap();
        assert!(graph.cycles_graph);
        assert_eq!(graph.label.chars().count(), USAGE_MENU_COLS);
        assert_eq!(
            graph
                .label
                .chars()
                .filter(|c| ('▁'..='█').contains(c))
                .count(),
            usage_history::HISTORY_DAYS
        );
    }

    #[test]
    fn the_graph_caption_shortens_before_the_bars_do() {
        let today = day("2026-10-07");
        let graph = |cols| {
            usage_rows(&[], Some(&history()), today, 14, cols)
                .pop()
                .unwrap()
                .label
        };
        assert!(graph(30).ends_with("14 days · 1.8m"));
        assert!(graph(24).ends_with(" 14 days"));
        assert!(graph(18).ends_with(" 14d"));
        for cols in [30, 24, 18] {
            assert_eq!(
                graph(cols)
                    .chars()
                    .filter(|c| ('▁'..='█').contains(c))
                    .count(),
                14
            );
        }
    }

    #[test]
    fn graph_span_cycles_through_the_offered_spans() {
        assert_eq!(next_graph_span(7), 14);
        assert_eq!(next_graph_span(14), usage_history::HISTORY_DAYS);
        assert_eq!(next_graph_span(usage_history::HISTORY_DAYS), 7);
        assert_eq!(next_graph_span(99), 7);
    }
}
