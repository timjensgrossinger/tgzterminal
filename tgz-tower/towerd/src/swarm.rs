//! The swarm role ("Split & run"): Tower splits a task into parts itself
//! and runs them as a fan-out, so no lead agent has to.
//!
//! The split comes from the triage model when one can run (a tool-less
//! call, like triage itself, asked for a JSON plan) and otherwise from the
//! heuristic plan (one part per file the task names). A plan that does not
//! hold up falls back to the heuristic one; a task that does not split at
//! all goes to the worker role whole. When every part is back, the flight's
//! gates run on the combined result, as if a lead had called
//! `tower_complete`.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::store::PendingAction;
use tower_core::fanout::{self, Verdict};

impl Engine {
    /// `launch_flight` on a swarm role: plan the split instead of starting
    /// an agent. `Ok` names what happens next.
    pub(super) fn start_swarm(&mut self, flight_id: &str, role: &RoleCfg) -> Res {
        let swarm = role.swarm.clone().unwrap_or_default();
        // One split per flight: not while one is planned or running.
        if self.swarm_planning.contains(flight_id) {
            return Err(format!(
                "{} is still planning this flight's split",
                role.name
            ));
        }
        if let Some(x) = self
            .st
            .fanouts
            .iter()
            .find(|x| x.flight_id == flight_id && x.status.is_open())
        {
            return Err(format!(
                "This flight's split {} is still {}",
                x.id,
                x.status.label()
            ));
        }
        let open = self
            .st
            .fanouts
            .iter()
            .filter(|f| f.from_role == role.id && f.status.is_open())
            .count() as u32;
        if open >= role.max_concurrency {
            return Err(format!(
                "{} already splits {open} task(s), its max_concurrency",
                role.name
            ));
        }
        let local_only = self.local_only_reason(flight_id).is_some();
        let worker = match swarm.worker.as_deref() {
            Some(w) => self
                .cfg
                .resolve_role(w)
                .map(|r| r.id.clone())
                .ok_or_else(|| format!("{}: unknown worker role {w}", role.name))?,
            None => self
                .auto_fallback(local_only)
                .map(|(id, _)| id)
                .ok_or_else(|| format!("{}: no role can do the parts", role.name))?,
        };
        if let Some(f) = self.flight_mut(flight_id) {
            f.role_id = Some(role.id.clone());
        }
        self.set_stage(flight_id, Stage::InFlight);
        let planner = self.usable_triage_role().cloned();
        match planner {
            Some(t) if self.triage_refusal(flight_id, &t).is_none() => {
                self.note(
                    flight_id,
                    format!("{}: {} is planning the split", role.name, t.name),
                );
                let f = self.flight(flight_id).cloned().ok_or("Flight is gone")?;
                let candidates: Vec<(String, String)> = self
                    .auto_candidates(local_only)
                    .into_iter()
                    .filter(|r| r.swarm.is_none())
                    .map(|r| (r.id.clone(), r.summary.clone()))
                    .collect();
                let max_agents = self.swarm_agents(&swarm);
                let system = prompt::fanout_planner(&candidates, max_agents);
                let user = format!(
                    "Task {}: {}\n\n{}",
                    f.key,
                    f.title,
                    shorten(f.brief.trim(), 6000)
                );
                let fid = flight_id.to_string();
                let rid = role.id.clone();
                let planned_by = format!("triage:{}", t.id);
                self.swarm_planning.insert(flight_id.to_string());
                self.ask_triage(flight_id, &t, system, user, move |result| Input::Planned {
                    flight_id: fid,
                    role_id: rid,
                    worker,
                    planned_by,
                    result,
                });
                Ok("planning".into())
            }
            _ => {
                self.on_planned(
                    flight_id,
                    &role.id,
                    &worker,
                    "heuristic",
                    Err("no planner can run".into()),
                );
                Ok("planned".into())
            }
        }
    }

    fn swarm_agents(&self, swarm: &config::SwarmCfg) -> u32 {
        let limits = self.cfg.fanout.limits();
        swarm
            .max_agents
            .unwrap_or(limits.max_agents)
            .clamp(1, limits.max_agents)
    }

    /// The planner answered (or there is none): start the fan-out, or give
    /// the whole task to the worker when there is nothing to split.
    pub(super) fn on_planned(
        &mut self,
        flight_id: &str,
        role_id: &str,
        worker: &str,
        planned_by: &str,
        result: Result<(String, Usage), String>,
    ) {
        self.swarm_planning.remove(flight_id);
        let Some(f) = self.flight(flight_id).cloned() else {
            return;
        };
        let role = self.cfg.role(role_id).cloned();
        let swarm = role
            .as_ref()
            .and_then(|r| r.swarm.clone())
            .unwrap_or_default();
        let max_agents = self.swarm_agents(&swarm);
        let limits = self.cfg.fanout.limits();
        let name = role.map(|r| r.name).unwrap_or_else(|| role_id.to_string());
        // The planner's plan, if it holds up.
        let mut why_not = None;
        let planned = match result {
            Ok((text, usage)) => {
                self.add_flight_usage(flight_id, &usage);
                match crate::drivers::llm::first_json_object(&text)
                    .ok_or_else(|| "it gave no JSON plan".to_string())
                    .and_then(|v| fanout::parse_plan(&v))
                {
                    Ok((mut tasks, analysis)) => match fanout::validate(&tasks, &limits) {
                        Ok(()) => {
                            fanout::serialize_conflicts(&mut tasks);
                            let waves = fanout::build_waves(&tasks);
                            match fanout::evaluate(&tasks, &waves, &limits) {
                                Verdict::Fanout => Some((tasks, analysis)),
                                Verdict::Single => {
                                    why_not = Some("it found nothing to split".to_string());
                                    None
                                }
                                Verdict::TooBig(w) => {
                                    why_not = Some(w);
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            why_not = Some(e.join("; "));
                            None
                        }
                    },
                    Err(e) => {
                        why_not = Some(e);
                        None
                    }
                }
            }
            Err(e) => {
                why_not = Some(e);
                None
            }
        };
        let (tasks, analysis, by) = match planned {
            Some((t, a)) => (t, a, planned_by.to_string()),
            None => {
                if planned_by != "heuristic" {
                    self.note(
                        flight_id,
                        format!(
                            "{name}: the planner's split did not hold ({}); splitting by the files the task names",
                            shorten(why_not.as_deref().unwrap_or("no answer"), 200)
                        ),
                    );
                }
                (
                    fanout::heuristic_plan(&f.title, &f.brief, max_agents),
                    String::new(),
                    "heuristic".to_string(),
                )
            }
        };
        let waves = fanout::build_waves(&tasks);
        if fanout::evaluate(&tasks, &waves, &limits) != Verdict::Fanout {
            let worker_name = self.role_name(worker);
            self.note(
                flight_id,
                format!("{name}: nothing to split, so {worker_name} takes the whole task"),
            );
            if let Some(x) = self.flight_mut(flight_id) {
                x.role_id = Some(worker.to_string());
            }
            if let Err(e) = self.launch_flight(flight_id, worker) {
                self.note(flight_id, format!("Launch failed: {e}"));
                self.set_stage(flight_id, Stage::NeedsYou);
            }
            return;
        }
        let started = self.start_fanout(
            flight_id, role_id, "", tasks, analysis, worker, max_agents, &by,
        );
        let Err(e) = started else {
            return;
        };
        // A planner plan that names a role it cannot use: split by the
        // files instead, once.
        if by != "heuristic" {
            self.note(
                flight_id,
                format!(
                    "{name}: the planner's split could not start ({}); splitting by the files the task names",
                    shorten(&e, 200)
                ),
            );
            let tasks = fanout::heuristic_plan(&f.title, &f.brief, max_agents);
            let waves = fanout::build_waves(&tasks);
            if fanout::evaluate(&tasks, &waves, &limits) == Verdict::Fanout {
                if let Err(e) = self.start_fanout(
                    flight_id,
                    role_id,
                    "",
                    tasks,
                    String::new(),
                    worker,
                    max_agents,
                    "heuristic",
                ) {
                    self.note(flight_id, format!("{name}: could not start the split: {e}"));
                    self.set_stage(flight_id, Stage::NeedsYou);
                }
                return;
            }
            let worker_name = self.role_name(worker);
            self.note(
                flight_id,
                format!("{name}: nothing to split, so {worker_name} takes the whole task"),
            );
            if let Some(x) = self.flight_mut(flight_id) {
                x.role_id = Some(worker.to_string());
            }
            if let Err(e) = self.launch_flight(flight_id, worker) {
                self.note(flight_id, format!("Launch failed: {e}"));
                self.set_stage(flight_id, Stage::NeedsYou);
            }
            return;
        }
        self.note(flight_id, format!("{name}: could not start the split: {e}"));
        self.set_stage(flight_id, Stage::NeedsYou);
    }

    /// Tower's own fan-out is over. All back: the flight's gates run on the
    /// combined result (landing then follows `land` and `[gates]`). Parts
    /// missing: you decide.
    pub(super) fn swarm_finished(&mut self, id: &str, clean: bool) {
        let Some(f) = self.fanout(id).cloned() else {
            return;
        };
        if clean {
            if let Some(x) = self.flight_mut(&f.flight_id) {
                x.outcome = Some("resolved".into());
            }
            if let Err(e) = self.run_gates(&f.flight_id) {
                self.note(&f.flight_id, format!("Checks could not run: {e}"));
                self.set_stage(&f.flight_id, Stage::NeedsYou);
            }
            return;
        }
        self.set_stage(&f.flight_id, Stage::NeedsYou);
        let missing = f.subtasks.len() - f.count(TaskState::Returned);
        let item = self.add_inbox(InboxItem {
            kind: InboxKind::Clearance,
            title: format!(
                "Fan-out {id} finished with {missing} part{} missing",
                if missing == 1 { "" } else { "s" }
            ),
            detail: "Run the checks on what came back, or leave the flight for you to finish."
                .into(),
            flight_id: Some(f.flight_id.clone()),
            options: vec![
                InboxOption {
                    id: "gates".into(),
                    label: "Run the checks".into(),
                    primary: true,
                },
                InboxOption {
                    id: "dismiss".into(),
                    label: "Leave it".into(),
                    primary: false,
                },
            ],
            ..Default::default()
        });
        self.st
            .pending_actions
            .insert(item, PendingAction::FanoutFailed { id: id.to_string() });
        self.changed();
    }

    /// Answers only Tower's own fan-out gets: run the checks anyway, or
    /// leave the flight.
    pub(super) fn resolve_swarm_failure(&mut self, id: &str, opt: &str) -> Res {
        let f = self.fanout(id).cloned().ok_or("That fan-out is gone")?;
        if f.status != FanoutStatus::Partial
            || !f.return_to.starts_with(super::fanouts::TOWER_RETURN)
        {
            return Err(format!(
                "Fan-out {id} is {}: nothing to decide",
                f.status.label()
            ));
        }
        match opt {
            "gates" => {
                // You chose to check what came back as the result.
                if let Some(x) = self.flight_mut(&f.flight_id) {
                    x.outcome = Some("resolved".into());
                }
                self.run_gates(&f.flight_id)
            }
            "dismiss" | "not_now" => Ok("Left for you".into()),
            other => Err(format!("Unknown answer {other:?}")),
        }
    }
}
