//! What Tower may do on its own, per action: `[policy]` in `tower.toml`.
//!
//! ```toml
//! [policy]
//! armed_ttl_secs = 28800        # how long "Armed" lasts once switched on
//! max_auto_per_hour = 20        # per action, across all watches
//!
//! [policy.actions]
//! "intake.launch" = "confirm"   # off | draft | confirm | auto
//! "ack.post" = "auto"
//! ```
//! A watch can override any action with its own `policy = { … }` table.
//!
//! `auto` only means something while Tower is **armed**: a switch the user
//! flips in the GUI that turns itself off again after `armed_ttl_secs`.
//! Disarmed, every `auto` is asked like `confirm`, so nothing ever starts by
//! itself unless the user chose both the level and the moment.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// Everything Tower can do that reaches outside it or starts an agent.
/// There is deliberately no action for sending mail or Teams messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Start an agent on a new intake item.
    IntakeLaunch,
    /// Ask a triage agent which role an intake item needs.
    IntakeTriage,
    /// Post the canned "received" acknowledgement on an issue.
    AckPost,
    /// Comment on an issue (landing summary, an agent's question).
    IssueComment,
    /// Move an issue to another status.
    IssueTransition,
    /// Push a branch and open a draft PR/MR.
    PrDraft,
    /// Land a flight whose gates passed.
    Land,
    /// Release a handoff to the next role.
    HandoffRelease,
    /// Split one task into parts that run as parallel child flights, wave
    /// by wave (`tower_fanout`, or a swarm role). Decided once per plan,
    /// not once per part.
    HandoffFanout,
    /// Hand a subtask to a model inside your own network, and give its
    /// result back to the agent that asked. Like schedules, its `auto` needs
    /// no Armed: nothing leaves your network and the result only goes back
    /// to the agent that asked for it.
    HandoffLocal,
    /// Run a scout (an agent that reads mail/Teams for new items).
    ScoutRun,
    /// Send a message Tower generated into a running agent.
    PaneMessage,
    /// Start a flight from a schedule. The one action whose `auto` needs no
    /// Armed: creating or enabling the schedule was the consent.
    ScheduleRun,
}

impl Action {
    pub const ALL: [Action; 13] = [
        Action::IntakeLaunch,
        Action::IntakeTriage,
        Action::AckPost,
        Action::IssueComment,
        Action::IssueTransition,
        Action::PrDraft,
        Action::Land,
        Action::HandoffRelease,
        Action::HandoffFanout,
        Action::HandoffLocal,
        Action::ScoutRun,
        Action::PaneMessage,
        Action::ScheduleRun,
    ];

    /// The key used in `[policy.actions]` and the audit log.
    pub fn key(self) -> &'static str {
        match self {
            Action::IntakeLaunch => "intake.launch",
            Action::IntakeTriage => "intake.triage",
            Action::AckPost => "ack.post",
            Action::IssueComment => "issue.comment",
            Action::IssueTransition => "issue.transition",
            Action::PrDraft => "pr.draft",
            Action::Land => "land",
            Action::HandoffRelease => "handoff.release",
            Action::HandoffFanout => "handoff.fanout",
            Action::HandoffLocal => "handoff.local",
            Action::ScoutRun => "scout.run",
            Action::PaneMessage => "pane.message",
            Action::ScheduleRun => "schedule.run",
        }
    }

    pub fn from_key(key: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.key() == key)
    }

    /// Out of the box: ask for everything, show acks as a draft to copy,
    /// and run no scouts.
    pub fn default_level(self) -> Level {
        match self {
            Action::AckPost => Level::Draft,
            Action::ScoutRun => Level::Off,
            Action::ScheduleRun | Action::HandoffLocal => Level::Auto,
            _ => Level::Confirm,
        }
    }

    /// Does `auto` wait for Armed? Everything but schedules, which the user
    /// set up one by one, and local handoffs, which never leave your network
    /// and only answer the agent that asked.
    pub fn needs_armed(self) -> bool {
        !matches!(self, Action::ScheduleRun | Action::HandoffLocal)
    }
}

/// What a hands-free flight runs instead of asking, while Tower is armed:
/// releasing a handoff, starting a fan-out, passing Tower's text on to an
/// agent. Landing is deliberately not one of them.
pub const HANDS_FREE: [Action; 3] = [
    Action::HandoffRelease,
    Action::HandoffFanout,
    Action::PaneMessage,
];

/// Raise the `HANDS_FREE` actions that would ask (`confirm`) to `auto`.
/// `off` and `draft` stay: hands-free never overrides "never" or "only
/// prepare it". `auto` still runs only while armed (`needs_armed`).
pub fn raise_for_hands_free(cfg: &PolicyCfg, o: &mut Overrides) {
    for a in HANDS_FREE {
        if cfg.level(a, Some(o)) == Level::Confirm {
            o.insert(a.key().to_string(), Level::Auto);
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Never.
    Off,
    /// Prepare it and show it; the user does it by hand.
    Draft,
    /// Ask in the Inbox; one click does it.
    Confirm,
    /// Do it, but only while armed and under the rate cap.
    Auto,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Draft => "draft",
            Level::Confirm => "confirm",
            Level::Auto => "auto",
        }
    }
}

/// What to do now.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Skip,
    Draft,
    Ask,
    Run,
}

/// Per-action overrides, as written in TOML: `"intake.launch" = "auto"`.
pub type Overrides = BTreeMap<String, Level>;

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct PolicyCfg {
    #[serde(default)]
    pub actions: Overrides,
    #[serde(default = "default_ttl")]
    pub armed_ttl_secs: u64,
    #[serde(default = "default_cap")]
    pub max_auto_per_hour: u32,
}

impl Default for PolicyCfg {
    fn default() -> Self {
        Self {
            actions: Overrides::new(),
            armed_ttl_secs: default_ttl(),
            max_auto_per_hour: default_cap(),
        }
    }
}

fn default_ttl() -> u64 {
    8 * 3600
}

fn default_cap() -> u32 {
    20
}

/// The longest Armed may last, whatever the config says.
pub const MAX_ARMED_SECS: u64 = 24 * 3600;

impl PolicyCfg {
    /// The level for `action`: the watch's override, else `[policy.actions]`,
    /// else the built-in default.
    pub fn level(&self, action: Action, watch: Option<&Overrides>) -> Level {
        watch
            .and_then(|w| w.get(action.key()))
            .or_else(|| self.actions.get(action.key()))
            .copied()
            .unwrap_or_else(|| action.default_level())
    }

    pub fn armed_ttl(&self) -> u64 {
        self.armed_ttl_secs.clamp(60, MAX_ARMED_SECS)
    }
}

/// Keys in an override table that name no action (typos), for the config
/// error list.
pub fn unknown_keys(overrides: &Overrides) -> Vec<String> {
    overrides
        .keys()
        .filter(|k| Action::from_key(k).is_none())
        .cloned()
        .collect()
}

/// Is Tower armed at `now`? `armed_until` is a unix time.
pub fn is_armed(armed_until: Option<u64>, now: u64) -> bool {
    armed_until.is_some_and(|until| now < until)
}

/// The hard part of every decision. `needs_terms_ack`: the engine this would
/// start or drive is in a terms grey zone the user has not accepted yet.
pub fn decide(level: Level, armed: bool, under_cap: bool, needs_terms_ack: bool) -> Decision {
    match level {
        Level::Off => Decision::Skip,
        Level::Draft => Decision::Draft,
        Level::Confirm => Decision::Ask,
        Level::Auto if armed && under_cap && !needs_terms_ack => Decision::Run,
        Level::Auto => Decision::Ask,
    }
}

/// When each action last ran on its own, for `max_auto_per_hour`. Kept in
/// memory only: a restart forgets it, and also disarms.
#[derive(Default, Debug)]
pub struct RateLog {
    runs: HashMap<Action, VecDeque<u64>>,
}

const HOUR: u64 = 3600;

impl RateLog {
    /// Would one more automatic `action` at `now` stay within `cap`?
    pub fn under_cap(&mut self, action: Action, now: u64, cap: u32) -> bool {
        let q = self.runs.entry(action).or_default();
        while q.front().is_some_and(|&t| t + HOUR <= now) {
            q.pop_front();
        }
        q.len() < cap as usize
    }

    /// Count an automatic run.
    pub fn record(&mut self, action: Action, now: u64) {
        let q = self.runs.entry(action).or_default();
        q.push_back(now);
        // Never grows past an hour's worth, however high the cap.
        while q.front().is_some_and(|&t| t + HOUR <= now) {
            q.pop_front();
        }
    }
}

/// The pre-policy knobs, mapped onto levels so old configs keep their
/// meaning (now subject to Armed). Returns the override to apply and a
/// warning to show, or `None` when the knob is off.
pub fn legacy(action: Action, knob: &str, on: bool) -> Option<(Level, String)> {
    on.then(|| {
        (
            Level::Auto,
            format!(
                "`{knob}` is deprecated: it now means \"{}\" = \"auto\" in [policy.actions], \
                 which only runs while Tower is armed",
                action.key()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_free_raises_only_what_would_ask_and_never_landing() {
        let mut cfg = PolicyCfg::default();
        cfg.actions.insert("pane.message".into(), Level::Draft);
        let mut o = Overrides::new();
        raise_for_hands_free(&cfg, &mut o);
        assert_eq!(cfg.level(Action::HandoffRelease, Some(&o)), Level::Auto);
        assert_eq!(cfg.level(Action::HandoffFanout, Some(&o)), Level::Auto);
        assert_eq!(
            cfg.level(Action::PaneMessage, Some(&o)),
            Level::Draft,
            "draft stays"
        );
        assert_eq!(
            cfg.level(Action::Land, Some(&o)),
            Level::Confirm,
            "landing still asks"
        );
        // A watch that switched a release off keeps it off.
        let mut o = Overrides::new();
        o.insert("handoff.release".into(), Level::Off);
        raise_for_hands_free(&cfg, &mut o);
        assert_eq!(cfg.level(Action::HandoffRelease, Some(&o)), Level::Off);
        // And auto still needs Armed for all of them.
        for a in HANDS_FREE {
            assert!(a.needs_armed(), "{}", a.key());
            assert_eq!(
                decide(Level::Auto, false, true, false),
                Decision::Ask,
                "disarmed, auto asks"
            );
        }
        assert_eq!(Action::HandoffFanout.default_level(), Level::Confirm);
        assert_eq!(Action::ALL.len(), 13);
    }

    #[test]
    fn keys_round_trip_and_are_unique() {
        for a in Action::ALL {
            assert_eq!(Action::from_key(a.key()), Some(a));
        }
        let mut keys: Vec<_> = Action::ALL.iter().map(|a| a.key()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), Action::ALL.len());
        assert_eq!(Action::from_key("mail.send"), None);
    }

    #[test]
    fn defaults_ask_draft_acks_and_run_no_scouts() {
        let cfg = PolicyCfg::default();
        assert_eq!(cfg.level(Action::IntakeLaunch, None), Level::Confirm);
        assert_eq!(cfg.level(Action::AckPost, None), Level::Draft);
        assert_eq!(cfg.level(Action::ScoutRun, None), Level::Off);
        // Only schedules and local handoffs run on their own out of the box:
        // setting a schedule up was the consent, and a local handoff never
        // leaves your network. They are also the only actions Armed does not
        // gate.
        let unarmed = [Action::ScheduleRun, Action::HandoffLocal];
        for a in unarmed {
            assert_eq!(cfg.level(a, None), Level::Auto);
            assert!(!a.needs_armed());
        }
        assert!(Action::ALL
            .iter()
            .filter(|a| !unarmed.contains(a))
            .all(|a| cfg.level(*a, None) != Level::Auto && a.needs_armed()));
    }

    #[test]
    fn watch_overrides_global_overrides_default() {
        let cfg: PolicyCfg = toml::from_str(
            r#"
            [actions]
            "intake.launch" = "auto"
            "land" = "off"
            "#,
        )
        .unwrap();
        let watch: Overrides = [("intake.launch".to_string(), Level::Draft)].into();
        assert_eq!(cfg.level(Action::IntakeLaunch, Some(&watch)), Level::Draft);
        assert_eq!(cfg.level(Action::IntakeLaunch, None), Level::Auto);
        assert_eq!(cfg.level(Action::Land, Some(&watch)), Level::Off);
        assert_eq!(cfg.level(Action::PrDraft, Some(&watch)), Level::Confirm);
    }

    #[test]
    fn unknown_keys_are_reported() {
        let o: Overrides = [
            ("land".to_string(), Level::Auto),
            ("mail.send".to_string(), Level::Auto),
        ]
        .into();
        assert_eq!(unknown_keys(&o), vec!["mail.send".to_string()]);
    }

    #[test]
    fn auto_runs_only_armed_under_cap_and_accepted() {
        use Decision::*;
        assert_eq!(decide(Level::Auto, true, true, false), Run);
        assert_eq!(decide(Level::Auto, false, true, false), Ask);
        assert_eq!(decide(Level::Auto, true, false, false), Ask);
        assert_eq!(decide(Level::Auto, true, true, true), Ask);
        assert_eq!(decide(Level::Confirm, true, true, false), Ask);
        assert_eq!(decide(Level::Draft, true, true, false), Draft);
        assert_eq!(decide(Level::Off, true, true, false), Skip);
    }

    #[test]
    fn armed_expires() {
        assert!(!is_armed(None, 100));
        assert!(is_armed(Some(200), 100));
        assert!(!is_armed(Some(200), 200));
        assert!(!is_armed(Some(200), 300));
    }

    #[test]
    fn ttl_is_clamped() {
        let mut cfg = PolicyCfg::default();
        assert_eq!(cfg.armed_ttl(), 8 * 3600);
        cfg.armed_ttl_secs = 0;
        assert_eq!(cfg.armed_ttl(), 60);
        cfg.armed_ttl_secs = 7 * 24 * 3600;
        assert_eq!(cfg.armed_ttl(), MAX_ARMED_SECS);
    }

    #[test]
    fn rate_cap_is_a_sliding_hour() {
        let mut log = RateLog::default();
        let a = Action::AckPost;
        assert!(log.under_cap(a, 1000, 2));
        log.record(a, 1000);
        log.record(a, 1500);
        assert!(!log.under_cap(a, 2000, 2));
        // Other actions have their own budget.
        assert!(log.under_cap(Action::Land, 2000, 2));
        // The first run falls out of the window.
        assert!(log.under_cap(a, 1000 + HOUR, 2));
        assert!(!log.under_cap(a, 0, 0));
    }

    #[test]
    fn legacy_knobs_become_auto_with_a_warning() {
        assert_eq!(legacy(Action::IntakeLaunch, "auto_launch", false), None);
        let (level, warning) = legacy(Action::IntakeLaunch, "auto_launch", true).unwrap();
        assert_eq!(level, Level::Auto);
        assert!(warning.contains("intake.launch"));
        assert!(warning.contains("armed"));
    }
}
