//! Response-time clocks.
//!
//! Jira Service Management keeps its SLAs in custom fields whose ids differ
//! per site (`customfield_10182` on one, something else on the next), so
//! they are found by shape, never by id or name: an object with `name` and
//! `completedCycles` (and, while a cycle runs, `ongoingCycle`). The tracker
//! has already applied business hours to `breachTime`, so a wall-clock
//! countdown to it is correct. Other trackers get Tower's own clock from a
//! watch's `respond_within_hours`.
//!
//! Every SLA cycle is a separate thing to answer: "first response" runs once
//! per ticket, but a "reply to a customer comment" SLA starts a new cycle on
//! each comment. [`cycle_key`] names one cycle, so Tower acknowledges each at
//! most once.

use serde_json::Value;
use tower_proto::SlaClock;

/// Id of Tower's own clock.
pub const RESPOND: &str = "respond";

/// Does this field value look like a JSM SLA?
pub fn is_sla_value(v: &Value) -> bool {
    v.get("name").is_some_and(Value::is_string)
        && v.get("completedCycles").is_some_and(Value::is_array)
}

/// Ids of every field in `fields` that holds an SLA.
pub fn sla_field_ids(fields: &Value) -> Vec<String> {
    let mut ids: Vec<String> = fields
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| is_sla_value(v))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn millis_secs(v: &Value, path: &str) -> u64 {
    v.pointer(path).and_then(Value::as_u64).unwrap_or(0) / 1000
}

/// The SLA clocks among an issue's fields, sorted by name.
pub fn clocks_from_fields(fields: &Value) -> Vec<SlaClock> {
    let mut out: Vec<SlaClock> = fields
        .as_object()
        .map(|o| {
            o.values()
                .filter(|v| is_sla_value(v))
                .map(clock_from_value)
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn clock_from_value(v: &Value) -> SlaClock {
    let id = match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    };
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("SLA")
        .to_string();
    match v.get("ongoingCycle").filter(|c| c.is_object()) {
        Some(c) => SlaClock {
            id,
            name,
            source: "jsm".into(),
            started_unix: millis_secs(c, "/startTime/epochMillis"),
            due_unix: millis_secs(c, "/breachTime/epochMillis"),
            goal_secs: millis_secs(c, "/goalDuration/millis"),
            breached: c.get("breached").and_then(Value::as_bool).unwrap_or(false),
            paused: c.get("paused").and_then(Value::as_bool).unwrap_or(false),
            stopped: false,
        },
        None => SlaClock {
            id,
            name,
            source: "jsm".into(),
            stopped: true,
            breached: v
                .get("completedCycles")
                .and_then(Value::as_array)
                .and_then(|a| a.last())
                .and_then(|c| c.get("breached"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ..Default::default()
        },
    }
}

/// Tower's own clock: answer within `hours` of `created_unix`.
pub fn respond_clock(created_unix: u64, hours: u64, answered: bool, now: u64) -> SlaClock {
    let started = if created_unix == 0 { now } else { created_unix };
    let goal = hours.saturating_mul(3600);
    let due = started.saturating_add(goal);
    SlaClock {
        id: RESPOND.into(),
        name: format!("Respond within {hours} h"),
        source: "tower".into(),
        started_unix: started,
        due_unix: due,
        goal_secs: goal,
        breached: !answered && now >= due,
        paused: false,
        stopped: answered,
    }
}

/// Names one cycle of one clock on one flight.
pub fn cycle_key(flight_id: &str, c: &SlaClock) -> String {
    format!("{flight_id}:{}:{}", c.id, c.started_unix)
}

/// How close a running clock is to breaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Calm,
    /// Half its time used.
    Half,
    /// Four fifths used.
    Late,
    Breached,
}

impl Urgency {
    pub fn label(self) -> &'static str {
        match self {
            Urgency::Calm => "on time",
            Urgency::Half => "half its time used",
            Urgency::Late => "80% of its time used",
            Urgency::Breached => "breached",
        }
    }
}

/// Urgency of `c` at `now`. Paused and stopped clocks are calm (a breach
/// already recorded by the tracker still counts as breached).
pub fn urgency(c: &SlaClock, now: u64) -> Urgency {
    if c.breached {
        return Urgency::Breached;
    }
    if c.stopped || c.paused || c.due_unix == 0 {
        return Urgency::Calm;
    }
    if now >= c.due_unix {
        return Urgency::Breached;
    }
    let span = c.due_unix.saturating_sub(c.started_unix);
    if span == 0 {
        return Urgency::Calm;
    }
    // Wall-clock share: an approximation where business hours apply, which
    // errs early (nights count as used), never late.
    let used = now.saturating_sub(c.started_unix) as f64 / span as f64;
    if used >= 0.8 {
        Urgency::Late
    } else if used >= 0.5 {
        Urgency::Half
    } else {
        Urgency::Calm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape read from a real JSM site (values made up).
    fn running() -> Value {
        json!({
            "id": "10", "name": "Reply to customer comment",
            "completedCycles": [],
            "ongoingCycle": {
                "startTime": {"epochMillis": 1_000_000_000_000_u64},
                "breachTime": {"epochMillis": 1_000_028_800_000_u64},
                "breached": false, "paused": false, "withinCalendarHours": false,
                "goalDuration": {"millis": 28_800_000, "friendly": "8h"},
                "elapsedTime": {"millis": 0}, "remainingTime": {"millis": 28_800_000}
            },
            "slaDisplayFormat": "NEW_SLA_FORMAT"
        })
    }

    #[test]
    fn sla_fields_are_found_by_shape_whatever_their_id() {
        let fields = json!({
            "summary": "x",
            "customfield_10182": {"id": "9", "name": "First response", "completedCycles": []},
            "customfield_99999": running(),
            "customfield_10010": {"requestType": {"name": "Help"}},
            "labels": ["a"],
            "status": {"name": "Open"}
        });
        assert_eq!(
            sla_field_ids(&fields),
            vec![
                "customfield_10182".to_string(),
                "customfield_99999".to_string()
            ]
        );
        assert!(sla_field_ids(&json!("nope")).is_empty());
    }

    #[test]
    fn a_running_cycle_becomes_a_clock() {
        let fields = json!({"customfield_1": running()});
        let c = &clocks_from_fields(&fields)[0];
        assert_eq!(c.id, "10");
        assert_eq!(c.source, "jsm");
        assert_eq!(c.started_unix, 1_000_000_000);
        assert_eq!(c.due_unix, 1_000_028_800);
        assert_eq!(c.goal_secs, 8 * 3600);
        assert!(!c.stopped && !c.breached && !c.paused);
    }

    #[test]
    fn a_clock_without_a_running_cycle_is_stopped() {
        let fields = json!({
            "a": {"id": "9", "name": "First response", "completedCycles": []},
            "b": {"id": "11", "name": "Resolution", "completedCycles": [{"breached": true}]}
        });
        let clocks = clocks_from_fields(&fields);
        assert_eq!(clocks.len(), 2);
        assert!(clocks.iter().all(|c| c.stopped));
        let res = clocks.iter().find(|c| c.id == "11").unwrap();
        assert!(res.breached, "a breached past cycle stays visible");
        assert_eq!(urgency(res, 0), Urgency::Breached);
        let first = clocks.iter().find(|c| c.id == "9").unwrap();
        assert_eq!(urgency(first, u64::MAX), Urgency::Calm);
    }

    #[test]
    fn urgency_follows_the_share_of_time_used() {
        let c = clocks_from_fields(&json!({"x": running()})).remove(0);
        let start = c.started_unix;
        let span = c.due_unix - start;
        assert_eq!(urgency(&c, start), Urgency::Calm);
        assert_eq!(urgency(&c, start + span / 2), Urgency::Half);
        assert_eq!(urgency(&c, start + span * 4 / 5), Urgency::Late);
        assert_eq!(urgency(&c, c.due_unix), Urgency::Breached);
        let mut paused = c.clone();
        paused.paused = true;
        assert_eq!(urgency(&paused, c.due_unix - 1), Urgency::Calm);
        assert!(Urgency::Late > Urgency::Half && Urgency::Breached > Urgency::Late);
    }

    #[test]
    fn towers_own_clock_runs_from_creation() {
        let c = respond_clock(1000, 24, false, 2000);
        assert_eq!(c.id, RESPOND);
        assert_eq!(c.due_unix, 1000 + 24 * 3600);
        assert!(!c.stopped && !c.breached);
        assert!(respond_clock(1000, 24, false, 1000 + 24 * 3600).breached);
        let answered = respond_clock(1000, 24, true, 10_000_000);
        assert!(answered.stopped && !answered.breached);
        // Unknown creation time: start now.
        assert_eq!(respond_clock(0, 1, false, 500).started_unix, 500);
    }

    #[test]
    fn every_cycle_has_its_own_key() {
        let mut c = clocks_from_fields(&json!({"x": running()})).remove(0);
        let first = cycle_key("f-1", &c);
        c.started_unix += 60;
        assert_ne!(
            first,
            cycle_key("f-1", &c),
            "a new customer comment, a new cycle"
        );
        assert_ne!(first, cycle_key("f-2", &c));
    }
}
