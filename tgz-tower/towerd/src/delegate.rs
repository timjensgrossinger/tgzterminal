//! Handing subtasks to local models: the `tower_models`, `tower_delegate`
//! and `tower_await` tools, the queue that waits for a busy model, and the
//! check a local result gets (the repo's gate commands) before it goes back
//! to the agent that asked.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::config::LOCAL_ROLE;
use crate::policy::{Action, Level};

/// How often an agent that ends its turn without `tower_return` is told to
/// report back before Tower gives up on it.
pub const MAX_REMINDERS: u8 = 2;

/// What `release` returns instead of an agent id when the model was busy.
pub(super) const QUEUED: &str = "(queued)";

/// How well a model's `good_for` matches a task: one point per strength
/// that shares a word (longer than two letters) with it.
pub fn strength_score(good_for: &[String], task: &str) -> usize {
    let words: Vec<String> = task
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect();
    good_for
        .iter()
        .filter(|g| {
            g.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
                .any(|w| {
                    words
                        .iter()
                        .any(|t| t.starts_with(w) || w.starts_with(t.as_str()))
                })
        })
        .count()
}

impl Engine {
    /// Agents on `model` that are working on an open flight. One that sits
    /// idle (Holding) is not using the model.
    pub(super) fn model_load(&self, model: &str) -> u32 {
        self.st
            .agents
            .iter()
            .filter(|a| {
                a.status.is_live() && a.status != AgentStatus::Holding && a.model.id == model
            })
            .filter(|a| {
                a.flight_id
                    .as_deref()
                    .and_then(|f| self.flight(f))
                    .map(|f| f.stage != Stage::Landed)
                    .unwrap_or(true)
            })
            .count() as u32
    }

    fn model_capacity(&self, model: &str) -> u32 {
        self.cfg
            .models
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.max_concurrency.max(1))
            .unwrap_or(1)
    }

    pub(super) fn model_free(&self, model: &str) -> bool {
        self.model_load(model) < self.model_capacity(model)
    }

    /// Live agents of `role_id` on flights that have not landed.
    pub(super) fn role_load(&self, role_id: &str) -> u32 {
        self.st
            .agents
            .iter()
            .filter(|a| a.role_id == role_id && a.status.is_live())
            .filter(|a| {
                a.flight_id
                    .as_deref()
                    .and_then(|f| self.flight(f))
                    .map(|f| f.stage != Stage::Landed)
                    .unwrap_or(true)
            })
            .count() as u32
    }

    /// Are older packets waiting for `model` in front of `child_id`?
    pub(super) fn queued_ahead(&self, child_id: &str, model: &str) -> bool {
        let pos = self
            .st
            .local_queue
            .iter()
            .position(|(c, _)| c == child_id)
            .unwrap_or(self.st.local_queue.len());
        self.st.local_queue[..pos].iter().any(|(c, _)| {
            self.child_packet(c).is_some_and(|p| {
                p.state == HandoffState::Draft && p.model.as_deref() == Some(model)
            })
        })
    }

    /// `tower_models`: the local models an agent may hand work to.
    pub(super) fn tool_models(&self) -> Res {
        let ladder = match self.cfg.role(LOCAL_ROLE) {
            Some(r) if r.all_local() => r.ladder.clone(),
            _ => vec![],
        };
        let infos: Vec<_> = self
            .model_infos()
            .into_iter()
            .filter(|m| ladder.contains(&m.id))
            .collect();
        if infos.is_empty() {
            return Ok("No local models are set up for delegation. Do the work yourself; the human can add models in Tower's Models view.".into());
        }
        let mut out = String::from(
            "Local models (inside the user's network). Hand one a self-contained subtask with tower_delegate:\n",
        );
        for m in infos {
            let mut facts = vec![];
            if !m.size.is_empty() {
                facts.push(m.size.clone());
            }
            if let Some(c) = m.context {
                facts.push(format!("{c} tokens context"));
            }
            out.push_str(&format!("- {} ({}", m.id, m.label));
            if !facts.is_empty() {
                out.push_str(&format!(", {}", facts.join(", ")));
            }
            out.push(')');
            if !m.good_for.is_empty() {
                out.push_str(&format!("; good for: {}", m.good_for.join(", ")));
            }
            if !m.notes.is_empty() {
                out.push_str(&format!("; {}", m.notes.replace('\n', " ")));
            }
            match &m.why {
                Some(w) => out.push_str(&format!("; NOT AVAILABLE: {w}")),
                None => out.push_str(&format!("; {} of {} busy", m.busy, m.max_concurrency)),
            }
            out.push('\n');
        }
        Ok(out)
    }

    /// The model `tower_delegate` runs on: `want`, or for `auto` the free,
    /// reachable model whose strengths best match `task`.
    fn pick_local_model(
        &self,
        ladder: &[String],
        want: &str,
        task: &str,
    ) -> Result<String, String> {
        if !want.is_empty() && want != "auto" {
            if !ladder.iter().any(|m| m == want) {
                return Err(format!(
                    "{want} is not a local model Tower can delegate to; tower_models lists them"
                ));
            }
            if let Some(why) = config::served_problem(want, &self.endpoints) {
                return Err(format!("{want} is not available: {why}"));
            }
            return Ok(want.to_string());
        }
        let mut reasons = vec![];
        // A free model first, then the best match, then models.toml order.
        let mut best: Option<(bool, usize, usize, &String)> = None;
        for (i, id) in ladder.iter().enumerate() {
            if let Some(why) = config::served_problem(id, &self.endpoints) {
                reasons.push(format!("{id}: {why}"));
                continue;
            }
            let good: Vec<String> = self
                .cfg
                .models
                .iter()
                .find(|m| &m.id == id)
                .map(|m| m.good_for.clone())
                .unwrap_or_default();
            let key = (
                self.model_free(id),
                strength_score(&good, task),
                usize::MAX - i,
                id,
            );
            if best
                .map(|b| (key.0, key.1, key.2) > (b.0, b.1, b.2))
                .unwrap_or(true)
            {
                best = Some(key);
            }
        }
        best.map(|b| b.3.clone()).ok_or_else(|| {
            format!(
                "No local model is reachable right now ({}). Do the work yourself.",
                reasons.join("; ")
            )
        })
    }

    /// `tower_delegate`: hand a subtask to a local model.
    pub(super) fn tool_delegate(&mut self, a: &Agent, fid: &str, args: &Value) -> Res {
        let s = |k: &str| {
            args.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let list = |k: &str| -> Vec<String> {
            match args.get(k) {
                Some(Value::Array(v)) => v
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
                Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
                _ => vec![],
            }
        };
        let role = self.cfg.role(LOCAL_ROLE).cloned().ok_or(
            "No local model is set up for delegation: the human can add one in Tower's Models view. Do the work yourself.",
        )?;
        if !role.all_local() {
            return Err(format!(
                "Role {LOCAL_ROLE} runs on a model outside the user's network; tower_delegate only hands work to local models. Use tower_handoff instead."
            ));
        }
        let goal = s("goal");
        let mut context = list("context");
        context.extend(list("files").into_iter().map(|f| format!("File: {f}")));
        let task = format!("{goal} {}", context.join(" "));
        let model = self.pick_local_model(&role.ladder, &s("model"), &task)?;
        let packet = HandoffPacket {
            to_role: LOCAL_ROLE.into(),
            goal,
            context,
            constraints: list("constraints"),
            acceptance: list("acceptance"),
            model: Some(model),
            local_only: args
                .get("local_only")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Default::default()
        };
        self.handoff(fid, &a.role_id, &a.id, packet, false)
    }

    /// Put a released-but-waiting handoff in line: a local one for its
    /// model, a fan-out part for a free slot. `why` is what it waits for.
    pub(super) fn queue_local(&mut self, child_id: &str, packet_id: &str, why: &str) {
        let entry = (child_id.to_string(), packet_id.to_string());
        if !self.st.local_queue.contains(&entry) {
            self.st.local_queue.push(entry);
            self.note(child_id, format!("Queued: {why}"));
            self.set_stage(child_id, Stage::Holding);
        }
    }

    /// Start queued local handoffs whose model has room now, oldest first.
    pub(super) fn drain_local_queue(&mut self) {
        if self.st.local_queue.is_empty() {
            return;
        }
        // `release` decides: it starts the head of each model's line and
        // leaves the rest queued.
        for (child, pid) in self.st.local_queue.clone() {
            let waiting = self
                .child_packet(&child)
                .is_some_and(|p| p.id == pid && p.state == HandoffState::Draft);
            if !waiting {
                self.st.local_queue.retain(|(c, _)| c != &child);
                continue;
            }
            // A failed start is noted on both flights by `release`.
            if !matches!(self.release(&child, &pid).as_deref(), Ok(QUEUED)) {
                self.changed();
            }
        }
    }

    /// `tower_await`: how a handoff the caller made is doing, without
    /// waiting (the shim polls this).
    pub(super) fn tool_await(&self, a: &Agent, args: &Value) -> Res {
        let id = args
            .get("handoff_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() {
            return Err("handoff_id is missing".into());
        }
        // A fan-out's id: how its parts are doing, or the combined result.
        if id.starts_with("x-") {
            return self.fanout_status(a, &id);
        }
        let child = self
            .st
            .flights
            .iter()
            // The child is the flight whose own packet this is: the
            // sender's flight records the packet too.
            .find(|f| {
                f.parent_id.is_some()
                    && self.child_packet(&f.id).map(|p| p.id).as_deref() == Some(id.as_str())
            })
            .ok_or_else(|| format!("No handoff {id}"))?;
        let packet = self.child_packet(&child.id).ok_or("No such handoff")?;
        if packet.return_to != a.id {
            return Err(format!("Handoff {id} was made by another agent"));
        }
        let queued = self.st.local_queue.iter().any(|(c, _)| c == &child.id);
        Ok(match packet.state {
            HandoffState::Draft if queued => format!(
                "status: queued\n{} is busy; it starts by itself when the model is free.",
                packet.model.as_deref().unwrap_or("The model")
            ),
            HandoffState::Draft if child.stage == Stage::NeedsYou => {
                let why = child
                    .timeline
                    .iter()
                    .rev()
                    .find_map(|e| match &e.body {
                        TimelineBody::System { text } if text.starts_with("Could not launch") => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "It could not start".into());
                format!("status: failed\n{why}. Do the work yourself, or ask the human.")
            }
            HandoffState::Draft => "status: waiting\nWaiting for the human's clearance.".into(),
            HandoffState::Rejected => "status: rejected\nThe human rejected this handoff.".into(),
            HandoffState::Accepted
                if child.stage == Stage::NeedsYou
                    && !child
                        .agent_id
                        .as_deref()
                        .and_then(|aid| self.agent(aid))
                        .is_some_and(|ag| ag.status.is_live()) =>
            {
                "status: failed\nThe model stopped without a result. Do that part yourself, or ask the human.".into()
            }
            _ if self.st.local_checks.contains_key(&child.id) => {
                "status: checking\nThe result is in; Tower is running the repo's checks on it."
                    .into()
            }
            HandoffState::Returned => format!(
                "status: returned\n{}",
                self.redact_for(&child.id, &a.id, self.result_text(child))
            ),
            _ => {
                let activity = child
                    .agent_id
                    .as_deref()
                    .and_then(|aid| self.agent(aid))
                    .and_then(|ag| ag.activity.clone())
                    // Tool titles name files and searches: not for a cloud
                    // sender of local-only work.
                    .filter(|_| !self.redacts(&child.id, &a.id))
                    .unwrap_or_else(|| "working".into());
                format!("status: running\n{activity}")
            }
        })
    }

    /// What a returned child flight reported, with its check, for
    /// `tower_await`.
    pub(super) fn result_text(&self, child: &Flight) -> String {
        let mut summary = None;
        let mut checks = None;
        for e in &child.timeline {
            if let TimelineBody::Message { from, text } = &e.body {
                if from == "gates" {
                    checks = Some(text.clone());
                } else if child.agent_id.as_deref() == Some(from.as_str()) {
                    summary = Some(text.clone());
                }
            }
        }
        let mut out = summary.unwrap_or_else(|| "(no summary)".into());
        if let Some(c) = checks {
            out.push_str("\n\n");
            out.push_str(&c);
        }
        out
    }

    /// Does this returned handoff go back without asking? `handoff.local`
    /// at `auto` covers the answer to an agent's own local handoff.
    pub(super) fn local_return_is_auto(&self, a: &Agent, child_id: &str) -> bool {
        a.model.locality == Locality::Local && {
            let overrides = self.overrides_for(child_id);
            self.cfg
                .policy
                .level(Action::HandoffLocal, overrides.as_ref())
                == Level::Auto
        }
    }

    /// A local result came back: run the repo's gate commands on it first
    /// when it has any, then hand it to the sender.
    pub(super) fn return_local(&mut self, child_id: &str, return_to: &str, msg: String) {
        let child = match self.flight(child_id).cloned() {
            Some(f) => f,
            None => return,
        };
        let dir = PathBuf::from(child.workdir.clone().unwrap_or_else(|| ".".into()));
        let gates_cfg = config::gates_for(&dir, &self.cfg.gates);
        if gates_cfg.commands.is_empty() {
            self.send_local_result(child_id, return_to, msg);
            return;
        }
        self.st
            .local_checks
            .insert(child_id.to_string(), (return_to.to_string(), msg));
        self.note(child_id, "Checking the result: gate commands running");
        let tx = self.tx.clone();
        let fid = child_id.to_string();
        let commands = gates_cfg.commands.clone();
        let timeout = Duration::from_secs(gates_cfg.timeout_secs.max(1));
        tokio::spawn(async move {
            let run = gates::run_gates(&dir, &commands, timeout).await;
            let _ = tx.send(Input::LocalChecked {
                flight_id: fid,
                run,
            });
        });
    }

    /// The gate commands on a local result finished.
    pub(super) fn on_local_checked(&mut self, child_id: &str, run: GateRun) {
        let Some((return_to, msg)) = self.st.local_checks.remove(child_id) else {
            return;
        };
        // A cloud sender of local-only work hears pass/fail, not the output.
        let quiet = self.redacts(child_id, &return_to);
        let check = match &run.failure {
            Some(_) if quiet => {
                let failed = run
                    .results
                    .iter()
                    .find(|g| g.state == GateState::Failed)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                format!("Check FAILED: `{failed}`. Review the change before you rely on it.")
            }
            Some(out) => {
                let failed = run
                    .results
                    .iter()
                    .find(|g| g.state == GateState::Failed)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                format!(
                    "Check FAILED: `{failed}`. Review the change before you rely on it.\n{}",
                    shorten(out, 1500)
                )
            }
            None => {
                let names: Vec<_> = run.results.iter().map(|g| g.name.clone()).collect();
                format!("Checks passed: {}", names.join(", "))
            }
        };
        if let Some(f) = self.flight_mut(child_id) {
            f.gates = run.results.clone();
        }
        self.timeline(
            child_id,
            TimelineBody::Message {
                from: "gates".into(),
                text: check.clone(),
            },
        );
        self.send_local_result(child_id, &return_to, format!("{msg}\n\n{check}"));
    }

    /// An ACP helper ended its turn on a handoff packet without calling
    /// `tower_return`: remind it, and after `MAX_REMINDERS` stop it and tell
    /// the sender, so its model is free again and nobody waits forever.
    pub(super) fn remind_unreturned(&mut self, agent_id: &str, stop_reason: &str) {
        // A turn you cancelled, or one the model refused, is not forgetting.
        if matches!(stop_reason, "cancelled" | "refusal") {
            return;
        }
        let Some(a) = self.agent(agent_id).cloned() else {
            return;
        };
        if a.driver != DriverKind::Acp || a.status != AgentStatus::Holding {
            return;
        }
        let Some(fid) = a.flight_id.clone() else {
            return;
        };
        let open = self.flight(&fid).is_some_and(|f| f.parent_id.is_some())
            && self
                .child_packet(&fid)
                .is_some_and(|p| p.state == HandoffState::Accepted);
        if !open {
            return;
        }
        let n = {
            let n = self.reminded.entry(agent_id.to_string()).or_insert(0);
            *n += 1;
            *n
        };
        if n <= MAX_REMINDERS {
            self.note(&fid, format!("Reminded {agent_id} to report back ({n})"));
            let _ = self.deliver(
                agent_id,
                "[Tower] You ended your turn without calling tower_return, so the agent that handed you this packet is still waiting. Finish the packet and call tower_return with what changed and how you checked it, or call tower_return saying exactly what blocks you.".into(),
            );
            return;
        }
        self.reminded.remove(agent_id);
        let key = self.flight(&fid).map(|f| f.key.clone()).unwrap_or_default();
        self.note(
            &fid,
            "Stopped: it ended its turn without tower_return after every reminder",
        );
        self.set_stage(&fid, Stage::NeedsYou);
        // A fan-out part: the barrier hears it, not the lead one by one.
        if self.flight(&fid).is_some_and(|f| f.group_id.is_some()) {
            self.fanout_child_done(
                &fid,
                false,
                Some("stopped without a result: it never called tower_return".into()),
            );
            let _ = self.stop_agent(agent_id);
            self.changed();
            return;
        }
        let _ = self.stop_agent(agent_id);
        if let Some(p) = self.child_packet(&fid) {
            if let Some(parent) = self.flight(&fid).and_then(|f| f.parent_id.clone()) {
                self.note(
                    &parent,
                    format!("Handoff {} ({key}) stopped without a result", p.id),
                );
            }
            if !p.return_to.is_empty() {
                let _ = self.deliver(
                    &p.return_to,
                    format!(
                        "[Tower] Handoff {} ({key}) stopped without a result: the model never reported back. Do that part yourself, or ask the human.",
                        p.id
                    ),
                );
            }
        }
        self.changed();
    }

    /// After a restart: run the checks that were in progress again.
    pub(super) fn resume_local_checks(&mut self) {
        if self.checks_resumed {
            return;
        }
        self.checks_resumed = true;
        for (child, (to, msg)) in std::mem::take(&mut self.st.local_checks) {
            self.return_local(&child, &to, msg);
        }
    }

    fn send_local_result(&mut self, child_id: &str, return_to: &str, msg: String) {
        let live = self.agent(return_to).is_some_and(|r| r.status.is_live());
        if !live {
            self.post(TEAM, "tower", &shorten(&msg, 600));
            return;
        }
        let res = self.deliver(return_to, msg).map(|_| "sent".to_string());
        self.audit_result(Action::HandoffLocal, Some(child_id), Some(return_to), &res);
    }
}

#[cfg(test)]
mod tests {
    use super::strength_score;

    #[test]
    fn strengths_match_on_shared_words() {
        let good = vec![
            "code edits".to_string(),
            "docs".to_string(),
            "tests".to_string(),
        ];
        assert_eq!(strength_score(&good, "Write docs for the parser"), 1);
        assert_eq!(strength_score(&good, "Add tests and edit the docs"), 3);
        assert_eq!(strength_score(&good, "Plan the migration"), 0);
        assert_eq!(strength_score(&[], "anything"), 0);
    }
}
