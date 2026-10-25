//! Running schedules: fire on time, skip while the last run is still open,
//! ask instead when `schedule.run` says so, and pause after failed runs.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::policy::{Action, Decision, Level, Overrides};
use crate::schedule::{self, Timing};
use crate::store::{PendingAction, ScheduleState, RECENT_RUNS};
use tower_proto::{RunMark, RunResult, ScheduleRow, ScheduleSpec};

impl Engine {
    pub(super) fn schedule_spec(&self, id: &str) -> Option<ScheduleSpec> {
        self.cfg.schedules.iter().find(|s| s.id == id).cloned()
    }

    fn schedule_state(&mut self, id: &str) -> &mut ScheduleState {
        self.st.schedules.entry(id.to_string()).or_default()
    }

    /// After a start or a config reload: work out every next fire time. A
    /// fire time that passed while towerd was down runs now with `catch_up`,
    /// otherwise it is recorded as missed.
    pub(super) fn schedules_rearm(&mut self, startup: bool) {
        let now = now_unix();
        for spec in self.cfg.schedules.clone() {
            let Ok(timing) = Timing::of(&spec) else {
                continue;
            };
            let tz = self.tz.clone();
            let key = timing_key(&spec);
            let st = self.schedule_state(&spec.id);
            let missed = st.next_fire.filter(|t| *t <= now);
            // A planned time still ahead stays, unless the timing changed:
            // replanning from now would push an interval back on every reload.
            let keep = st.next_fire.filter(|t| *t > now && st.planned_for == key);
            st.next_fire = match (missed, keep) {
                (_, Some(t)) => Some(t),
                // Due on this very tick: a reload must not swallow it.
                (Some(t), _) if !startup => Some(t),
                (Some(t), _) if spec.catch_up && spec.enabled => Some(t),
                _ => timing.next(now, st.last_fire, &tz),
            };
            st.planned_for = key;
            if let (true, Some(t), false) = (startup, missed, spec.catch_up) {
                if spec.enabled {
                    self.schedule_mark(
                        &spec.id,
                        RunMark {
                            at_unix: t,
                            result: RunResult::Skipped,
                            flight: None,
                            note: "missed: Tower was not running".into(),
                        },
                    );
                }
            }
        }
        self.changed();
    }

    /// Called every tick: fire whatever is due.
    pub(super) fn schedules_due(&mut self) {
        // Demo data on screen must never start real flights.
        if self.demo.is_some() {
            return;
        }
        let now = now_unix();
        for spec in self.cfg.schedules.clone() {
            if !spec.enabled {
                continue;
            }
            let Ok(timing) = Timing::of(&spec) else {
                continue;
            };
            let tz = self.tz.clone();
            let st = self.schedule_state(&spec.id);
            if st.paused_reason.is_some() {
                continue;
            }
            let due = match st.next_fire {
                Some(t) => t,
                None => {
                    st.next_fire = timing.next(now, st.last_fire, &tz);
                    st.planned_for = timing_key(&spec);
                    self.changed();
                    continue;
                }
            };
            if now < due {
                continue;
            }
            // Keep an interval on its grid: count from the planned time.
            st.next_fire = timing.next(now, Some(due), &tz);
            self.changed();
            self.schedule_fire_due(&spec, due);
        }
    }

    fn schedule_overrides(spec: &ScheduleSpec) -> Overrides {
        let mut o = Overrides::new();
        let level = match spec.policy.as_deref() {
            Some("off") => Some(Level::Off),
            Some("confirm") => Some(Level::Confirm),
            Some("auto") => Some(Level::Auto),
            _ => None,
        };
        if let Some(l) = level {
            o.insert(Action::ScheduleRun.key().to_string(), l);
        }
        o
    }

    /// Is the flight of this schedule's last run still open? A run already
    /// counted as failed does not hold the next one back, or a flight stuck
    /// waiting for you would silence the schedule for good.
    fn schedule_running(&self, id: &str) -> bool {
        let failed = |fid: &str| {
            self.st.schedules.get(id).is_some_and(|st| {
                st.recent
                    .iter()
                    .any(|m| m.flight.as_deref() == Some(fid) && m.result.is_failure())
            })
        };
        self.st
            .flight_schedule
            .iter()
            .filter(|(_, s)| s.as_str() == id)
            .any(|(fid, _)| {
                self.flight(fid).is_some_and(|f| f.stage != Stage::Landed) && !failed(fid)
            })
    }

    fn schedule_ask_pending(&self, id: &str) -> bool {
        self.st
            .pending_actions
            .values()
            .any(|p| matches!(p, PendingAction::Schedule { id: i } if i == id))
    }

    fn schedule_fire_due(&mut self, spec: &ScheduleSpec, due: u64) {
        let skipped = |note: &str| RunMark {
            at_unix: due,
            result: RunResult::Skipped,
            flight: None,
            note: note.to_string(),
        };
        if spec.overlap != "run" && self.schedule_running(&spec.id) {
            self.schedule_mark(&spec.id, skipped("the last run is still open"));
            return;
        }
        if self.schedule_ask_pending(&spec.id) {
            // One question per schedule is enough.
            return;
        }
        let overrides = Self::schedule_overrides(spec);
        match self.decide_action(
            Action::ScheduleRun,
            Some(&overrides),
            false,
            None,
            Some(&spec.id),
        ) {
            Decision::Run => {
                let _ = self.fire_schedule(&spec.id, false);
            }
            Decision::Ask => {
                let id = self.add_inbox(InboxItem {
                    kind: InboxKind::Clearance,
                    title: format!("Run schedule \"{}\"?", spec.name),
                    detail: format!(
                        "{} with {}: {}",
                        Timing::of(spec).map(|t| t.describe()).unwrap_or_default(),
                        self.schedule_role_label(spec),
                        shorten(&spec.brief, 200)
                    ),
                    options: vec![
                        InboxOption {
                            id: "run".into(),
                            label: "Run".into(),
                            primary: true,
                        },
                        InboxOption {
                            id: "not_now".into(),
                            label: "Not now".into(),
                            primary: false,
                        },
                    ],
                    ..Default::default()
                });
                self.st.pending_actions.insert(
                    id,
                    PendingAction::Schedule {
                        id: spec.id.clone(),
                    },
                );
                self.schedule_mark(&spec.id, skipped("waiting for you in the Inbox"));
            }
            Decision::Draft | Decision::Skip => {
                self.schedule_mark(&spec.id, skipped("schedule.run is off"));
            }
        }
    }

    fn schedule_role_label(&self, spec: &ScheduleSpec) -> String {
        if spec.role == "auto" {
            "Auto (triage picks)".into()
        } else {
            self.role_name(&spec.role)
        }
    }

    /// Start a flight from schedule `id` now. `by_human`: Run now, or Run
    /// in the Inbox.
    pub(super) fn fire_schedule(&mut self, id: &str, by_human: bool) -> Res {
        let spec = self
            .schedule_spec(id)
            .ok_or_else(|| format!("No schedule {id:?}"))?;
        let now = now_unix();
        let since = self.st.schedules.get(id).and_then(|s| s.last_success);
        let title_tpl = if spec.title.trim().is_empty() {
            spec.name.clone()
        } else {
            spec.title.clone()
        };
        let title = schedule::fill(&title_tpl, now, since, &self.tz);
        let brief = schedule::fill(&spec.brief, now, since, &self.tz);
        let path = spec.path.as_deref().map(schedule::expand);
        let (workdir, worktree, new_folder) = match spec.place.as_str() {
            "dir" => (path, false, false),
            "worktree" => (path, true, false),
            _ => (None, false, true),
        };
        let auto = spec.role == "auto";
        let role_id = (!auto).then(|| spec.role.clone());
        let started_by = format!("schedule:{id}");
        let res = self.create_flight(
            title,
            brief,
            role_id,
            workdir,
            worktree,
            new_folder,
            auto,
            true,
            spec.tier.clone(),
            spec.local_only,
            &started_by,
        );
        // A failed launch can still leave its flight behind.
        let fid = match &res {
            Ok(fid) => Some(fid.clone()),
            Err(_) => self
                .st
                .flights
                .iter()
                .rev()
                .find(|f| f.started_by.as_deref() == Some(started_by.as_str()))
                .filter(|f| f.created_unix >= now)
                .map(|f| f.id.clone()),
        };
        self.schedule_state(id).last_fire = Some(now);
        if let Some(fid) = &fid {
            self.st.flight_schedule.insert(fid.clone(), id.to_string());
            let how = if by_human { "Run now" } else { "On schedule" };
            self.note(fid, format!("{how}: schedule \"{}\"", spec.name));
        }
        if by_human {
            self.audit_result(Action::ScheduleRun, fid.as_deref(), Some(id), &res);
        }
        match &res {
            Ok(_) => self.schedule_mark(
                id,
                RunMark {
                    at_unix: now,
                    result: RunResult::Running,
                    flight: fid,
                    note: String::new(),
                },
            ),
            Err(e) => {
                self.post_system(
                    &format!("Schedule \"{}\" could not start: {e}", spec.name),
                    now,
                );
                self.schedule_mark(
                    id,
                    RunMark {
                        at_unix: now,
                        result: RunResult::Failed,
                        flight: fid,
                        note: shorten(e, 200),
                    },
                );
            }
        }
        res
    }

    /// Add a run to the schedule's history, counting failures in a row.
    fn schedule_mark(&mut self, id: &str, mark: RunMark) {
        let result = mark.result;
        let at = mark.at_unix;
        let st = self.schedule_state(id);
        st.recent.push(mark);
        if st.recent.len() > RECENT_RUNS {
            let drop = st.recent.len() - RECENT_RUNS;
            st.recent.drain(..drop);
        }
        self.schedule_counted(id, result, at);
    }

    /// Update failures in a row (and pause) for a run that ended `result`.
    fn schedule_counted(&mut self, id: &str, result: RunResult, at: u64) {
        let limit = self
            .schedule_spec(id)
            .map(|s| s.pause_after_failures)
            .unwrap_or(0);
        let name = self.schedule_spec(id).map(|s| s.name).unwrap_or_default();
        let st = self.schedule_state(id);
        match result {
            RunResult::Landed | RunResult::Proposed => {
                st.fail_streak = 0;
                st.last_success = Some(at);
            }
            r if r.is_failure() => st.fail_streak += 1,
            _ => {}
        }
        let pause = limit > 0 && st.fail_streak >= limit && st.paused_reason.is_none();
        if pause {
            let why = format!("{} failed runs in a row", st.fail_streak);
            st.paused_reason = Some(why.clone());
            let item = self.add_inbox(InboxItem {
                kind: InboxKind::Clearance,
                title: format!("Schedule \"{name}\" is paused"),
                detail: format!(
                    "{why}. Check its last flight, then resume it here or in Routines."
                ),
                options: vec![
                    InboxOption {
                        id: "run".into(),
                        label: "Resume".into(),
                        primary: true,
                    },
                    InboxOption {
                        id: "not_now".into(),
                        label: "Leave paused".into(),
                        primary: false,
                    },
                ],
                ..Default::default()
            });
            self.st
                .pending_actions
                .insert(item, PendingAction::ResumeSchedule { id: id.to_string() });
        }
        self.changed();
    }

    /// A flight a schedule started ended (or failed a gate): record it on
    /// the run.
    pub(super) fn schedule_flight_result(
        &mut self,
        flight_id: &str,
        result: RunResult,
        note: &str,
    ) {
        let Some(id) = self.st.flight_schedule.get(flight_id).cloned() else {
            return;
        };
        let now = now_unix();
        let st = self.schedule_state(&id);
        let Some(m) = st
            .recent
            .iter_mut()
            .rev()
            .find(|m| m.flight.as_deref() == Some(flight_id))
        else {
            return;
        };
        if m.result == result {
            return;
        }
        // One bad run counts once, however many ways it failed.
        let counted_already = m.result.is_failure() && result.is_failure();
        m.result = result;
        m.note = note.to_string();
        if matches!(
            result,
            RunResult::Landed | RunResult::Proposed | RunResult::Declined
        ) {
            // Done either way; a later gate retry no longer counts.
            self.st.flight_schedule.remove(flight_id);
        }
        if !counted_already {
            self.schedule_counted(&id, result, now);
        }
    }

    // -- from the Tower view ------------------------------------------------

    pub(super) fn save_schedule(&mut self, mut spec: ScheduleSpec) -> Res {
        spec.name = spec.name.trim().to_string();
        spec.role = spec.role.trim().to_string();
        let cfg = &self.cfg;
        schedule::validate(&spec, &|r| cfg.role(r).is_some())?;
        let mut list = schedule::load(&self.paths.config)?;
        if spec.id.is_empty() {
            let taken: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
            spec.id = schedule::fresh_id(&spec.name, &taken);
            list.push(spec.clone());
        } else if let Some(s) = list.iter_mut().find(|s| s.id == spec.id) {
            *s = spec.clone();
        } else {
            list.push(spec.clone());
        }
        schedule::save(&self.paths.config, &list)?;
        // Timing may have changed: plan from now. Editing a paused schedule
        // is how you fix it, so it also resumes.
        let st = self.schedule_state(&spec.id);
        st.next_fire = None;
        st.paused_reason = None;
        st.fail_streak = 0;
        self.reload();
        Ok(spec.id)
    }

    pub(super) fn delete_schedule(&mut self, id: &str) -> Res {
        let mut list = schedule::load(&self.paths.config)?;
        let before = list.len();
        list.retain(|s| s.id != id);
        if list.len() == before {
            return Err(format!("No schedule {id:?}"));
        }
        schedule::save(&self.paths.config, &list)?;
        self.st.schedules.remove(id);
        self.st.flight_schedule.retain(|_, s| s != id);
        self.st
            .pending_actions
            .retain(|_, p| !matches!(p, PendingAction::Schedule { id: i } | PendingAction::ResumeSchedule { id: i } if i == id));
        self.reload();
        Ok(String::new())
    }

    pub(super) fn set_schedule_enabled(&mut self, id: &str, enabled: bool) -> Res {
        let mut list = schedule::load(&self.paths.config)?;
        let s = list
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("No schedule {id:?}"))?;
        s.enabled = enabled;
        schedule::save(&self.paths.config, &list)?;
        let st = self.schedule_state(id);
        if enabled {
            st.paused_reason = None;
            st.fail_streak = 0;
        }
        st.next_fire = None;
        self.reload();
        Ok(if enabled { "Resumed" } else { "Paused" }.into())
    }

    pub(super) fn schedule_rows(&self) -> Vec<ScheduleRow> {
        let mut rows: Vec<ScheduleRow> = self
            .cfg
            .schedules
            .iter()
            .map(|spec| {
                let st = self.st.schedules.get(&spec.id).cloned().unwrap_or_default();
                let timing = Timing::of(spec);
                let role_err = (spec.role != "auto" && self.cfg.role(&spec.role).is_none())
                    .then(|| format!("Unknown role {}", spec.role));
                ScheduleRow {
                    spec: spec.clone(),
                    when_text: timing.as_ref().map(|t| t.describe()).unwrap_or_default(),
                    next_fire_unix: if spec.enabled && st.paused_reason.is_none() {
                        st.next_fire
                    } else {
                        None
                    },
                    last_fire_unix: st.last_fire,
                    recent: st.recent.clone(),
                    paused_reason: st.paused_reason.clone(),
                    error: timing.err().or(role_err),
                }
            })
            .collect();
        rows.sort_by(|a, b| a.spec.name.to_lowercase().cmp(&b.spec.name.to_lowercase()));
        rows
    }
}

/// What a planned fire time was computed from.
fn timing_key(spec: &ScheduleSpec) -> String {
    format!(
        "{}|{}",
        spec.cron.as_deref().unwrap_or(""),
        spec.every.as_deref().unwrap_or("")
    )
}
