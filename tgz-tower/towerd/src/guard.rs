//! The engine's side of `[policy]` and the vendor-terms notes: one call per
//! decision point, so the rest of the engine never reasons about levels,
//! Armed, rate caps or terms itself.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::audit::Entry;
use crate::compliance::{self, Input as TermsInput, Mode, TermsRef, Verdict};
use crate::policy::{self, Action, Decision, Overrides};
use crate::store::PendingAction;

/// Inbox option ids for "Tower wants to do X" items.
const RUN: &str = "run";
const NOT_NOW: &str = "not_now";
const ACCEPT: &str = "accept";

impl Engine {
    pub(super) fn armed(&self) -> bool {
        policy::is_armed(self.armed_until, now_unix())
    }

    pub(super) fn set_armed(&mut self, on: bool) {
        self.armed_until = on.then(|| now_unix() + self.cfg.policy.armed_ttl());
        let text = match self.armed_until {
            Some(_) => format!(
                "Armed: actions set to \"auto\" run on their own for the next {} min",
                self.cfg.policy.armed_ttl() / 60
            ),
            None => "Disarmed: Tower asks before everything".to_string(),
        };
        self.post_system(&text, now_unix());
        self.changed();
    }

    /// Turn Armed off once it has expired, saying so once.
    pub(super) fn expire_armed(&mut self) {
        if self.armed_until.is_some() && !self.armed() {
            self.armed_until = None;
            self.post_system("Armed expired: Tower asks before everything", now_unix());
            self.changed();
        }
    }

    /// Decide `action` for a flight (and the watch it came from), count it
    /// against the rate cap when it runs, and write the audit line.
    /// `needs_terms`: what it would start is in a terms note not yet accepted.
    pub(super) fn decide_action(
        &mut self,
        action: Action,
        overrides: Option<&Overrides>,
        needs_terms: bool,
        flight: Option<&str>,
        target: Option<&str>,
    ) -> Decision {
        let level = self.cfg.policy.level(action, overrides);
        let now = now_unix();
        let armed = self.armed();
        let under = self
            .rates
            .under_cap(action, now, self.cfg.policy.max_auto_per_hour);
        let decision = policy::decide(level, armed || !action.needs_armed(), under, needs_terms);
        if decision == Decision::Run {
            self.rates.record(action, now);
        }
        let outcome = match decision {
            Decision::Run => "started",
            Decision::Ask => "asked",
            Decision::Draft => "drafted",
            Decision::Skip => "skipped",
        };
        let mut e = Entry::new(action, level, decision, outcome);
        e.armed = armed;
        e.flight = flight;
        e.target = target;
        self.log_audit(&e);
        self.count_decision(action, level, decision, flight, target);
        if level == policy::Level::Auto && decision == Decision::Ask {
            let why = if !armed && action.needs_armed() {
                "Tower is not armed"
            } else if needs_terms {
                "a vendor-terms note needs your OK first"
            } else {
                "the hourly limit for automatic actions is reached"
            };
            if let Some(fid) = flight {
                self.note(
                    fid,
                    format!("{} is set to auto, but {why}: asking", action.key()),
                );
            }
        }
        decision
    }

    /// Record how an action that ran (or was clicked) turned out.
    pub(super) fn audit_result(
        &self,
        action: Action,
        flight: Option<&str>,
        target: Option<&str>,
        result: &Result<String, String>,
    ) {
        let outcome = match result {
            Ok(_) => "done".to_string(),
            Err(e) => format!("failed: {}", shorten(e, 200)),
        };
        let level = self.cfg.policy.level(action, None);
        let mut e = Entry::new(action, level, Decision::Run, &outcome);
        e.armed = self.armed();
        e.flight = flight;
        e.target = target;
        self.log_audit(&e);
    }

    fn log_audit(&self, entry: &Entry) {
        if self.demo.is_some() {
            return;
        }
        if let Err(err) = self.audit.append(entry) {
            eprintln!("tower: audit log: {err}");
        }
    }

    /// The overrides of the watch a flight came from.
    pub(super) fn overrides_for(&self, flight_id: &str) -> Option<Overrides> {
        let name = self.st.flight_source.get(flight_id)?;
        self.watch(name).map(|w| w.policy())
    }

    /// The vendor-terms verdict for running `role` with `input`.
    pub(super) fn terms_for(&self, role: &RoleCfg, input: TermsInput) -> Verdict {
        let vendor = compliance::vendor_of(role.driver, &role.command);
        // OpenCode over ACP on local models only: Tower switches every other
        // provider off (`enabled_providers`), so no provider login is in use
        // and the provider-login note does not apply.
        if vendor == compliance::Vendor::Opencode
            && role.driver == DriverKind::Acp
            && role.all_local()
        {
            return Verdict::Allowed;
        }
        let mode = match role.driver {
            DriverKind::Pane => Mode::Pane,
            DriverKind::Acp => Mode::Acp,
            DriverKind::Llm => Mode::Headless,
        };
        compliance::verdict(vendor, role.auth, mode, input)
    }

    /// The note `verdict` points at, if the user has not accepted it yet.
    pub(super) fn terms_pending(&self, verdict: Verdict) -> Option<&'static TermsRef> {
        match verdict {
            Verdict::Warn(t) if verdict.needs_ack(self.st.terms_acks.get(t.id).copied()) => Some(t),
            _ => None,
        }
    }

    /// Ask the user to accept `terms`; `then` runs once they do. One item
    /// per note: a second request just replaces what runs after it.
    pub(super) fn ask_terms(
        &mut self,
        terms: &'static TermsRef,
        vendor: &str,
        flight: Option<&str>,
        then: Option<PendingAction>,
    ) {
        let existing = self.st.pending_actions.iter().find_map(|(id, p)| match p {
            PendingAction::Terms { terms: t, .. } if t == terms.id => Some(id.clone()),
            _ => None,
        });
        let pending = PendingAction::Terms {
            terms: terms.id.to_string(),
            then: then.map(Box::new),
        };
        if let Some(id) = existing {
            self.st.pending_actions.insert(id, pending);
            self.changed();
            return;
        }
        let mut detail = format!("{}\n\n", terms.summary);
        if !terms.quote.is_empty() {
            detail.push_str(&format!(
                "\u{201c}{}\u{201d}\n{}\n\n",
                terms.quote, terms.url
            ));
        }
        detail.push_str(
            "Tower never touches your login: it starts the vendor's own program and the \
             program signs in itself. Accepting means you have read this and run it on \
             your own account at your own risk. Asked again when Tower's terms notes change.",
        );
        detail.push_str(&format!(" (Notes as of {}.)", compliance::MATRIX_AS_OF));
        let id = self.add_inbox(InboxItem {
            kind: InboxKind::Clearance,
            title: format!("{vendor}: vendor terms note"),
            detail,
            flight_id: flight.map(str::to_string),
            options: vec![
                InboxOption {
                    id: ACCEPT.into(),
                    label: "I accept".into(),
                    primary: true,
                },
                InboxOption {
                    id: NOT_NOW.into(),
                    label: "Not now".into(),
                    primary: false,
                },
            ],
            ..Default::default()
        });
        self.st.pending_actions.insert(id, pending);
    }

    /// Before starting `role` on `flight`: refuse (and ask) while a terms
    /// note for it is not accepted. Launch paths call this, so every agent
    /// start is covered, clicked or automatic.
    pub(super) fn check_terms(&mut self, role: &RoleCfg, flight_id: &str) -> Result<(), String> {
        let verdict = self.terms_for(role, TermsInput::Initial);
        let Some(terms) = self.terms_pending(verdict) else {
            return Ok(());
        };
        let vendor = compliance::vendor_of(role.driver, &role.command).label();
        self.ask_terms(
            terms,
            vendor,
            Some(flight_id),
            Some(PendingAction::Launch {
                flight: flight_id.to_string(),
                role: role.id.clone(),
            }),
        );
        Err(format!(
            "{vendor} on this sign-in has a vendor-terms note: accept it in the Inbox to start"
        ))
    }

    /// Ask "Tower wants to …" with Run / Not now.
    pub(super) fn ask_action(
        &mut self,
        title: String,
        detail: String,
        flight: &str,
        pending: PendingAction,
    ) {
        let id = self.add_inbox(InboxItem {
            kind: InboxKind::Clearance,
            title,
            detail,
            flight_id: Some(flight.to_string()),
            options: vec![
                InboxOption {
                    id: RUN.into(),
                    label: "Run".into(),
                    primary: true,
                },
                InboxOption {
                    id: NOT_NOW.into(),
                    label: "Not now".into(),
                    primary: false,
                },
            ],
            ..Default::default()
        });
        self.st.pending_actions.insert(id, pending);
    }

    /// The user answered a Run / accept item.
    pub(super) fn resolve_pending_action(&mut self, item_id: &str, opt: &str) -> Res {
        let pending = self
            .st
            .pending_actions
            .remove(item_id)
            .ok_or("That item is gone")?;
        self.st.inbox.retain(|i| i.id != item_id);
        self.changed();
        let yes = opt == RUN || opt == ACCEPT || opt == "approve";
        if !yes {
            return Ok("Left for later".into());
        }
        self.run_pending(pending)
    }

    fn run_pending(&mut self, pending: PendingAction) -> Res {
        match pending {
            PendingAction::Terms { terms, then } => {
                self.st
                    .terms_acks
                    .insert(terms.clone(), compliance::MATRIX_VERSION);
                self.post_system(
                    &format!("Vendor terms note \"{terms}\" accepted"),
                    now_unix(),
                );
                self.changed();
                match then {
                    Some(next) => self.run_pending(*next),
                    None => Ok("Accepted".into()),
                }
            }
            PendingAction::Launch { flight, role } => {
                let still_cleared = self
                    .flight(&flight)
                    .is_some_and(|f| matches!(f.stage, Stage::Cleared | Stage::Holding));
                if !still_cleared {
                    return Ok("That flight has moved on".into());
                }
                let res = self.launch_flight(&flight, &role);
                self.audit_result(Action::IntakeLaunch, Some(&flight), Some(&role), &res);
                if let Err(e) = &res {
                    self.note(&flight, format!("Launch failed: {e}"));
                }
                res
            }
            PendingAction::Ack {
                flight,
                text,
                internal,
            } => {
                if self.flight(&flight).is_none() {
                    return Ok("That flight is gone".into());
                }
                let vis = if internal {
                    crate::responses::Visibility::Internal
                } else {
                    crate::responses::Visibility::Public
                };
                self.post_ack(&flight, text, vis);
                Ok("Posting".into())
            }
            PendingAction::Message { agent, text, fyi } => {
                let flight = self.agent(&agent).and_then(|a| a.flight_id.clone());
                let res = if fyi {
                    self.deliver_fyi(&agent, text)
                } else {
                    self.deliver(&agent, text)
                }
                .map(|_| "sent".to_string());
                self.audit_result(Action::PaneMessage, flight.as_deref(), Some(&agent), &res);
                res
            }
            PendingAction::Scout { name } => self.run_scout(&name, true),
            PendingAction::Schedule { id } => self.fire_schedule(&id, true),
            PendingAction::ResumeSchedule { id } => self.set_schedule_enabled(&id, true),
            PendingAction::Triage { flight, fallback } => {
                if self.flight(&flight).is_none() {
                    return Ok("That flight is gone".into());
                }
                self.run_intake_triage(&flight, fallback);
                Ok("Triage started".into())
            }
        }
    }

    /// Send `text`, which Tower or another agent wrote, to `agent_id`, as
    /// `pane.message` allows: on "ask" it waits in the Inbox (several for
    /// the same agent are merged into one item). What you write yourself
    /// goes through `deliver` directly.
    pub(super) fn deliver_tower(&mut self, agent_id: &str, text: String, what: &str) {
        self.deliver_tower_as(agent_id, text, what, false);
    }

    /// Like `deliver_tower`, for an update the agent should know about but
    /// need not stop for (an issue comment): a busy Claude pane agent gets
    /// it after its next tool call instead of when its turn ends.
    pub(super) fn deliver_tower_update(&mut self, agent_id: &str, text: String, what: &str) {
        self.deliver_tower_as(agent_id, text, what, true);
    }

    fn deliver_tower_as(&mut self, agent_id: &str, text: String, what: &str, fyi: bool) {
        let Some(a) = self.agent(agent_id).cloned() else {
            return;
        };
        // A CLI you started yourself reads results with tower_await.
        if a.adopted {
            return;
        }
        let needs_terms = self
            .cfg
            .role(&a.role_id)
            .cloned()
            .map(|r| {
                let v = self.terms_for(&r, TermsInput::Tower);
                self.terms_pending(v).is_some()
            })
            .unwrap_or(false);
        let fid = a.flight_id.clone();
        let overrides = fid.as_deref().and_then(|f| self.overrides_for(f));
        match self.decide_action(
            Action::PaneMessage,
            overrides.as_ref(),
            needs_terms,
            fid.as_deref(),
            Some(agent_id),
        ) {
            Decision::Run => {
                let res = if fyi {
                    self.deliver_fyi(agent_id, text)
                } else {
                    self.deliver(agent_id, text)
                }
                .map(|_| "sent".to_string());
                self.audit_result(Action::PaneMessage, fid.as_deref(), Some(agent_id), &res);
            }
            Decision::Ask => self.ask_message(&a, text, what, fyi),
            Decision::Draft => {
                if let Some(f) = fid.as_deref() {
                    self.note(
                        f,
                        format!(
                            "Not sent to {agent_id} (pane.message = draft), {what}:\n{}",
                            shorten(&text, 2000)
                        ),
                    );
                }
            }
            Decision::Skip => {}
        }
    }

    fn ask_message(&mut self, a: &Agent, text: String, what: &str, fyi: bool) {
        let existing = self.st.pending_actions.iter().find_map(|(id, p)| match p {
            PendingAction::Message { agent, .. } if agent == &a.id => Some(id.clone()),
            _ => None,
        });
        if let Some(id) = existing {
            if let Some(PendingAction::Message {
                text: queued,
                fyi: queued_fyi,
                ..
            }) = self.st.pending_actions.get_mut(&id)
            {
                queued.push_str("\n\n");
                queued.push_str(&text);
                // Merged with a real message, the whole lot is one.
                *queued_fyi = *queued_fyi && fyi;
                let detail = shorten(queued, 1200);
                if let Some(item) = self.st.inbox.iter_mut().find(|i| i.id == id) {
                    item.title = format!("Send {} messages to {}?", "several", a.id);
                    item.detail = detail;
                }
                self.changed();
            }
            return;
        }
        let id = self.add_inbox(InboxItem {
            kind: InboxKind::Clearance,
            title: format!("Send {what} to {}?", a.id),
            detail: shorten(&text, 1200),
            flight_id: a.flight_id.clone(),
            agent_id: Some(a.id.clone()),
            options: vec![
                InboxOption {
                    id: RUN.into(),
                    label: "Send".into(),
                    primary: true,
                },
                InboxOption {
                    id: NOT_NOW.into(),
                    label: "Drop".into(),
                    primary: false,
                },
            ],
            ..Default::default()
        });
        self.st.pending_actions.insert(
            id,
            PendingAction::Message {
                agent: a.id.clone(),
                text,
                fyi,
            },
        );
    }

    /// The effective level of every action, for the Policy view.
    pub(super) fn policy_rows(&self) -> Vec<PolicyRow> {
        Action::ALL
            .iter()
            .map(|a| PolicyRow {
                action: a.key().to_string(),
                level: self.cfg.policy.level(*a, None).label().to_string(),
                configured: self.cfg.policy.actions.contains_key(a.key()),
            })
            .collect()
    }
}
