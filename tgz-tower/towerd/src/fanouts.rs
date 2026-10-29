//! Fan-out: one task split into parts that run as parallel child flights,
//! wave by wave, with one combined result at the end.
//!
//! Two ways in, one engine: an agent calls `tower_fanout` with its plan, or
//! Tower splits a task itself for a swarm role (`swarm.rs`). Either way the
//! plan is checked and layered into waves by `tower_core::fanout`, the
//! policy is asked once (`handoff.fanout`), and every part becomes a child
//! flight through the same `make_child` a handoff uses. A wave starts once
//! the one before it is over; a part that fails ends its wave with a
//! question, or with what `[fanout] on_failure` says.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::policy::{Action, Decision as PolicyDecision};
use crate::store::PendingAction;
use tower_core::fanout::{self, PartResult, Step, Verdict};

/// `return_to` of a fan-out Tower planned itself: no agent waits for it.
pub(super) const TOWER_RETURN: &str = "tower:";

/// The combined result is cut to this many characters.
const AGGREGATE_MAX: usize = 6000;

/// Finished fan-outs kept for the Board and History.
const KEEP_FINISHED: usize = 50;

/// What a wave with failed parts does next.
#[derive(Clone, Copy, Debug, PartialEq)]
enum OnFailure {
    Ask,
    Continue,
    Stop,
}

impl Engine {
    pub(super) fn fanout(&self, id: &str) -> Option<&Fanout> {
        self.st.fanouts.iter().find(|x| x.id == id)
    }

    fn fanout_mut(&mut self, id: &str) -> Option<&mut Fanout> {
        self.st.fanouts.iter_mut().find(|x| x.id == id)
    }

    /// The fan-out a child flight is a part of, and the part's id.
    fn part_of(&self, child_id: &str) -> Option<(String, String)> {
        let gid = self.flight(child_id)?.group_id.clone()?;
        let f = self.fanout(&gid)?;
        let t = f.task_of_flight(child_id)?;
        Some((gid, t.id.clone()))
    }

    // ------------------------------------------------------------------
    // Starting
    // ------------------------------------------------------------------

    /// `tower_fanout`: the calling agent's plan for its own flight.
    pub(super) fn tool_fanout(&mut self, a: &Agent, fid: &str, args: &Value) -> Res {
        let flight = self.flight(fid).cloned().ok_or("Flight is gone")?;
        if flight.group_id.is_some() {
            return Err(
                "You are one part of a fan-out already: do your part and call tower_return".into(),
            );
        }
        if let Some(open) = self
            .st
            .fanouts
            .iter()
            .find(|x| x.flight_id == fid && x.status.is_open())
        {
            return Err(format!(
                "This flight already has fan-out {} running; wait for its result (tower_await with handoff_id \"{}\")",
                open.id, open.id
            ));
        }
        let (tasks, analysis) = fanout::parse_plan(args)?;
        let local_only = self.local_only_reason(fid).is_some();
        let worker = match args
            .get("to_role")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(r) => self
                .cfg
                .resolve_role(r)
                .map(|x| x.id.clone())
                .ok_or_else(|| format!("Unknown role {r}"))?,
            None => self
                .auto_fallback(local_only)
                .map(|(id, _)| id)
                .ok_or("No role can take the parts: name one with to_role")?,
        };
        let limits = self.cfg.fanout.limits();
        let max_agents = args
            .get("max_agents")
            .and_then(Value::as_u64)
            .map(|n| (n as u32).clamp(1, limits.max_agents))
            .unwrap_or(limits.max_agents);
        self.start_fanout(
            fid, &a.role_id, &a.id, tasks, analysis, &worker, max_agents, "agent",
        )
    }

    /// Check a plan, store it as fan-out `x-N`, and ask the policy once.
    /// `Ok` is the text for whoever asked.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_fanout(
        &mut self,
        parent: &str,
        from_role: &str,
        return_to: &str,
        mut tasks: Vec<FanoutTask>,
        analysis: String,
        worker: &str,
        max_agents: u32,
        planned_by: &str,
    ) -> Res {
        let limits = self.cfg.fanout.limits();
        fanout::validate(&tasks, &limits).map_err(|e| e.join("; "))?;
        fanout::serialize_conflicts(&mut tasks);
        let waves = fanout::build_waves(&tasks);
        match fanout::evaluate(&tasks, &waves, &limits) {
            Verdict::Single => {
                return Err(
                    "That plan has one part: hand it off with tower_handoff, or do it yourself"
                        .into(),
                )
            }
            Verdict::TooBig(why) => return Err(why),
            Verdict::Fanout => {}
        }
        // Every part's role must exist, do work, and keep local-only work
        // inside your network: refuse before anything is created.
        let stays_local = self.local_only_reason(parent).is_some();
        let mut all_local = true;
        for t in tasks.iter_mut() {
            let rid = t.role.clone().unwrap_or_else(|| worker.to_string());
            let role = self
                .cfg
                .resolve_role(&rid)
                .cloned()
                .ok_or_else(|| format!("Part {}: unknown role {rid}", t.id))?;
            if role.driver == DriverKind::Llm {
                return Err(format!(
                    "Part {}: {} has no tools and cannot do a part",
                    t.id, role.name
                ));
            }
            if role.swarm.is_some() {
                return Err(format!(
                    "Part {}: {} splits work itself; give the parts to a role that does them",
                    t.id, role.name
                ));
            }
            if let Some(why) = self.role_warning(&role) {
                return Err(format!("Part {}: {} cannot run: {why}", t.id, role.name));
            }
            // The lead is an agent of this role and the role runs one at a
            // time: its parts could never start while it waits for them.
            let lead_is_it = self
                .agent(return_to)
                .is_some_and(|a| a.status.is_live() && a.role_id == role.id);
            if lead_is_it && role.max_concurrency <= 1 {
                return Err(format!(
                    "Part {}: {} runs one agent at a time and you are it; give the parts another role (to_role)",
                    t.id, role.name
                ));
            }
            self.target_stays_local_ok(&role, stays_local)
                .map_err(|e| format!("Part {}: {e}", t.id))?;
            all_local &= role.all_local();
            t.role = Some(role.id.clone());
        }
        let worker_id = self
            .cfg
            .resolve_role(worker)
            .map(|r| r.id.clone())
            .unwrap_or_else(|| worker.to_string());
        self.st.counters.fanout += 1;
        let id = format!("x-{}", self.st.counters.fanout);
        let now = now_unix();
        let hands_free = self.flight(parent).is_some_and(|f| f.hands_free);
        let n = tasks.len();
        let w = waves.len();
        self.st.fanouts.push(Fanout {
            id: id.clone(),
            flight_id: parent.to_string(),
            return_to: if return_to.is_empty() {
                format!("{TOWER_RETURN}{id}")
            } else {
                return_to.to_string()
            },
            from_role: from_role.to_string(),
            worker_role: worker_id.clone(),
            subtasks: tasks,
            waves,
            wave: 0,
            max_agents: max_agents.max(1),
            status: FanoutStatus::Waiting,
            planned_by: planned_by.to_string(),
            analysis: analysis.clone(),
            hands_free,
            created_unix: now,
            updated_unix: now,
            finished_unix: None,
        });
        let worker_name = self.role_name(&worker_id);
        let by = match planned_by {
            "agent" => format!("planned by {}", self.role_name(from_role)),
            "heuristic" => "planned by heuristics (one part per file)".to_string(),
            p => format!(
                "planned by {}",
                self.role_name(p.strip_prefix("triage:").unwrap_or(p))
            ),
        };
        self.note(
            parent,
            format!(
                "Fan-out {id}: {n} parts in {w} wave{}, {by}, on {worker_name}, at most {} at once",
                if w == 1 { "" } else { "s" },
                max_agents.max(1)
            ),
        );
        self.changed();
        // One decision for the whole plan; parts that only run local models
        // follow `handoff.local`, like a local handoff.
        let action = if all_local {
            Action::HandoffLocal
        } else {
            Action::HandoffFanout
        };
        let overrides = self.overrides_for(parent);
        match self.decide_action(action, overrides.as_ref(), false, Some(parent), Some(&id)) {
            PolicyDecision::Run => {
                self.set_fanout_status(&id, FanoutStatus::Running);
                self.advance_fanout(&id);
                Ok(self.fanout_started_text(&id))
            }
            PolicyDecision::Ask => {
                let detail = self.fanout_plan_text(&id);
                self.ask_action(
                    format!("Fan out {n} parts in {w} wave(s) to {worker_name}?"),
                    detail,
                    parent,
                    PendingAction::Fanout { id: id.clone() },
                );
                Ok(format!(
                    "Fan-out {id} ({n} parts in {w} wave(s)) is waiting for the human's clearance. Keep working or end your turn; the combined result arrives as a [Tower] message, or call tower_await with handoff_id \"{id}\"."
                ))
            }
            PolicyDecision::Draft | PolicyDecision::Skip => {
                self.st.fanouts.retain(|x| x.id != id);
                self.note(
                    parent,
                    format!("Fan-out {id} not started: handoff.fanout is off"),
                );
                Err(format!(
                    "{} is not allowed in [policy]: split the work with tower_handoff, or do it yourself",
                    action.key()
                ))
            }
        }
    }

    fn fanout_started_text(&self, id: &str) -> String {
        let Some(f) = self.fanout(id) else {
            return String::new();
        };
        let running = f.count(TaskState::Running);
        let queued = f.count(TaskState::Queued);
        format!(
            "Fan-out {id} started: {} parts in {} wave(s), {running} running, {queued} queued. Keep working or end your turn; one combined result arrives as a [Tower] message, or call tower_await with handoff_id \"{id}\".",
            f.subtasks.len(),
            f.waves.len()
        )
    }

    /// The plan, wave by wave, for an Inbox item.
    fn fanout_plan_text(&self, id: &str) -> String {
        let Some(f) = self.fanout(id) else {
            return String::new();
        };
        f.waves
            .iter()
            .enumerate()
            .map(|(i, ids)| {
                let parts: Vec<String> = ids
                    .iter()
                    .filter_map(|t| f.task(t))
                    .map(|t| shorten(&t.title, 60))
                    .collect();
                format!("Wave {}: {}", i + 1, parts.join(" · "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn set_fanout_status(&mut self, id: &str, status: FanoutStatus) {
        let now = now_unix();
        if let Some(f) = self.fanout_mut(id) {
            f.status = status;
            f.updated_unix = now;
            if !status.is_open() {
                f.finished_unix = Some(now);
            }
        }
        self.changed();
    }

    // ------------------------------------------------------------------
    // Waves
    // ------------------------------------------------------------------

    /// Walk the barrier until there is nothing to do right now.
    pub(super) fn advance_fanout(&mut self, id: &str) {
        // A part that fails while its wave is being started calls back in
        // here; the outer loop sees its state anyway.
        if !self.fanouts_advancing.insert(id.to_string()) {
            return;
        }
        // Bounded: every turn of the loop starts, skips or finishes parts.
        for _ in 0..64 {
            let Some(f) = self.fanout(id).cloned() else {
                break;
            };
            if f.status != FanoutStatus::Running {
                break;
            }
            match fanout::next_step(&f) {
                Step::StartWave(n) => {
                    if n as u32 != f.wave {
                        if let Some(x) = self.fanout_mut(id) {
                            x.wave = n as u32;
                        }
                        self.note(
                            &f.flight_id,
                            format!("Fan-out {id}: wave {} of {}", n + 1, f.waves.len()),
                        );
                    }
                    self.begin_wave(id, n);
                }
                Step::Wait => break,
                Step::AskFailure(failed) => match self.on_failure(&f) {
                    OnFailure::Continue => self.continue_after_failure(id),
                    OnFailure::Stop => self.stop_fanout(id, "a part failed and on_failure is stop"),
                    OnFailure::Ask => {
                        self.ask_fanout_failure(id, &failed);
                        break;
                    }
                },
                Step::Finish => {
                    self.finish_fanout(id);
                    break;
                }
            }
        }
        self.fanouts_advancing.remove(id);
    }

    fn on_failure(&self, f: &Fanout) -> OnFailure {
        match self.cfg.fanout.on_failure.as_str() {
            "ask" => OnFailure::Ask,
            "continue" => OnFailure::Continue,
            "stop" => OnFailure::Stop,
            // An agent can judge a partial result and try again itself;
            // Tower's own split asks you, unless the flight runs hands-free.
            _ if !f.return_to.starts_with(TOWER_RETURN) || f.hands_free => OnFailure::Continue,
            _ => OnFailure::Ask,
        }
    }

    /// Skip what waits on the failed parts and move to the next wave.
    fn continue_after_failure(&mut self, id: &str) {
        let Some(f) = self.fanout_mut(id) else { return };
        let skipped = fanout::skip_dependents(f);
        let parent = f.flight_id.clone();
        let next = f.wave + 1;
        let last = next as usize >= f.waves.len();
        if !last {
            f.wave = next;
        } else {
            // Nothing after this wave: mark it finished by moving past it.
            f.wave = f.waves.len() as u32;
        }
        if !skipped.is_empty() {
            self.note(
                &parent,
                format!(
                    "Fan-out {id}: skipped {} (they wait on a part that failed)",
                    skipped.join(", ")
                ),
            );
        }
        self.changed();
    }

    /// Start every pending part of wave `n`.
    fn begin_wave(&mut self, id: &str, n: usize) {
        let Some(f) = self.fanout(id).cloned() else {
            return;
        };
        let Some(wave) = f.waves.get(n).cloned() else {
            return;
        };
        for tid in wave {
            let Some(t) = self.fanout(id).and_then(|x| x.task(&tid)).cloned() else {
                continue;
            };
            if t.state != TaskState::Pending {
                continue;
            }
            let role = t.role.clone().unwrap_or_else(|| f.worker_role.clone());
            let packet = part_packet(&t, &role);
            let child =
                match self.make_child(&f.flight_id, &f.from_role, &f.return_to, packet, "fanout") {
                    Ok((child, packet, _)) => (child, packet),
                    Err(e) => {
                        self.set_part(
                            id,
                            &tid,
                            TaskState::Failed,
                            Some(format!("could not start: {e}")),
                            None,
                        );
                        continue;
                    }
                };
            let (child_id, packet) = child;
            let now = now_unix();
            if let Some(c) = self.flight_mut(&child_id) {
                c.group_id = Some(id.to_string());
                c.hands_free = f.hands_free;
            }
            if let Some(x) = self.fanout_mut(id) {
                if let Some(task) = x.task_mut(&tid) {
                    task.flight_id = Some(child_id.clone());
                    task.packet_id = Some(packet.id.clone());
                    task.started_unix = Some(now);
                    task.state = TaskState::Queued;
                    task.reason = None;
                }
            }
            let tier = t.tier.as_deref().and_then(core::router::Tier::parse);
            self.route_flight(&child_id, tier.map(|t| (t, "fanout")));
            // The plan was cleared as a whole: no Inbox item per part.
            // `release` keeps the part's state in step (queued, running, or
            // failed to start).
            let _ = self.release(&child_id, &packet.id);
        }
        self.changed();
    }

    /// Set a part's state by its fan-out and part id.
    fn set_part(
        &mut self,
        id: &str,
        tid: &str,
        state: TaskState,
        reason: Option<String>,
        _flight: Option<&str>,
    ) {
        let now = now_unix();
        if let Some(f) = self.fanout_mut(id) {
            f.updated_unix = now;
            if let Some(t) = f.task_mut(tid) {
                t.state = state;
                if reason.is_some() {
                    t.reason = reason;
                }
            }
        }
        self.changed();
    }

    /// `release` reports where a part's child flight went.
    pub(super) fn fanout_part_state(
        &mut self,
        child_id: &str,
        state: TaskState,
        reason: Option<String>,
    ) {
        if let Some((gid, tid)) = self.part_of(child_id) {
            let done = self
                .fanout(&gid)
                .and_then(|f| f.task(&tid))
                .is_some_and(|t| t.state.is_done());
            if !done {
                self.set_part(&gid, &tid, state, reason, Some(child_id));
            }
        }
    }

    /// Is this child a fan-out part that must wait for a slot? `Some(why)`
    /// when its fan-out already runs `max_agents` parts, or its role is at
    /// `max_concurrency`.
    pub(super) fn fanout_wait(&self, child_id: &str, packet: &HandoffPacket) -> Option<String> {
        let (gid, _) = self.part_of(child_id)?;
        let f = self.fanout(&gid)?;
        let running = f
            .subtasks
            .iter()
            .filter(|t| t.state == TaskState::Running && t.flight_id.as_deref() != Some(child_id))
            .count() as u32;
        if running >= f.max_agents {
            return Some(format!(
                "waiting for a free slot (fan-out {} runs {} parts at once)",
                f.id, f.max_agents
            ));
        }
        let cap = self
            .cfg
            .role(&packet.to_role)
            .map(|r| r.max_concurrency)
            .unwrap_or(1);
        if self.role_load(&packet.to_role) >= cap {
            return Some(format!(
                "waiting for a free {} agent ({cap} at once)",
                self.role_name(&packet.to_role)
            ));
        }
        None
    }

    /// A part is over: returned, failed, stopped. Moves the barrier.
    pub(super) fn fanout_child_done(&mut self, child_id: &str, ok: bool, reason: Option<String>) {
        let Some((gid, tid)) = self.part_of(child_id) else {
            return;
        };
        let already = self
            .fanout(&gid)
            .and_then(|f| f.task(&tid))
            .is_some_and(|t| t.state.is_done());
        if already {
            return;
        }
        let state = if ok {
            TaskState::Returned
        } else {
            TaskState::Failed
        };
        if !ok {
            if let Some(parent) = self.fanout(&gid).map(|f| f.flight_id.clone()) {
                let key = self
                    .flight(child_id)
                    .map(|f| f.key.clone())
                    .unwrap_or_default();
                self.note(
                    &parent,
                    format!(
                        "Fan-out {gid}: part {tid} ({key}) failed: {}",
                        reason.as_deref().unwrap_or("no result")
                    ),
                );
            }
        }
        self.set_part(&gid, &tid, state, reason, Some(child_id));
        self.advance_fanout(&gid);
    }

    /// `tower_return` from a fan-out part: the result goes to the barrier,
    /// not to the lead agent one part at a time.
    pub(super) fn fanout_return(
        &mut self,
        a: &Agent,
        child_id: &str,
        packet: &HandoffPacket,
        summary: &str,
    ) -> Res {
        self.set_packet_state(&packet.id, HandoffState::Returned);
        self.timeline(
            child_id,
            TimelineBody::Message {
                from: a.id.clone(),
                text: summary.to_string(),
            },
        );
        self.set_stage(child_id, Stage::Landed);
        self.learn(child_id, true);
        self.note(child_id, "Returned");
        let (gid, tid) = self.part_of(child_id).unwrap_or_default();
        if let Some(parent) = self.fanout(&gid).map(|f| f.flight_id.clone()) {
            let key = self
                .flight(child_id)
                .map(|f| f.key.clone())
                .unwrap_or_default();
            self.note(
                &parent,
                format!("Fan-out {gid}: part {tid} ({key}) returned"),
            );
        }
        // ACP helpers are done after returning; panes stay for you to read.
        if a.driver != DriverKind::Pane {
            self.stop_after_turn.insert(a.id.clone());
        }
        self.fanout_child_done(child_id, true, None);
        Ok(format!(
            "Returned to Tower's fan-out {gid}: your part is done, end your turn."
        ))
    }

    // ------------------------------------------------------------------
    // Failures
    // ------------------------------------------------------------------

    fn ask_fanout_failure(&mut self, id: &str, failed: &[String]) {
        let Some(f) = self.fanout(id).cloned() else {
            return;
        };
        self.set_fanout_status(id, FanoutStatus::Paused);
        // One question per fan-out at a time.
        let asked = self
            .st
            .pending_actions
            .iter()
            .any(|(_, p)| matches!(p, PendingAction::FanoutFailed { id: x } if x == id));
        if asked {
            return;
        }
        let names: Vec<String> = failed
            .iter()
            .filter_map(|t| f.task(t))
            .map(|t| {
                format!(
                    "{}: {}",
                    shorten(&t.title, 50),
                    t.reason.as_deref().unwrap_or("no result")
                )
            })
            .collect();
        let retry_left = failed
            .iter()
            .filter_map(|t| f.task(t))
            .any(|t| t.retries < self.cfg.fanout.max_retries);
        let mut options = Vec::new();
        if retry_left {
            options.push(InboxOption {
                id: "retry".into(),
                label: "Retry them".into(),
                primary: true,
            });
        }
        options.push(InboxOption {
            id: "continue".into(),
            label: "Go on without them".into(),
            primary: !retry_left,
        });
        options.push(InboxOption {
            id: "stop".into(),
            label: "Stop the fan-out".into(),
            primary: false,
        });
        let item = self.add_inbox(InboxItem {
            kind: InboxKind::Clearance,
            title: format!(
                "Fan-out {id}: {} part{} failed",
                failed.len(),
                if failed.len() == 1 { "" } else { "s" }
            ),
            detail: names.join("\n"),
            flight_id: Some(f.flight_id.clone()),
            options,
            ..Default::default()
        });
        self.st
            .pending_actions
            .insert(item, PendingAction::FanoutFailed { id: id.to_string() });
        self.changed();
    }

    /// What an answer to a fan-out's question means, before anything is
    /// changed: `Ok(option)` to act on (`approve` resolved to the primary
    /// choice, `deny` and the like to stop or leave it), `Err` when it does
    /// not fit the fan-out as it is now.
    pub(super) fn fanout_answer(&self, id: &str, opt: &str) -> Result<String, String> {
        let f = self.fanout(id).ok_or("That fan-out is gone")?;
        let retry_left = self.retryable_parts(f).next().is_some();
        let opt = match (f.status, opt) {
            (FanoutStatus::Paused, "approve" | "run") => {
                if retry_left {
                    "retry"
                } else {
                    "continue"
                }
            }
            (FanoutStatus::Paused, "deny" | "dismiss" | "not_now") => "stop",
            (FanoutStatus::Paused, o @ ("continue" | "stop")) => o,
            (FanoutStatus::Paused, "retry") if retry_left => "retry",
            (FanoutStatus::Paused, "retry") => {
                return Err("No failed part has a retry left: go on without it, or stop".into())
            }
            (FanoutStatus::Partial, "approve" | "run" | "gates")
                if f.return_to.starts_with(TOWER_RETURN) =>
            {
                "gates"
            }
            (FanoutStatus::Partial, "deny" | "dismiss" | "not_now") => "dismiss",
            (FanoutStatus::Paused | FanoutStatus::Partial, other) => {
                return Err(format!("{other:?} is not an answer to this question"))
            }
            (s, _) => return Err(format!("Fan-out {id} is {}: nothing to decide", s.label())),
        };
        Ok(opt.to_string())
    }

    /// Failed parts of the wave the barrier stopped at that may run again.
    fn retryable_parts<'a>(&self, f: &'a Fanout) -> impl Iterator<Item = &'a FanoutTask> + 'a {
        let max = self.cfg.fanout.max_retries;
        let wave: Vec<String> = f.waves.get(f.wave as usize).cloned().unwrap_or_default();
        f.subtasks.iter().filter(move |t| {
            t.state == TaskState::Failed && t.retries < max && wave.contains(&t.id)
        })
    }

    /// Your answer to "parts failed": `retry`, `continue` or `stop` (see
    /// `fanout_answer` for what is accepted when).
    pub(super) fn resolve_fanout_failure(&mut self, id: &str, opt: &str) -> Res {
        let opt = self.fanout_answer(id, opt)?;
        let f = self.fanout(id).cloned().ok_or("That fan-out is gone")?;
        match opt.as_str() {
            "retry" => {
                // Only the parts this question was about: earlier failures
                // you already went on without stay as they are.
                let again: Vec<String> = self.retryable_parts(&f).map(|t| t.id.clone()).collect();
                if let Some(x) = self.fanout_mut(id) {
                    for t in x.subtasks.iter_mut() {
                        if again.contains(&t.id) {
                            t.retries += 1;
                            t.state = TaskState::Pending;
                            t.reason = None;
                            t.flight_id = None;
                            t.packet_id = None;
                        }
                    }
                    // What was skipped because they failed may run again.
                    let mut back: Vec<String> = again.clone();
                    loop {
                        let mut more = false;
                        for t in x.subtasks.iter_mut() {
                            if t.state == TaskState::Skipped
                                && t.depends_on.iter().any(|d| back.contains(d))
                            {
                                t.state = TaskState::Pending;
                                t.reason = None;
                                back.push(t.id.clone());
                                more = true;
                            }
                        }
                        if !more {
                            break;
                        }
                    }
                }
                if again.is_empty() {
                    return Err("No failed part has a retry left".into());
                }
                self.note(
                    &f.flight_id,
                    format!("Fan-out {id}: retrying {}", again.join(", ")),
                );
                self.set_fanout_status(id, FanoutStatus::Running);
                self.advance_fanout(id);
                Ok(format!("Retrying {}", again.join(", ")))
            }
            "continue" => {
                self.set_fanout_status(id, FanoutStatus::Running);
                self.continue_after_failure(id);
                self.advance_fanout(id);
                Ok("Going on without the failed parts".into())
            }
            "stop" => {
                self.set_fanout_status(id, FanoutStatus::Running);
                self.stop_fanout(id, "you stopped it after a failure");
                // Finishes with what came back.
                self.advance_fanout(id);
                Ok("Stopped".into())
            }
            other => self.resolve_swarm_failure(id, other),
        }
    }

    /// Skip everything not yet started, then finish with what returned.
    fn stop_fanout(&mut self, id: &str, why: &str) {
        let Some(x) = self.fanout_mut(id) else { return };
        for t in x.subtasks.iter_mut() {
            if t.state == TaskState::Pending {
                t.state = TaskState::Skipped;
                t.reason = Some(why.to_string());
            }
        }
        x.wave = x.waves.len() as u32;
        let parent = x.flight_id.clone();
        self.note(&parent, format!("Fan-out {id} stopped: {why}"));
        self.changed();
    }

    /// Give up on a fan-out: nothing more starts, queued parts are dropped,
    /// running parts are stopped, and the asker hears why.
    pub(super) fn cancel_fanout(&mut self, id: &str, why: &str) {
        let Some(f) = self.fanout(id).cloned() else {
            return;
        };
        if !f.status.is_open() {
            return;
        }
        // Closed first: stopping a running part reports it failed, and that
        // must not move the barrier (start the next wave, or finish and
        // deliver a result) in the middle of a cancel.
        self.set_fanout_status(id, FanoutStatus::Cancelled);
        for t in &f.subtasks {
            let Some(child) = t.flight_id.clone() else {
                continue;
            };
            match t.state {
                TaskState::Queued => {
                    self.st.local_queue.retain(|(c, _)| c != &child);
                    self.note(&child, format!("Not started: fan-out {id} was cancelled"));
                    self.set_stage(&child, Stage::NeedsYou);
                }
                TaskState::Running => {
                    if let Some(agent) = self.flight(&child).and_then(|c| c.agent_id.clone()) {
                        let _ = self.stop_agent(&agent);
                    }
                }
                _ => {}
            }
        }
        if let Some(x) = self.fanout_mut(id) {
            for t in x.subtasks.iter_mut() {
                if !t.state.is_done() {
                    t.state = TaskState::Skipped;
                    t.reason = Some(format!("cancelled: {why}"));
                }
            }
        }
        self.changed();
        self.st.inbox.retain(|i| {
            !(i.flight_id.as_deref() == Some(f.flight_id.as_str())
                && i.title.contains(&format!("Fan-out {id}")))
        });
        self.st.pending_actions.retain(|_, p| {
            !matches!(p, PendingAction::Fanout { id: x } | PendingAction::FanoutFailed { id: x } if x == id)
        });
        self.note(&f.flight_id, format!("Fan-out {id} cancelled: {why}"));
        if !f.return_to.starts_with(TOWER_RETURN) {
            let _ = self.deliver(
                &f.return_to,
                format!("[Tower] Your fan-out {id} was cancelled: {why}. Do the work another way, or ask the human."),
            );
        }
        self.prune_fanouts();
    }

    // ------------------------------------------------------------------
    // The end
    // ------------------------------------------------------------------

    fn finish_fanout(&mut self, id: &str) {
        let Some(f) = self.fanout(id).cloned() else {
            return;
        };
        let clean = f.subtasks.iter().all(|t| t.state == TaskState::Returned);
        self.set_fanout_status(
            id,
            if clean {
                FanoutStatus::Done
            } else {
                FanoutStatus::Partial
            },
        );
        let f = self.fanout(id).cloned().unwrap_or(f);
        let text = self.fanout_aggregate(&f, false);
        self.timeline(
            &f.flight_id,
            TimelineBody::Message {
                from: "tower".into(),
                text: text.clone(),
            },
        );
        if f.return_to.starts_with(TOWER_RETURN) {
            self.swarm_finished(id, clean);
        } else {
            let live = self.agent(&f.return_to).is_some_and(|a| a.status.is_live());
            if live {
                self.deliver_tower(
                    &f.return_to,
                    format!("[Tower] Result of fan-out {id}:\n{text}"),
                    "a fan-out result",
                );
            } else {
                // The Team channel reaches cloud agents: local-only parts
                // only say they are done.
                let public = self.fanout_aggregate(&f, true);
                self.post(
                    TEAM,
                    "tower",
                    &format!(
                        "Fan-out {id} finished ({} is not live): {}",
                        f.return_to,
                        shorten(&public, 600)
                    ),
                );
            }
        }
        self.prune_fanouts();
    }

    /// The one combined result: every part's summary, redacted for whoever
    /// reads it. `public`: for anyone (a local-only part only says it is
    /// done), whoever asked.
    fn fanout_aggregate(&self, f: &Fanout, public: bool) -> String {
        let mut owned: Vec<(String, String, String, String)> = Vec::new();
        for t in &f.subtasks {
            let Some(child) = t.flight_id.as_deref().and_then(|c| self.flight(c)) else {
                continue;
            };
            let by = {
                let role = child
                    .role_id
                    .as_deref()
                    .map(|r| self.role_name(r))
                    .unwrap_or_default();
                match child.model.as_ref() {
                    Some(m) if !m.label.is_empty() => format!("{role} ({})", m.label),
                    _ => role,
                }
            };
            let text = if t.state == TaskState::Returned {
                let full = self.result_text(child);
                if public || f.return_to.starts_with(TOWER_RETURN) {
                    if self.local_only_reason(&child.id).is_some() {
                        format!("done (local-only: read it in {})", child.key)
                    } else {
                        full
                    }
                } else {
                    self.redact_for(&child.id, &f.return_to, full)
                }
            } else {
                String::new()
            };
            owned.push((t.id.clone(), child.key.clone(), by, text));
        }
        let parts: Vec<PartResult<'_>> = owned
            .iter()
            .map(|(task_id, key, by, text)| PartResult {
                task_id,
                key,
                by,
                text,
            })
            .collect();
        fanout::aggregate(f, &parts, AGGREGATE_MAX)
    }

    /// Keep every open fan-out and the newest finished ones.
    fn prune_fanouts(&mut self) {
        let finished: Vec<String> = self
            .st
            .fanouts
            .iter()
            .filter(|f| !f.status.is_open())
            .map(|f| f.id.clone())
            .collect();
        if finished.len() > KEEP_FINISHED {
            let drop: HashSet<String> = finished[..finished.len() - KEEP_FINISHED]
                .iter()
                .cloned()
                .collect();
            self.st.fanouts.retain(|f| !drop.contains(&f.id));
        }
        // A finished fan-out whose flight is gone is only clutter.
        let flights: HashSet<String> = self.st.flights.iter().map(|f| f.id.clone()).collect();
        self.st
            .fanouts
            .retain(|f| f.status.is_open() || flights.contains(&f.flight_id));
        self.changed();
    }

    // ------------------------------------------------------------------
    // Time and restarts
    // ------------------------------------------------------------------

    /// From `tick`: stop parts past `child_timeout_mins`, and move every
    /// running fan-out (a queued part may have started or failed).
    pub(super) fn fanout_tick(&mut self) {
        if self.st.fanouts.is_empty() {
            return;
        }
        let limit = self.cfg.fanout.child_timeout_mins * 60;
        let now = now_unix();
        let mut late: Vec<(String, Option<String>)> = Vec::new();
        let mut open: Vec<String> = Vec::new();
        for f in &self.st.fanouts {
            if f.status != FanoutStatus::Running {
                continue;
            }
            open.push(f.id.clone());
            if limit == 0 {
                continue;
            }
            for t in &f.subtasks {
                // A queued part's clock runs from when it was queued: one
                // that never gets a slot must not hold the barrier forever.
                let over = matches!(t.state, TaskState::Running | TaskState::Queued)
                    && t.started_unix
                        .is_some_and(|s| now.saturating_sub(s) > limit);
                if over {
                    if let Some(c) = t.flight_id.clone() {
                        let agent = (t.state == TaskState::Running)
                            .then(|| self.flight(&c).and_then(|x| x.agent_id.clone()))
                            .flatten();
                        late.push((c, agent));
                    }
                }
            }
        }
        let mins = self.cfg.fanout.child_timeout_mins;
        for (child, agent) in late {
            // The reason first: stopping the agent reports the part too.
            let why = if agent.is_some() {
                format!("timed out after {mins} minutes")
            } else {
                format!("waited {mins} minutes for a free agent")
            };
            self.st.local_queue.retain(|(c, _)| c != &child);
            self.set_stage(&child, Stage::NeedsYou);
            self.fanout_child_done(&child, false, Some(why));
            if let Some(a) = agent {
                let _ = self.stop_agent(&a);
            }
        }
        for id in open {
            self.advance_fanout(&id);
        }
    }

    /// After a restart: parts that were running have no agent any more
    /// (ACP agents died with the old daemon; a pane agent that is still
    /// live keeps its part).
    pub(super) fn resume_fanouts(&mut self) {
        let mut lost: Vec<String> = Vec::new();
        for f in &self.st.fanouts {
            if f.status != FanoutStatus::Running {
                continue;
            }
            for t in &f.subtasks {
                if t.state != TaskState::Running {
                    continue;
                }
                let Some(c) = t.flight_id.as_deref() else {
                    continue;
                };
                let alive = self
                    .flight(c)
                    .and_then(|x| x.agent_id.as_deref())
                    .and_then(|a| self.agent(a))
                    .is_some_and(|a| a.status.is_live());
                if !alive {
                    lost.push(c.to_string());
                }
            }
        }
        for c in lost {
            self.fanout_child_done(&c, false, Some("Tower restarted while it ran".into()));
        }
        // A swarm flight whose split was being planned when Tower stopped:
        // the planner's answer is gone, so it waits for you.
        let stranded: Vec<String> = self
            .st
            .flights
            .iter()
            .filter(|f| f.stage == Stage::InFlight && f.agent_id.is_none())
            .filter(|f| {
                f.role_id
                    .as_deref()
                    .and_then(|r| self.cfg.role(r))
                    .is_some_and(|r| r.swarm.is_some())
            })
            .filter(|f| {
                !self
                    .st
                    .fanouts
                    .iter()
                    .any(|x| x.flight_id == f.id && x.status.is_open())
            })
            .map(|f| f.id.clone())
            .collect();
        for fid in stranded {
            self.note(
                &fid,
                "Tower restarted while planning the split: launch it again",
            );
            self.set_stage(&fid, Stage::NeedsYou);
        }
    }

    /// `tower_await` on a fan-out id (`x-3`).
    pub(super) fn fanout_status(&self, a: &Agent, id: &str) -> Res {
        let f = self.fanout(id).ok_or_else(|| format!("No fan-out {id}"))?;
        if f.return_to != a.id {
            return Err(format!("Fan-out {id} was made by another agent"));
        }
        Ok(match f.status {
            FanoutStatus::Waiting => "status: waiting\nWaiting for the human's clearance.".into(),
            FanoutStatus::Running | FanoutStatus::Paused => {
                let parts: Vec<String> = f
                    .subtasks
                    .iter()
                    .map(|t| format!("{} {}", t.id, t.state.label()))
                    .collect();
                format!(
                    "status: running\nwave {}/{}{}: {}",
                    (f.wave as usize + 1).min(f.waves.len()),
                    f.waves.len(),
                    if f.status == FanoutStatus::Paused {
                        " (paused: the human decides about failed parts)"
                    } else {
                        ""
                    },
                    parts.join(", ")
                )
            }
            FanoutStatus::Done | FanoutStatus::Partial => {
                format!("status: returned\n{}", self.fanout_aggregate(f, false))
            }
            FanoutStatus::Cancelled => "status: rejected\nThe fan-out was cancelled.".into(),
        })
    }
}

/// The handoff packet for one part: its own brief, the files it owns, and
/// the rules for working next to the other parts in the same folder.
fn part_packet(t: &FanoutTask, role: &str) -> HandoffPacket {
    let mut context = Vec::new();
    if !t.brief.trim().is_empty() {
        context.push(t.brief.trim().to_string());
    }
    if !t.target_files.is_empty() {
        context.push(format!("Files you own: {}", t.target_files.join(", ")));
    }
    let mut constraints = vec![
        "Other agents work on other parts of this task in the same folder right now: do not touch, revert or reformat files outside your part.".to_string(),
    ];
    if !t.target_files.is_empty() {
        constraints.push("Only change the files you own.".into());
    }
    if t.read_only {
        constraints.push("Read only: change no files; report what you found.".into());
    }
    let acceptance = if t.acceptance.iter().any(|a| !a.trim().is_empty()) {
        t.acceptance.clone()
    } else if t.read_only {
        vec!["Call tower_return with what you found.".into()]
    } else {
        vec!["Your change builds (or lints) cleanly; call tower_return with what changed and how you checked it.".into()]
    };
    HandoffPacket {
        to_role: role.to_string(),
        goal: if t.title.trim().is_empty() {
            shorten(t.brief.trim(), 200)
        } else {
            t.title.trim().to_string()
        },
        context,
        constraints,
        acceptance,
        ..Default::default()
    }
}
