//! How much of each subscription's rate-limit windows is used, per vendor.
//!
//! Two credential-free sources feed it, neither of which this module reads
//! itself:
//!
//! * **Claude**: Claude Code hands its `statusLine` command a JSON object
//!   whose `rate_limits` carries the account's rolling five-hour and weekly
//!   windows (documented at code.claude.com/docs/en/statusline). A relay
//!   script (see `termwindow::tgz_usage_relay`) saves the last one to a file;
//!   [`poll_claude_sample`] reads it when it changes. `rate_limits` is only
//!   present for Pro/Max subscribers and only after a session's first
//!   response, so a sample without it keeps the windows already known.
//! * **Codex**: every `token_count` event in a rollout carries
//!   `rate_limits.primary` / `secondary` (`used_percent`, `window_minutes`,
//!   `resets_at`). The transcript folds in [`super::usage`] and
//!   [`super::usage_history`] already read those lines; they hand each one to
//!   [`record_codex_line`].
//!
//! The newest sample per vendor is kept in one process-wide store: every
//! window shows the same account figures. Nothing here touches the GUI; the
//! herd scan and the history scan already repaint the windows when they land.

use super::vendor::AgentVendor;
use serde::Deserialize;
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

/// The relay file is a single statusline payload; anything larger is not one.
const MAX_SAMPLE_BYTES: u64 = 256 * 1024;

/// A vendor whose every window has reset and whose last sample is older than
/// this is no longer shown: nobody has used it in over a week.
const STALE_AFTER: Duration = Duration::from_secs(8 * 24 * 60 * 60);

/// Used share at which a gauge turns from the accent towards the attention
/// colour, and at which it is fully the attention colour.
const WARN_PERCENT: f32 = 60.;
const CRITICAL_PERCENT: f32 = 85.;

/// Which rate-limit window a band measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaWindow {
    FiveHour,
    Weekly,
    /// A Claude apps gateway spend limit (daily, weekly or monthly).
    Spend,
    /// A window of some other length, in minutes.
    Other(u32),
}

impl QuotaWindow {
    fn from_minutes(minutes: u32) -> Self {
        match minutes {
            300 => Self::FiveHour,
            10080 => Self::Weekly,
            other => Self::Other(other),
        }
    }

    /// Short label for the popup row.
    pub fn label(&self) -> String {
        match self {
            Self::FiveHour => "5h window".to_string(),
            Self::Weekly => "Weekly".to_string(),
            Self::Spend => "Spend limit".to_string(),
            Self::Other(minutes) if *minutes >= 24 * 60 && minutes % (24 * 60) == 0 => {
                format!("{}d window", minutes / (24 * 60))
            }
            Self::Other(minutes) if *minutes >= 60 => format!("{}h window", minutes / 60),
            Self::Other(minutes) => format!("{minutes}m window"),
        }
    }

    /// Display order inside a vendor card.
    fn order(&self) -> u32 {
        match self {
            Self::FiveHour => 0,
            Self::Weekly => 2,
            Self::Spend => 3,
            Self::Other(minutes) if *minutes < 10080 => 1,
            Self::Other(_) => 2,
        }
    }
}

/// One window's state as last reported.
#[derive(Clone, Debug, PartialEq)]
pub struct QuotaBand {
    pub window: QuotaWindow,
    /// 0..=100 (a spend limit can report more once it is exceeded).
    pub used_percent: f32,
    pub resets_at: Option<SystemTime>,
}

/// Whether a band still describes the current window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandState {
    /// `remaining` is the time until it resets, when known.
    Live { remaining: Option<Duration> },
    /// Its reset time has passed: the vendor has not reported the new window
    /// yet, which it does on the next turn.
    Reset,
}

impl QuotaBand {
    pub fn state(&self, now: SystemTime) -> BandState {
        match self.resets_at {
            Some(at) => match at.duration_since(now) {
                Ok(remaining) if !remaining.is_zero() => BandState::Live {
                    remaining: Some(remaining),
                },
                _ => BandState::Reset,
            },
            None => BandState::Live { remaining: None },
        }
    }

    /// The used share as a fraction for drawing, 0 once the window reset.
    pub fn fraction(&self, now: SystemTime) -> f32 {
        match self.state(now) {
            BandState::Live { .. } => (self.used_percent / 100.).clamp(0., 1.),
            BandState::Reset => 0.,
        }
    }
}

/// How alarming a used share is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GaugeLevel {
    Ok,
    Warn,
    Critical,
}

pub fn gauge_level(used_percent: f32) -> GaugeLevel {
    if used_percent >= CRITICAL_PERCENT {
        GaugeLevel::Critical
    } else if used_percent >= WARN_PERCENT {
        GaugeLevel::Warn
    } else {
        GaugeLevel::Ok
    }
}

/// One vendor's account as last sampled.
#[derive(Clone, Debug, PartialEq)]
pub struct VendorQuota {
    pub vendor: AgentVendor,
    /// The plan name when the vendor reports one (`plus`, `pro`, …).
    pub plan: Option<String>,
    /// In display order.
    pub bands: Vec<QuotaBand>,
    /// Claude: the estimated cost of the session that last reported.
    pub last_cost_usd: Option<f64>,
    pub sampled_at: SystemTime,
}

impl VendorQuota {
    /// Nothing current to show: every window reset long ago.
    pub fn is_stale(&self, now: SystemTime) -> bool {
        let old = now
            .duration_since(self.sampled_at)
            .is_ok_and(|age| age > STALE_AFTER);
        old && self
            .bands
            .iter()
            .all(|band| band.state(now) == BandState::Reset)
    }
}

fn vendor_rank(vendor: &AgentVendor) -> u32 {
    match vendor {
        AgentVendor::Claude => 0,
        AgentVendor::Codex => 1,
        _ => 2,
    }
}

fn sort_bands(bands: &mut [QuotaBand]) {
    bands.sort_by_key(|band| band.window.order());
}

/// What every window shows: the vendors with something to say, Claude first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuotaSnapshot {
    pub vendors: Vec<VendorQuota>,
}

impl QuotaSnapshot {
    pub fn vendor(&self, vendor: &AgentVendor) -> Option<&VendorQuota> {
        self.vendors.iter().find(|quota| &quota.vendor == vendor)
    }

    /// The band the `Σ` button shows: of the first vendor (Claude, then
    /// Codex) with a window still running, the one closest to its limit —
    /// that is the one that will stop the next turn.
    pub fn priority_band(&self, now: SystemTime) -> Option<(&VendorQuota, &QuotaBand)> {
        self.vendors.iter().find_map(|quota| {
            quota
                .bands
                .iter()
                .filter(|band| matches!(band.state(now), BandState::Live { .. }))
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
                .map(|band| (quota, band))
        })
    }
}

/// The newest sample per vendor.
#[derive(Debug, Default)]
pub struct QuotaStore {
    vendors: Vec<VendorQuota>,
    /// Modification time of the Claude sample file as last read.
    claude_sample_mtime: Option<SystemTime>,
}

impl QuotaStore {
    /// Keep `quota` unless a newer sample of the same vendor is already held.
    /// A sample without windows (a Claude session before its first response)
    /// keeps the windows already known. Returns whether anything changed.
    pub fn record(&mut self, mut quota: VendorQuota) -> bool {
        sort_bands(&mut quota.bands);
        match self.vendors.iter_mut().find(|q| q.vendor == quota.vendor) {
            Some(held) => {
                if held.sampled_at > quota.sampled_at {
                    return false;
                }
                if quota.bands.is_empty() {
                    quota.bands = held.bands.clone();
                    quota.plan = quota.plan.or_else(|| held.plan.clone());
                }
                if *held == quota {
                    return false;
                }
                *held = quota;
            }
            None => {
                self.vendors.push(quota);
                self.vendors.sort_by_key(|q| vendor_rank(&q.vendor));
            }
        }
        true
    }

    pub fn snapshot(&self, now: SystemTime) -> QuotaSnapshot {
        QuotaSnapshot {
            vendors: self
                .vendors
                .iter()
                .filter(|quota| !quota.is_stale(now))
                .cloned()
                .collect(),
        }
    }
}

static STORE: LazyLock<Mutex<QuotaStore>> = LazyLock::new(Default::default);

fn store() -> std::sync::MutexGuard<'static, QuotaStore> {
    STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Every vendor's last known quota. A lock and a clone: cheap enough for
/// paint.
pub fn snapshot() -> QuotaSnapshot {
    store().snapshot(SystemTime::now())
}

fn epoch_seconds(seconds: f64) -> Option<SystemTime> {
    (seconds.is_finite() && seconds > 0.)
        .then(|| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds))
}

// ---------------------------------------------------------------------------
// Claude: the statusline payload
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StatuslineInput {
    rate_limits: Option<ClaudeRateLimits>,
    cost: Option<ClaudeCost>,
}

#[derive(Deserialize)]
struct ClaudeRateLimits {
    five_hour: Option<ClaudeWindow>,
    seven_day: Option<ClaudeWindow>,
    spend_limit: Option<ClaudeWindow>,
}

#[derive(Deserialize)]
struct ClaudeWindow {
    used_percentage: Option<f64>,
    resets_at: Option<f64>,
}

#[derive(Deserialize)]
struct ClaudeCost {
    total_cost_usd: Option<f64>,
}

/// The quota in one statusline payload, or `None` when it is not one or
/// carries nothing to show.
pub fn parse_claude_statusline(json: &str, sampled_at: SystemTime) -> Option<VendorQuota> {
    let input: StatuslineInput = serde_json::from_str(json).ok()?;
    let mut bands = vec![];
    if let Some(limits) = input.rate_limits {
        for (window, reported) in [
            (QuotaWindow::FiveHour, limits.five_hour),
            (QuotaWindow::Weekly, limits.seven_day),
            (QuotaWindow::Spend, limits.spend_limit),
        ] {
            let Some(reported) = reported else { continue };
            let Some(used) = reported.used_percentage.filter(|used| used.is_finite()) else {
                continue;
            };
            bands.push(QuotaBand {
                window,
                used_percent: used as f32,
                resets_at: reported.resets_at.and_then(epoch_seconds),
            });
        }
    }
    let last_cost_usd = input
        .cost
        .and_then(|cost| cost.total_cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.);
    if bands.is_empty() && last_cost_usd.is_none() {
        return None;
    }
    sort_bands(&mut bands);
    Some(VendorQuota {
        vendor: AgentVendor::Claude,
        plan: None,
        bands,
        last_cost_usd,
        sampled_at,
    })
}

/// Read the Claude sample at `path` if it changed since the last read.
/// Returns whether the store changed.
///
/// Blocking file I/O: call it off the GUI thread.
pub fn poll_claude_sample(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    if store().claude_sample_mtime == Some(modified) || metadata.len() > MAX_SAMPLE_BYTES {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut store = store();
    store.claude_sample_mtime = Some(modified);
    match parse_claude_statusline(&text, modified) {
        Some(quota) => store.record(quota),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Codex: `rate_limits` on rollout `token_count` events
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CodexLine {
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    rate_limits: Option<CodexRateLimits>,
}

#[derive(Deserialize)]
struct CodexRateLimits {
    primary: Option<CodexWindow>,
    secondary: Option<CodexWindow>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: Option<f64>,
    window_minutes: Option<u32>,
    resets_at: Option<f64>,
    /// Older builds wrote a countdown instead of a time.
    resets_in_seconds: Option<f64>,
}

/// The quota in one rollout line, or `None` when the line has none — most
/// lines, and the events whose windows are both `null`.
pub fn parse_codex_line(line: &str) -> Option<VendorQuota> {
    if !line.contains("\"rate_limits\"") {
        return None;
    }
    let record: CodexLine = serde_json::from_str(line).ok()?;
    let payload = record.payload?;
    if payload.kind.as_deref() != Some("token_count") {
        return None;
    }
    let limits = payload.rate_limits?;
    let sampled_at = chrono::DateTime::parse_from_rfc3339(record.timestamp.as_deref()?)
        .ok()
        .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
        .map(|millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis))?;
    let mut bands: Vec<QuotaBand> = [limits.primary, limits.secondary]
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let used = window.used_percent.filter(|used| used.is_finite())?;
            let resets_at = window.resets_at.and_then(epoch_seconds).or_else(|| {
                window
                    .resets_in_seconds
                    .filter(|secs| secs.is_finite() && *secs >= 0.)
                    .map(|secs| sampled_at + Duration::from_secs_f64(secs))
            });
            Some(QuotaBand {
                window: QuotaWindow::from_minutes(window.window_minutes?),
                used_percent: used as f32,
                resets_at,
            })
        })
        .collect();
    if bands.is_empty() {
        return None;
    }
    sort_bands(&mut bands);
    Some(VendorQuota {
        vendor: AgentVendor::Codex,
        plan: limits.plan_type.filter(|plan| !plan.is_empty()),
        bands,
        last_cost_usd: None,
        sampled_at,
    })
}

/// Hand a rollout line to the store. Cheap for the lines without quota: a
/// substring test.
pub fn record_codex_line(line: &str) {
    if let Some(quota) = parse_codex_line(line) {
        store().record(quota);
    }
}

// ---------------------------------------------------------------------------
// Text helpers shared by the popup
// ---------------------------------------------------------------------------

/// `38m`, `2h 13m`, `4d 6h`; `<1m` under a minute.
pub fn countdown_text(remaining: Duration) -> String {
    let minutes = remaining.as_secs() / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        format!("{days}d {}h", hours % 24)
    } else if hours > 0 {
        format!("{hours}h {}m", minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "<1m".to_string()
    }
}

/// `now`, `2m ago`, `3h ago`, `2d ago`.
pub fn freshness_text(sampled_at: SystemTime, now: SystemTime) -> String {
    let age = now.duration_since(sampled_at).unwrap_or_default().as_secs();
    match age {
        age if age < 60 => "now".to_string(),
        age if age < 3600 => format!("{}m ago", age / 60),
        age if age < 86_400 => format!("{}h ago", age / 3600),
        age => format!("{}d ago", age / 86_400),
    }
}

/// `plus` → `Plus`.
pub fn plan_label(plan: &str) -> String {
    let mut chars = plan.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Where the relay leaves the last Claude statusline payload.
pub fn claude_sample_path(data_dir: &Path) -> PathBuf {
    data_dir.join(config::brand::state_file_name("claude-statusline", "json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_790_000_000;

    #[test]
    fn claude_payload_with_both_windows() {
        let json = r#"{
            "session_id": "abc",
            "cost": {"total_cost_usd": 3.41},
            "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": 1790003600},
                "seven_day": {"used_percentage": 41.2, "resets_at": 1790500000}
            }
        }"#;
        let quota = parse_claude_statusline(json, at(NOW)).unwrap();
        assert_eq!(quota.vendor, AgentVendor::Claude);
        assert_eq!(quota.bands.len(), 2);
        assert_eq!(quota.bands[0].window, QuotaWindow::FiveHour);
        assert_eq!(quota.bands[0].used_percent, 23.5);
        assert_eq!(quota.bands[0].resets_at, Some(at(1_790_003_600)));
        assert_eq!(quota.bands[1].window, QuotaWindow::Weekly);
        assert_eq!(quota.last_cost_usd, Some(3.41));
    }

    #[test]
    fn claude_payload_without_rate_limits_keeps_only_the_cost() {
        let quota = parse_claude_statusline(r#"{"cost":{"total_cost_usd":0.5}}"#, at(NOW)).unwrap();
        assert!(quota.bands.is_empty());
        assert_eq!(quota.last_cost_usd, Some(0.5));
        assert_eq!(parse_claude_statusline(r#"{"model":{}}"#, at(NOW)), None);
        assert_eq!(parse_claude_statusline("not json", at(NOW)), None);
    }

    #[test]
    fn claude_spend_limit_is_a_band() {
        let json = r#"{"rate_limits":{"spend_limit":{"used_percentage":62.8,"resets_at":1790787200,"used_usd":314.12,"limit_usd":500,"period":"monthly"}}}"#;
        let quota = parse_claude_statusline(json, at(NOW)).unwrap();
        assert_eq!(quota.bands[0].window, QuotaWindow::Spend);
    }

    fn codex_line(primary: &str, secondary: &str) -> String {
        format!(
            r#"{{"timestamp":"2026-07-03T14:35:36.935Z","type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex","primary":{primary},"secondary":{secondary},"credits":null,"plan_type":"plus"}}}}}}"#
        )
    }

    #[test]
    fn codex_line_maps_window_minutes() {
        let line = codex_line(
            r#"{"used_percent":12.0,"window_minutes":300,"resets_at":1783106790}"#,
            r#"{"used_percent":27.0,"window_minutes":10080,"resets_at":1783556356}"#,
        );
        let quota = parse_codex_line(&line).unwrap();
        assert_eq!(quota.vendor, AgentVendor::Codex);
        assert_eq!(quota.plan.as_deref(), Some("plus"));
        assert_eq!(quota.bands[0].window, QuotaWindow::FiveHour);
        assert_eq!(quota.bands[0].used_percent, 12.0);
        assert_eq!(quota.bands[1].window, QuotaWindow::Weekly);
        assert_eq!(quota.bands[1].resets_at, Some(at(1_783_556_356)));
    }

    #[test]
    fn codex_line_with_null_windows_has_no_quota() {
        assert_eq!(parse_codex_line(&codex_line("null", "null")), None);
        assert_eq!(parse_codex_line(r#"{"type":"event_msg"}"#), None);
    }

    #[test]
    fn codex_countdown_form_is_read_relative_to_the_record() {
        let line = codex_line(
            r#"{"used_percent":5.0,"window_minutes":300,"resets_in_seconds":60}"#,
            "null",
        );
        let quota = parse_codex_line(&line).unwrap();
        assert_eq!(
            quota.bands[0].resets_at,
            Some(quota.sampled_at + Duration::from_secs(60))
        );
    }

    fn quota(vendor: AgentVendor, sampled: u64, bands: Vec<QuotaBand>) -> VendorQuota {
        VendorQuota {
            vendor,
            plan: None,
            bands,
            last_cost_usd: None,
            sampled_at: at(sampled),
        }
    }

    fn band(window: QuotaWindow, used: f32, resets: u64) -> QuotaBand {
        QuotaBand {
            window,
            used_percent: used,
            resets_at: Some(at(resets)),
        }
    }

    #[test]
    fn store_keeps_the_newest_sample_and_windows_through_an_empty_one() {
        let mut store = QuotaStore::default();
        let five = band(QuotaWindow::FiveHour, 10., NOW + 100);
        assert!(store.record(quota(AgentVendor::Claude, NOW, vec![five.clone()])));
        // Older: ignored.
        assert!(!store.record(quota(AgentVendor::Claude, NOW - 10, vec![])));
        // Newer without windows: keeps the windows.
        assert!(store.record(VendorQuota {
            last_cost_usd: Some(1.0),
            ..quota(AgentVendor::Claude, NOW + 5, vec![])
        }));
        let snap = store.snapshot(at(NOW + 6));
        assert_eq!(snap.vendors[0].bands, vec![five]);
        assert_eq!(snap.vendors[0].last_cost_usd, Some(1.0));
        // The same sample again changes nothing.
        let same = snap.vendors[0].clone();
        assert!(!store.record(same));
    }

    #[test]
    fn store_orders_claude_first() {
        let mut store = QuotaStore::default();
        store.record(quota(
            AgentVendor::Codex,
            NOW,
            vec![band(QuotaWindow::FiveHour, 1., NOW + 9)],
        ));
        store.record(quota(
            AgentVendor::Claude,
            NOW,
            vec![band(QuotaWindow::FiveHour, 1., NOW + 9)],
        ));
        let snap = store.snapshot(at(NOW));
        assert_eq!(snap.vendors[0].vendor, AgentVendor::Claude);
        assert_eq!(snap.vendors[1].vendor, AgentVendor::Codex);
    }

    #[test]
    fn band_state_and_fraction_follow_the_reset_time() {
        let b = band(QuotaWindow::FiveHour, 47., NOW + 7980);
        assert_eq!(
            b.state(at(NOW)),
            BandState::Live {
                remaining: Some(Duration::from_secs(7980))
            }
        );
        assert!((b.fraction(at(NOW)) - 0.47).abs() < 1e-6);
        assert_eq!(b.state(at(NOW + 7980)), BandState::Reset);
        assert_eq!(b.fraction(at(NOW + 9000)), 0.);
    }

    #[test]
    fn priority_band_is_the_fullest_live_window_of_the_first_vendor() {
        let mut store = QuotaStore::default();
        store.record(quota(
            AgentVendor::Claude,
            NOW,
            vec![
                band(QuotaWindow::FiveHour, 10., NOW + 100),
                band(QuotaWindow::Weekly, 90., NOW + 100_000),
            ],
        ));
        store.record(quota(
            AgentVendor::Codex,
            NOW,
            vec![band(QuotaWindow::FiveHour, 99., NOW + 100)],
        ));
        let snap = store.snapshot(at(NOW));
        let (vendor, picked) = snap.priority_band(at(NOW)).unwrap();
        assert_eq!(vendor.vendor, AgentVendor::Claude);
        assert_eq!(picked.window, QuotaWindow::Weekly);
        // Once Claude's windows have all reset, Codex is next.
        let later = at(NOW + 200_000);
        let snap = store.snapshot(later);
        assert_eq!(snap.priority_band(later), None);
        let between = at(NOW + 50);
        let mut store2 = QuotaStore::default();
        store2.record(quota(
            AgentVendor::Claude,
            NOW,
            vec![band(QuotaWindow::FiveHour, 10., NOW + 10)],
        ));
        store2.record(quota(
            AgentVendor::Codex,
            NOW,
            vec![band(QuotaWindow::FiveHour, 99., NOW + 100)],
        ));
        let snap = store2.snapshot(between);
        assert_eq!(
            snap.priority_band(between).unwrap().0.vendor,
            AgentVendor::Codex
        );
    }

    #[test]
    fn stale_vendors_are_left_out() {
        let mut store = QuotaStore::default();
        store.record(quota(
            AgentVendor::Codex,
            NOW,
            vec![band(QuotaWindow::FiveHour, 1., NOW + 10)],
        ));
        assert_eq!(store.snapshot(at(NOW + 3600)).vendors.len(), 1);
        assert!(store.snapshot(at(NOW + 9 * 24 * 3600)).vendors.is_empty());
    }

    #[test]
    fn text_helpers() {
        assert_eq!(countdown_text(Duration::from_secs(30)), "<1m");
        assert_eq!(countdown_text(Duration::from_secs(38 * 60)), "38m");
        assert_eq!(
            countdown_text(Duration::from_secs(2 * 3600 + 13 * 60)),
            "2h 13m"
        );
        assert_eq!(
            countdown_text(Duration::from_secs(4 * 86_400 + 6 * 3600)),
            "4d 6h"
        );
        assert_eq!(freshness_text(at(NOW), at(NOW + 5)), "now");
        assert_eq!(freshness_text(at(NOW), at(NOW + 150)), "2m ago");
        assert_eq!(freshness_text(at(NOW), at(NOW + 7200)), "2h ago");
        assert_eq!(plan_label("plus"), "Plus");
        assert_eq!(QuotaWindow::Other(1440).label(), "1d window");
        assert_eq!(QuotaWindow::Other(120).label(), "2h window");
        assert_eq!(gauge_level(10.), GaugeLevel::Ok);
        assert_eq!(gauge_level(60.), GaugeLevel::Warn);
        assert_eq!(gauge_level(85.), GaugeLevel::Critical);
    }

    #[test]
    fn claude_sample_file_is_read_once_per_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sample.json");
        std::fs::write(
            &path,
            r#"{"rate_limits":{"five_hour":{"used_percentage":5,"resets_at":4102444800}}}"#,
        )
        .unwrap();
        // The global store: other tests never record Claude through it.
        poll_claude_sample(&path);
        assert!(!poll_claude_sample(&path));
        let snap = snapshot();
        let claude = snap.vendor(&AgentVendor::Claude).unwrap();
        assert_eq!(claude.bands[0].used_percent, 5.);
    }
}
