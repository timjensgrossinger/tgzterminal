//! What Tower did, per local day, for the Activity view: flights started by
//! origin, actions that ran without a click, Inbox asks, outcomes and spend.
//!
//! Counters live in `state.json` (`Persisted.daily`, 90 days). The first
//! start with none folds the existing flights and `audit.jsonl` in, so the
//! charts are not empty on day one.

use jiff::tz::TimeZone;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use tower_proto::{AutoEvent, DayStats, Flight};

/// Days of counters kept.
pub const KEEP_DAYS: usize = 90;
/// Automatic events kept for the "Ran by itself" list.
pub const RECENT_CAP: usize = 50;

pub type Daily = BTreeMap<String, DayStats>;

/// Change the counters of `day`, creating it, and drop days past `KEEP_DAYS`.
pub fn bump(daily: &mut Daily, day: &str, f: impl FnOnce(&mut DayStats)) {
    let d = daily.entry(day.to_string()).or_insert_with(|| DayStats {
        day: day.to_string(),
        ..Default::default()
    });
    f(d);
    prune(daily);
}

/// Drop days past `KEEP_DAYS`, oldest first.
pub fn prune(daily: &mut Daily) {
    while daily.len() > KEEP_DAYS {
        let oldest = daily.keys().next().cloned().unwrap_or_default();
        daily.remove(&oldest);
    }
}

pub fn add(map: &mut BTreeMap<String, u32>, key: &str) {
    *map.entry(key.to_string()).or_insert(0) += 1;
}

/// `schedule:nightly` -> `schedule`; unset (older flights) -> `you`.
pub fn origin(started_by: Option<&str>) -> &str {
    match started_by {
        Some(s) => s.split(':').next().unwrap_or(s),
        None => "you",
    }
}

/// Origin of a flight from before `started_by` was recorded.
pub fn guess_origin(f: &Flight) -> &'static str {
    match f.source.as_deref() {
        Some("handoff") => "handoff",
        Some("scout") | Some("mail") | Some("teams") => "scout",
        Some("local") | None => "you",
        Some(_) if f.kind.is_some() => "autofix",
        Some(_) => "intake",
    }
}

/// The last `n` days up to and including `now`'s, oldest first; days with
/// nothing are filled in empty.
pub fn window(daily: &Daily, now: u64, n: usize, tz: &TimeZone) -> Vec<DayStats> {
    (0..n as u64)
        .rev()
        .map(|i| {
            // Noon steps over DST changes without landing on the wrong day.
            let day = crate::clock::day_key(noon(now, tz).saturating_sub(i * 86_400), tz);
            daily.get(&day).cloned().unwrap_or(DayStats {
                day,
                ..Default::default()
            })
        })
        .collect()
}

fn noon(now: u64, tz: &TimeZone) -> u64 {
    let c = crate::clock::civil(now, tz);
    let start = now - u64::from(c.hour) * 3600 - u64::from(c.minute) * 60 - now % 60;
    start + 12 * 3600
}

/// One `audit.jsonl` line, as `audit::Entry` writes it.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct AuditLine {
    pub ts: u64,
    pub action: String,
    pub level: String,
    pub decision: String,
    #[serde(default)]
    pub flight: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub outcome: String,
}

impl AuditLine {
    /// A decision that ran on its own (as opposed to after a click).
    pub fn ran_by_itself(&self) -> bool {
        self.decision == "run" && self.level == "auto" && self.outcome == "started"
    }

    pub fn event(&self) -> AutoEvent {
        AutoEvent {
            at_unix: self.ts,
            action: self.action.clone(),
            flight: self.flight.clone(),
            target: self.target.clone(),
            outcome: self.outcome.clone(),
        }
    }
}

/// Every readable line of `audit.jsonl.1` then `audit.jsonl`, oldest first.
pub fn read_audit(data_dir: &Path) -> Vec<AuditLine> {
    let mut out = Vec::new();
    for name in ["audit.jsonl.1", "audit.jsonl"] {
        let Ok(text) = std::fs::read_to_string(data_dir.join(name)) else {
            continue;
        };
        out.extend(
            text.lines()
                .filter_map(|l| serde_json::from_str::<AuditLine>(l).ok()),
        );
    }
    out
}

/// The newest automatic events, newest first.
pub fn recent(lines: &[AuditLine]) -> Vec<AutoEvent> {
    lines
        .iter()
        .rev()
        .filter(|l| l.ran_by_itself())
        .take(RECENT_CAP)
        .map(AuditLine::event)
        .collect()
}

/// Counters rebuilt from what is still on disk.
pub fn backfill(flights: &[Flight], audit: &[AuditLine], tz: &TimeZone) -> Daily {
    let mut daily = Daily::new();
    for f in flights {
        let day = crate::clock::day_key(f.created_unix, tz);
        let origin = match f.started_by.as_deref() {
            Some(s) => origin(Some(s)).to_string(),
            None => guess_origin(f).to_string(),
        };
        bump(&mut daily, &day, |d| {
            add(&mut d.started, &origin);
            d.cost_usd += f.usage.cost_usd.unwrap_or(0.0);
            d.tokens += f.usage.total();
        });
        // The same split `land()` records live: a landed proposal counts
        // as proposed.
        let ended = match (f.stage, f.outcome.as_deref()) {
            (_, Some("proposed")) => Some("proposed"),
            (tower_proto::Stage::Landed, _) => Some("landed"),
            (_, Some("declined")) => Some("declined"),
            _ => None,
        };
        if let Some(what) = ended {
            let day = crate::clock::day_key(f.landed_unix.unwrap_or(f.updated_unix), tz);
            bump(&mut daily, &day, |d| add(&mut d.outcomes, what));
        }
    }
    for l in audit {
        let day = crate::clock::day_key(l.ts, tz);
        if l.ran_by_itself() {
            bump(&mut daily, &day, |d| add(&mut d.auto, &l.action));
        } else if l.decision == "ask" {
            bump(&mut daily, &day, |d| d.asked += 1);
        }
    }
    daily
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_proto::{Stage, Usage};

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Vienna").unwrap()
    }

    // 2026-10-24 12:00 CEST.
    const NOW: u64 = 1_792_836_000;

    #[test]
    fn bump_keeps_at_most_ninety_days() {
        let mut d = Daily::new();
        for i in 0..100u64 {
            let day = crate::clock::day_key(NOW - i * 86_400, &tz());
            bump(&mut d, &day, |s| s.asked += 1);
        }
        assert_eq!(d.len(), KEEP_DAYS);
        assert_eq!(d.keys().last().unwrap(), "2026-10-24");
    }

    #[test]
    fn window_fills_empty_days_oldest_first() {
        let mut d = Daily::new();
        bump(&mut d, "2026-10-23", |s| add(&mut s.started, "you"));
        let w = window(&d, NOW, 3, &tz());
        let days: Vec<_> = w.iter().map(|s| s.day.as_str()).collect();
        assert_eq!(days, ["2026-10-22", "2026-10-23", "2026-10-24"]);
        assert_eq!(w[1].started["you"], 1);
        assert!(w[0].started.is_empty());
    }

    #[test]
    fn origins() {
        assert_eq!(origin(Some("schedule:nightly")), "schedule");
        assert_eq!(origin(Some("you")), "you");
        assert_eq!(origin(None), "you");
    }

    #[test]
    fn backfill_folds_flights_and_audit() {
        let flights = vec![
            Flight {
                id: "f-1".into(),
                source: Some("local".into()),
                created_unix: NOW,
                updated_unix: NOW,
                stage: Stage::Landed,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cost_usd: Some(0.5),
                },
                ..Default::default()
            },
            Flight {
                id: "f-2".into(),
                source: Some("jira".into()),
                kind: Some("code".into()),
                created_unix: NOW,
                updated_unix: NOW,
                outcome: Some("declined".into()),
                ..Default::default()
            },
        ];
        let line = |decision: &str, level: &str, outcome: &str| AuditLine {
            ts: NOW,
            action: "ack.post".into(),
            level: level.into(),
            decision: decision.into(),
            flight: None,
            target: Some("PROJ-1".into()),
            outcome: outcome.into(),
        };
        let audit = vec![
            line("run", "auto", "started"),
            line("ask", "auto", "asked"),
            // A result line after a click is not "by itself".
            line("run", "confirm", "done"),
        ];
        let d = backfill(&flights, &audit, &tz());
        let day = &d["2026-10-24"];
        assert_eq!(day.started["you"], 1);
        assert_eq!(day.started["autofix"], 1);
        assert_eq!(day.outcomes["landed"], 1);
        assert_eq!(day.outcomes["declined"], 1);
        assert_eq!(day.auto["ack.post"], 1);
        assert_eq!(day.asked, 1);
        assert_eq!(day.tokens, 15);
        assert!((day.cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(recent(&audit).len(), 1);
    }

    #[test]
    fn audit_lines_parse_as_written() {
        let e = crate::audit::Entry::new(
            crate::policy::Action::AckPost,
            crate::policy::Level::Auto,
            crate::policy::Decision::Run,
            "started",
        );
        let line: AuditLine = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert!(line.ran_by_itself());
        assert_eq!(line.action, "ack.post");
    }
}
