//! Response-time clocks and acknowledgements on intake flights.
//!
//! Each poll (and each follow-up read of an issue) refreshes a flight's
//! clocks. Every running SLA cycle Tower has not seen yet gets one
//! acknowledgement, decided by `ack.post`: a draft on the flight's
//! timeline (the default), a "Post acknowledgement?" Inbox item, or (armed,
//! `auto`) a comment. Clocks running low raise a Deadline item at half and
//! 80% of their time and at breach.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::issues::Watch;
use crate::policy::{Action, Decision};
use crate::responses::{self, Visibility};
use crate::sla::{self, Urgency};
use crate::store::PendingAction;

/// Remember at most this many acknowledged cycles and warnings.
const MEMORY_CAP: usize = 4000;

impl Engine {
    /// Store a flight's clocks from the tracker (`jsm`), add Tower's own
    /// clock if its watch has `respond_within_hours`, then acknowledge new
    /// cycles. `answered`: someone other than the reporter has replied.
    pub(super) fn set_clocks(&mut self, fid: &str, jsm: Vec<SlaClock>, answered: bool) {
        let Some(f) = self.flight(fid).cloned() else {
            return;
        };
        let Some(w) = self.watch_for(fid) else {
            return;
        };
        let now = now_unix();
        let mut clocks = jsm;
        if let Some(h) = w.respond_within_hours() {
            let answered = answered || self.st.answered.contains(fid);
            if answered {
                self.st.answered.insert(fid.to_string());
            }
            // From when the issue was opened, or when Tower first saw it if
            // that is later: a new watch over an old backlog must not open
            // with every issue already breached.
            let created = self
                .st
                .issue_created
                .get(fid)
                .copied()
                .unwrap_or(0)
                .max(f.created_unix);
            clocks.push(sla::respond_clock(created, h, answered, now));
        }
        if f.sla != clocks {
            if let Some(fm) = self.flight_mut(fid) {
                fm.sla = clocks;
            }
            self.changed();
        }
        self.consider_acks(fid);
    }

    /// One acknowledgement per running cycle Tower has not handled yet.
    fn consider_acks(&mut self, fid: &str) {
        let Some(f) = self.flight(fid).cloned() else {
            return;
        };
        if f.stage == Stage::Landed {
            return;
        }
        let fresh: Vec<SlaClock> = f
            .sla
            .iter()
            .filter(|c| !c.stopped)
            .filter(|c| !self.st.acks.contains_key(&sla::cycle_key(fid, c)))
            .cloned()
            .collect();
        for c in fresh {
            self.offer_ack(fid, &c);
        }
    }

    fn offer_ack(&mut self, fid: &str, clock: &SlaClock) {
        let Some(f) = self.flight(fid).cloned() else {
            return;
        };
        let Some(w) = self.watch_for(fid) else {
            return;
        };
        let key = sla::cycle_key(fid, clock);
        // Handled now, whatever the decision: never offered twice.
        self.remember_ack(&key);
        let template = w
            .ack_template()
            .cloned()
            .unwrap_or_else(|| self.cfg.ack.template.clone());
        let reporter = f
            .issue
            .as_ref()
            .map(|i| i.reporter.clone())
            .unwrap_or_default();
        let text = responses::render(
            &template,
            &self.cfg.ack.signature,
            &responses::Vars {
                key: &f.key,
                summary: &f.title,
                reporter: &reporter,
                sla: &clock.name,
            },
        );
        let visibility = w.ack_visibility().unwrap_or(self.cfg.ack.visibility);
        if visibility == Visibility::Internal && !matches!(w, Watch::Jira(_)) {
            self.note(
                fid,
                format!(
                    "No acknowledgement for {}: {} has no internal comments and [ack] visibility is internal",
                    clock.name,
                    w.label()
                ),
            );
            return;
        }
        let overrides = w.policy();
        match self.decide_action(Action::AckPost, Some(&overrides), false, Some(fid), Some(&f.key)) {
            Decision::Run => self.post_ack(fid, text, visibility),
            Decision::Ask => {
                let id = self.add_inbox(InboxItem {
                    kind: InboxKind::Clearance,
                    title: format!("Acknowledge {} ({})?", f.key, clock.name),
                    detail: format!("Posts a {} comment:\n\n{text}", visibility_words(visibility)),
                    flight_id: Some(fid.to_string()),
                    options: vec![
                        InboxOption {
                            id: "run".into(),
                            label: "Post".into(),
                            primary: true,
                        },
                        InboxOption {
                            id: "not_now".into(),
                            label: "Skip".into(),
                            primary: false,
                        },
                    ],
                    ..Default::default()
                });
                self.st.pending_actions.insert(
                    id,
                    PendingAction::Ack {
                        flight: fid.to_string(),
                        text,
                        internal: visibility == Visibility::Internal,
                    },
                );
            }
            Decision::Draft => self.note(
                fid,
                format!(
                    "Draft acknowledgement for {} ({}; ack.post = draft). Post it yourself:\n\n{text}",
                    clock.name,
                    visibility_words(visibility)
                ),
            ),
            Decision::Skip => {}
        }
    }

    fn remember_ack(&mut self, key: &str) {
        self.st.acks.insert(key.to_string(), now_unix());
        if self.st.acks.len() > MEMORY_CAP {
            // Forget the oldest quarter.
            let mut by_age: Vec<(String, u64)> =
                self.st.acks.iter().map(|(k, t)| (k.clone(), *t)).collect();
            by_age.sort_by_key(|(_, t)| *t);
            for (k, _) in by_age.into_iter().take(MEMORY_CAP / 4) {
                self.st.acks.remove(&k);
            }
        }
        self.changed();
    }

    /// Post an acknowledgement on the flight's issue. Never retried.
    pub(super) fn post_ack(&mut self, fid: &str, text: String, visibility: Visibility) {
        let Some(w) = self.watch_for(fid) else {
            return;
        };
        let Some(f) = self.flight(fid).cloned() else {
            return;
        };
        let number = self.st.flight_issue.get(fid).and_then(|i| i.number);
        let token = match &w {
            Watch::Jira(j) => match crate::jira::secrets::get(&j.site, &self.paths.data) {
                Some(t) => Some(t),
                None => {
                    self.note(fid, "No Jira token: run `tower auth jira <site>`");
                    return;
                }
            },
            _ => None,
        };
        let role = (visibility == Visibility::Internal).then(|| self.cfg.ack.internal_role.clone());
        self.note(
            fid,
            format!(
                "Posting the acknowledgement ({})…",
                visibility_words(visibility)
            ),
        );
        let tx = self.tx.clone();
        let flight_id = fid.to_string();
        let key = f.key.clone();
        tokio::spawn(async move {
            let result = match (&w, token, number) {
                (Watch::Jira(j), Some(t), _) => {
                    crate::jira::post_comment(j, &t, &key, &text, role.as_deref()).await
                }
                (_, _, Some(n)) => crate::issues::comment_cli(&w, n, &text).await.map(|_| None),
                _ => Err("the issue has no number to comment on".into()),
            };
            let _ = tx.send(Input::Acked { flight_id, result });
        });
    }

    pub(super) fn on_acked(&mut self, fid: &str, result: Result<Option<String>, String>) {
        let key = self.flight(fid).map(|f| f.key.clone());
        let audit = result.clone().map(|_| "posted".to_string());
        self.audit_result(Action::AckPost, Some(fid), key.as_deref(), &audit);
        match result {
            Ok(id) => {
                if let Some(id) = id {
                    self.st.own_comments.push(id);
                    if self.st.own_comments.len() > MEMORY_CAP {
                        self.st.own_comments.drain(..MEMORY_CAP / 4);
                    }
                }
                self.st.answered.insert(fid.to_string());
                self.note(fid, "Acknowledgement posted");
                let jsm: Vec<SlaClock> = self
                    .flight(fid)
                    .map(|f| {
                        f.sla
                            .iter()
                            .filter(|c| c.source == "jsm")
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                self.set_clocks(fid, jsm, true);
            }
            Err(e) => self.note(
                fid,
                format!(
                    "Could not confirm the acknowledgement was posted ({}). Tower does not retry: \
                     check the issue before posting it again.",
                    shorten(&e, 200)
                ),
            ),
        }
    }

    /// Raise a Deadline item when a running clock crosses half, 80% or
    /// breach. Called from the tick, at most every 30 s.
    pub(super) fn sla_tick(&mut self) {
        if self
            .last_sla_check
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.last_sla_check = Some(Instant::now());
        let now = now_unix();
        let due: Vec<(String, String, SlaClock, Urgency)> = self
            .st
            .flights
            .iter()
            .filter(|f| f.stage != Stage::Landed)
            .flat_map(|f| {
                f.sla
                    .iter()
                    .map(move |c| (f.id.clone(), f.key.clone(), c.clone(), sla::urgency(c, now)))
            })
            .filter(|(_, _, c, u)| *u >= Urgency::Half && !c.stopped)
            .collect();
        for (fid, key, clock, urgency) in due {
            let cycle = sla::cycle_key(&fid, &clock);
            let seen = self.st.sla_warned.get(&cycle).copied().unwrap_or(0);
            let level = urgency as u8;
            if level <= seen {
                continue;
            }
            self.st.sla_warned.insert(cycle.clone(), level);
            if self.st.sla_warned.len() > MEMORY_CAP {
                self.st.sla_warned.clear();
                self.st.sla_warned.insert(cycle.clone(), level);
            }
            // One item per cycle: the newer warning replaces the older.
            self.st.inbox.retain(|i| {
                !(i.kind == InboxKind::Deadline
                    && i.flight_id.as_deref() == Some(fid.as_str())
                    && i.title.contains(&clock.name))
            });
            let left = clock.due_unix.saturating_sub(now);
            let detail = if urgency == Urgency::Breached {
                format!(
                    "{} breached. Answer the issue or tell the reporter what happens next.",
                    clock.name
                )
            } else {
                format!(
                    "{} ({}): {} left until it breaches.",
                    clock.name,
                    urgency.label(),
                    human(left)
                )
            };
            self.add_inbox(InboxItem {
                kind: InboxKind::Deadline,
                title: format!("{key}: {} {}", clock.name, urgency.label()),
                detail,
                flight_id: Some(fid.clone()),
                options: vec![InboxOption {
                    id: "dismiss".into(),
                    label: "Dismiss".into(),
                    primary: true,
                }],
                ..Default::default()
            });
            self.note(&fid, format!("{}: {}", clock.name, urgency.label()));
        }
    }
}

fn visibility_words(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public: the customer sees it and is notified",
        Visibility::Internal => "internal",
    }
}

/// `3 h 20 min`, `12 min`.
fn human(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{m} min")
    }
}
