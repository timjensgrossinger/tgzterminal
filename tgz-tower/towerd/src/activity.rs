//! The engine's side of the Activity and Routines views: daily counters,
//! the "ran by itself" list, and re-reading launchd and the crontab.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::history;
use crate::policy::{Action, Decision, Level};
use tower_proto::{ActivityStats, AutoEvent, RunResult};

/// How often launchd and the crontab are re-read.
const ROUTINES_EVERY: Duration = Duration::from_secs(60);

impl Engine {
    /// Once per start: rebuild the counters if there are none yet, and the
    /// "ran by itself" list from the audit log.
    pub(super) fn history_startup(&mut self) {
        if self.demo.is_some() {
            return;
        }
        let audit = history::read_audit(&self.paths.data);
        if !self.st.history_backfilled {
            let mut daily = history::backfill(&self.st.flights, &audit, &self.tz);
            // Keep anything already counted (there should be nothing).
            for (day, d) in std::mem::take(&mut self.st.daily) {
                daily.insert(day, d);
            }
            history::prune(&mut daily);
            self.st.daily = daily;
            self.st.history_backfilled = true;
            self.changed();
        }
        self.auto_recent = history::recent(&audit).into();
    }

    fn today(&self) -> String {
        crate::clock::day_key(now_unix(), &self.tz)
    }

    /// Count one policy decision (from `decide_action`).
    pub(super) fn count_decision(
        &mut self,
        action: Action,
        level: Level,
        decision: Decision,
        flight: Option<&str>,
        target: Option<&str>,
    ) {
        if self.demo.is_some() {
            return;
        }
        let day = self.today();
        match (level, decision) {
            (Level::Auto, Decision::Run) => {
                history::bump(&mut self.st.daily, &day, |d| {
                    history::add(&mut d.auto, action.key())
                });
                self.auto_recent.push_front(AutoEvent {
                    at_unix: now_unix(),
                    action: action.key().to_string(),
                    flight: flight.map(str::to_string),
                    target: target.map(str::to_string),
                    outcome: "started".into(),
                });
                self.auto_recent.truncate(history::RECENT_CAP);
            }
            (_, Decision::Ask) => history::bump(&mut self.st.daily, &day, |d| d.asked += 1),
            _ => {}
        }
        self.changed();
    }

    /// Count a new flight under its origin.
    pub(super) fn record_started(&mut self, started_by: &str) {
        if self.demo.is_some() {
            return;
        }
        let day = self.today();
        let origin = history::origin(Some(started_by)).to_string();
        history::bump(&mut self.st.daily, &day, |d| {
            history::add(&mut d.started, &origin)
        });
    }

    /// Count how a flight ended (`landed`, `proposed`, `declined`,
    /// `gates_failed`, `escalated`), and tell its schedule, if any.
    pub(super) fn record_outcome(&mut self, flight_id: &str, what: &str) {
        if self.demo.is_some() {
            return;
        }
        let day = self.today();
        history::bump(&mut self.st.daily, &day, |d| {
            history::add(&mut d.outcomes, what)
        });
        let run = match what {
            "landed" => Some((RunResult::Landed, "")),
            "proposed" => Some((RunResult::Proposed, "")),
            "declined" => Some((RunResult::Declined, "the agent declined it")),
            "gates_failed" => Some((RunResult::Failed, "a gate failed")),
            _ => None,
        };
        if let Some((r, note)) = run {
            self.schedule_flight_result(flight_id, r, note);
        }
        self.changed();
    }

    /// Count tokens and spend on the day they were used.
    pub(super) fn record_usage(&mut self, u: &Usage) {
        let day = self.today();
        history::bump(&mut self.st.daily, &day, |d| {
            d.tokens += u.total();
            d.cost_usd += u.cost_usd.unwrap_or(0.0);
        });
    }

    pub(super) fn activity_stats(&self) -> ActivityStats {
        ActivityStats {
            days: history::window(&self.st.daily, now_unix(), history::KEEP_DAYS, &self.tz),
            recent: self.auto_recent.iter().cloned().collect(),
        }
    }

    /// Re-read launchd and the crontab off the engine task.
    pub(super) fn refresh_routines(&mut self) {
        if self.routines_busy || self.demo.is_some() || !self.scan_enabled {
            return;
        }
        self.routines_busy = true;
        self.last_routines = Some(Instant::now());
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let found = crate::routines::collect(&home, now_unix());
            let _ = tx.send(Input::Routines(found));
        });
    }

    pub(super) fn routines_due(&mut self) {
        if !self.cfg.extras.machine_routines {
            return;
        }
        if self
            .last_routines
            .map(|t| t.elapsed() > ROUTINES_EVERY)
            .unwrap_or(true)
        {
            self.refresh_routines();
        }
    }

    pub(super) fn on_routines(&mut self, found: crate::routines::Collected) {
        self.routines_busy = false;
        if found.rows != self.routines || found.note != self.routines_note {
            self.routines = found.rows;
            self.routines_note = found.note;
            self.changed();
        }
    }
}
