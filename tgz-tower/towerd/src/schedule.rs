//! Schedules: start a flight at a time. `schedules.toml` holds them; the
//! Tower view writes it, and hand edits are picked up like any config file.
//!
//! ```toml
//! [[schedule]]
//! id = "morning-prs"
//! name = "Morning PR review"
//! cron = "30 8 * * 1-5"          # or: every = "2h"
//! role = "review"                # or "auto": triage picks
//! title = "PR review {date}"
//! brief = "Review the pull requests opened since {since}."
//! where = "dir"                  # new_folder | dir | worktree
//! path = "~/src/acme"
//! ```
//!
//! Creating or enabling a schedule is the consent: it fires without Armed.
//! What its agent then wants to write outside Tower still follows `[policy]`.

use crate::clock;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tower_core::cron::{self, Cron};
use tower_proto::ScheduleSpec;

pub const FILE: &str = "schedules.toml";

const HEADER: &str = "# Tower schedules: each starts a flight at a time (local time).\n\
# Written by the Routines view (key 8); hand edits are picked up too.\n\
# Fields: id, name, enabled, cron = \"30 8 * * 1-5\" or every = \"2h\", role (id or \"auto\"),\n\
# tier, title, brief ({date} {weekday} {since}), where = new_folder | dir | worktree, path,\n\
# overlap = skip | run, catch_up, pause_after_failures, policy (overrides schedule.run).\n";

#[derive(Deserialize, Serialize, Default)]
struct File {
    #[serde(default)]
    schedule: Vec<ScheduleSpec>,
}

/// When a schedule fires.
#[derive(Clone, Debug, PartialEq)]
pub enum Timing {
    Cron(Cron),
    Every(u64),
}

impl Timing {
    pub fn of(spec: &ScheduleSpec) -> Result<Timing, String> {
        let cron = spec
            .cron
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let every = spec
            .every
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (cron, every) {
            (Some(_), Some(_)) => Err("set either cron or every, not both".into()),
            (Some(c), None) => Cron::parse(c).map(Timing::Cron),
            (None, Some(e)) => cron::parse_every(e).map(Timing::Every),
            (None, None) => Err("say when it runs (cron or every)".into()),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Timing::Cron(c) => c.describe(),
            Timing::Every(s) => cron::describe_every(*s),
        }
    }

    /// The first fire time after `after`. An interval counts from the last
    /// fire (`last`), or from `after` when it never fired.
    pub fn next(&self, after: u64, last: Option<u64>, tz: &TimeZone) -> Option<u64> {
        match self {
            Timing::Cron(c) => clock::next_fire(c, after, tz),
            Timing::Every(s) => {
                let base = last.unwrap_or(after);
                let mut t = base + s;
                if t <= after {
                    // Missed several: the next one on the grid after `after`.
                    t = after + s - (after - base) % s;
                }
                Some(t)
            }
        }
    }
}

/// Read `schedules.toml`; a missing file is no schedules.
pub fn load(dir: &Path) -> Result<Vec<ScheduleSpec>, String> {
    match std::fs::read_to_string(dir.join(FILE)) {
        Ok(t) => {
            let mut list = toml::from_str::<File>(&t)
                .map(|f| f.schedule)
                .map_err(|e| format!("{FILE}: {e}"))?;
            // Hand-written entries may leave the id out, or repeat one.
            for i in 0..list.len() {
                let dup = list[..i].iter().any(|s| s.id == list[i].id);
                if list[i].id.trim().is_empty() || dup {
                    let taken: Vec<&str> = list.iter().map(|s| s.id.as_str()).collect();
                    let id = fresh_id(&list[i].name, &taken);
                    list[i].id = id;
                }
            }
            Ok(list)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(format!("{FILE}: {e}")),
    }
}

/// Write all schedules, replacing the file atomically.
pub fn save(dir: &Path, specs: &[ScheduleSpec]) -> Result<(), String> {
    let body = toml::to_string(&File {
        schedule: specs.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    let path = dir.join(FILE);
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, format!("{HEADER}\n{body}")).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// `Morning PR review!` -> `morning-pr-review`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "schedule".into()
    } else {
        out
    }
}

/// An id for `name` that none of `taken` has.
pub fn fresh_id(name: &str, taken: &[&str]) -> String {
    let base = slug(name);
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(&id.as_str()))
        .unwrap_or(base)
}

/// What is wrong with a spec, before it is saved. `role_ok` says whether a
/// role id exists.
pub fn validate(spec: &ScheduleSpec, role_ok: &dyn Fn(&str) -> bool) -> Result<(), String> {
    if spec.name.trim().is_empty() {
        return Err("Give the schedule a name".into());
    }
    Timing::of(spec)?;
    let role = spec.role.trim();
    if role.is_empty() {
        return Err("Pick a role, or Auto".into());
    }
    if role != "auto" && !role_ok(role) {
        return Err(format!("Unknown role {role}"));
    }
    if spec.brief.trim().is_empty() && spec.title.trim().is_empty() {
        return Err("Say what the agent should do".into());
    }
    match spec.place.as_str() {
        "" | "new_folder" => {}
        "dir" | "worktree" => {
            let p = spec.path.as_deref().map(str::trim).unwrap_or("");
            if p.is_empty() {
                return Err("Say which folder it runs in".into());
            }
        }
        other => return Err(format!("Unknown place {other:?}")),
    }
    if !matches!(spec.overlap.as_str(), "skip" | "run") {
        return Err("overlap is skip or run".into());
    }
    if let Some(t) = spec.tier.as_deref().filter(|t| !t.is_empty()) {
        if tower_core::router::Tier::parse(t).is_none() {
            return Err(format!("Unknown tier {t}"));
        }
    }
    if let Some(p) = spec.policy.as_deref() {
        if !matches!(p, "off" | "confirm" | "auto") {
            return Err("policy is off, confirm or auto".into());
        }
    }
    Ok(())
}

/// Fill `{date}`, `{weekday}` and `{since}` in.
pub fn fill(text: &str, now: u64, since: Option<u64>, tz: &TimeZone) -> String {
    let since = match since {
        Some(t) => {
            let c = clock::civil(t, tz);
            format!("{} {:02}:{:02}", clock::day_key(t, tz), c.hour, c.minute)
        }
        None => "the last run (this is the first)".into(),
    };
    text.replace("{date}", &clock::day_key(now, tz))
        .replace("{weekday}", clock::weekday_name(now, tz))
        .replace("{since}", &since)
}

/// `~/x` -> `$HOME/x`.
pub fn expand(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(cron: &str) -> ScheduleSpec {
        ScheduleSpec {
            id: "nightly".into(),
            name: "Nightly".into(),
            cron: Some(cron.into()),
            role: "crew".into(),
            brief: "check deps".into(),
            ..Default::default()
        }
    }

    #[test]
    fn validation_names_the_problem() {
        let ok = |r: &str| r == "crew";
        assert_eq!(validate(&spec("0 2 * * *"), &ok), Ok(()));
        assert!(validate(&spec("0 25 * * *"), &ok)
            .unwrap_err()
            .contains("hour"));
        let mut s = spec("0 2 * * *");
        s.every = Some("1h".into());
        assert!(validate(&s, &ok).unwrap_err().contains("not both"));
        let mut s = spec("0 2 * * *");
        s.role = "nobody".into();
        assert_eq!(validate(&s, &ok).unwrap_err(), "Unknown role nobody");
        s.role = "auto".into();
        assert_eq!(validate(&s, &ok), Ok(()));
        s.place = "dir".into();
        assert!(validate(&s, &ok).unwrap_err().contains("folder"));
    }

    #[test]
    fn ids_are_slugs_and_unique() {
        assert_eq!(slug("Morning PR review!"), "morning-pr-review");
        assert_eq!(slug("!!!"), "schedule");
        assert_eq!(fresh_id("A b", &["a-b", "a-b-2"]), "a-b-3");
    }

    #[test]
    fn intervals_stay_on_their_grid() {
        let t = Timing::Every(3600);
        assert_eq!(t.next(1000, None, &TimeZone::UTC), Some(4600));
        assert_eq!(t.next(1000, Some(900), &TimeZone::UTC), Some(4500));
        // Down for a few hours: the next slot after now, not a burst.
        assert_eq!(t.next(20_000, Some(900), &TimeZone::UTC), Some(22_500));
    }

    #[test]
    fn placeholders_are_filled() {
        let tz = TimeZone::UTC;
        // 2026-10-24 06:30 UTC, a Saturday.
        let now = 1_792_823_400;
        let s = fill(
            "{weekday} {date} since {since}",
            now,
            Some(now - 86_400),
            &tz,
        );
        assert_eq!(s, "Sat 2026-10-24 since 2026-10-23 06:30");
        assert!(fill("{since}", now, None, &tz).contains("first"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let d = std::env::temp_dir().join(format!("tower-sched-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(load(&d), Ok(vec![]));
        let specs = vec![spec("0 2 * * *")];
        save(&d, &specs).unwrap();
        assert_eq!(load(&d), Ok(specs));
        let text = std::fs::read_to_string(d.join(FILE)).unwrap();
        assert!(text.starts_with("# Tower schedules"));
        // A hand-written entry without an id gets one; a repeated id too.
        std::fs::write(
            d.join(FILE),
            "[[schedule]]\nname = \"Hand made\"\n[[schedule]]\nid = \"hand-made\"\nname = \"Hand made\"\n",
        )
        .unwrap();
        let ids: Vec<String> = load(&d).unwrap().into_iter().map(|s| s.id).collect();
        // An explicit id wins; the one without gets the next free one.
        assert_eq!(ids, ["hand-made-2", "hand-made"]);
    }
}
