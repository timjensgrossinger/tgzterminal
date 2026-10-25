//! Local wall-clock time for schedules: unix seconds to and from the civil
//! minutes `tower_core::cron` works in.
//!
//! DST: a wall time that does not exist (the spring-forward gap) runs at the
//! first instant after the gap; one that happens twice (fall-back) runs once,
//! at the earlier instant. Both are `jiff`'s "compatible" rule.

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use tower_core::cron::{CivilMinute, Cron};

/// The machine's time zone; UTC when it cannot be found.
pub fn local_tz() -> TimeZone {
    TimeZone::try_system().unwrap_or(TimeZone::UTC)
}

/// The civil minute `unix` falls in.
pub fn civil(unix: u64, tz: &TimeZone) -> CivilMinute {
    let ts = Timestamp::from_second(unix as i64).unwrap_or(Timestamp::UNIX_EPOCH);
    let dt = ts.to_zoned(tz.clone()).datetime();
    CivilMinute::new(
        dt.year() as i32,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
    )
}

/// The first instant of civil minute `c`.
pub fn unix_of(c: &CivilMinute, tz: &TimeZone) -> Option<u64> {
    let dt = DateTime::new(
        c.year as i16,
        c.month as i8,
        c.day as i8,
        c.hour as i8,
        c.minute as i8,
        0,
        0,
    )
    .ok()?;
    let z = dt.to_zoned(tz.clone()).ok()?;
    u64::try_from(z.timestamp().as_second()).ok()
}

/// When `cron` next fires strictly after `after_unix`.
pub fn next_fire(cron: &Cron, after_unix: u64, tz: &TimeZone) -> Option<u64> {
    let mut from = civil(after_unix, tz);
    // A fall-back hour repeats civil minutes: walk past those already gone
    // (at most an hour of them) rather than returning one in the past.
    for _ in 0..61 {
        let next = cron.next_after(&from)?;
        let at = unix_of(&next, tz)?;
        if at > after_unix {
            return Some(at);
        }
        from = next;
    }
    None
}

/// `2026-10-24`.
pub fn day_key(unix: u64, tz: &TimeZone) -> String {
    let c = civil(unix, tz);
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

/// `Mon`, `Tue`, ...
pub fn weekday_name(unix: u64, tz: &TimeZone) -> &'static str {
    const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    NAMES[civil(unix, tz).weekday() as usize % 7]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vienna() -> TimeZone {
        TimeZone::get("Europe/Vienna").unwrap()
    }

    fn at(s: &str) -> u64 {
        s.parse::<Timestamp>().unwrap().as_second() as u64
    }

    #[test]
    fn civil_and_back_round_trip() {
        let tz = vienna();
        let t = at("2026-10-24T05:30:00Z"); // 07:30 CEST
        let c = civil(t, &tz);
        assert_eq!((c.hour, c.minute), (7, 30));
        assert_eq!(unix_of(&c, &tz), Some(t));
        assert_eq!(day_key(t, &tz), "2026-10-24");
        assert_eq!(weekday_name(t, &tz), "Sat");
    }

    #[test]
    fn next_fire_is_in_local_time() {
        let tz = vienna();
        let cron = Cron::parse("0 7 * * 1-5").unwrap();
        // Saturday morning -> Monday 07:00 CET (after the switch on Oct 25).
        let next = next_fire(&cron, at("2026-10-24T05:30:00Z"), &tz).unwrap();
        assert_eq!(next, at("2026-10-26T06:00:00Z"));
    }

    #[test]
    fn a_time_in_the_spring_gap_runs_after_it() {
        let tz = vienna();
        // 2026-03-29 02:30 does not exist in Vienna.
        let cron = Cron::parse("30 2 * * *").unwrap();
        let next = next_fire(&cron, at("2026-03-28T23:00:00Z"), &tz).unwrap();
        assert_eq!(next, at("2026-03-29T01:30:00Z"));
    }

    #[test]
    fn a_repeated_fall_back_time_runs_once() {
        let tz = vienna();
        // 02:30 happens twice on 2026-10-25: first at 00:30Z, then at 01:30Z.
        let cron = Cron::parse("30 2 * * *").unwrap();
        let first = next_fire(&cron, at("2026-10-24T22:00:00Z"), &tz).unwrap();
        assert_eq!(first, at("2026-10-25T00:30:00Z"));
        let second = next_fire(&cron, first, &tz).unwrap();
        assert_eq!(second, at("2026-10-26T01:30:00Z"));
    }

    #[test]
    fn a_minute_step_finds_a_time_after_the_repeated_hour() {
        let tz = vienna();
        // 02:20 in the second pass of the fall-back hour (01:20Z).
        let cron = Cron::parse("*/5 * * * *").unwrap();
        let after = at("2026-10-25T01:20:00Z");
        let next = next_fire(&cron, after, &tz).unwrap();
        assert!(next > after);
        assert!(next <= at("2026-10-25T02:00:00Z"));
    }
}
