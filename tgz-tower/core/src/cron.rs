//! Cron expressions and plain intervals for scheduled jobs. Pure helpers,
//! no IO, no dependencies. All time math is *civil local time*: the caller
//! converts from and to real time zones.

/// One minute on the civil calendar (no seconds, no time zone).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CivilMinute {
    pub year: i32,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

impl CivilMinute {
    pub fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Self {
        CivilMinute {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    /// 0 = Sunday .. 6 = Saturday.
    pub fn weekday(&self) -> u8 {
        weekday_of(self.year, self.month, self.day)
    }

    /// The next minute (rolls hours, days, months, years).
    pub fn plus_minute(&self) -> CivilMinute {
        if self.minute < 59 {
            return CivilMinute {
                minute: self.minute + 1,
                ..*self
            };
        }
        self.next_hour()
    }

    /// The start of the next hour.
    fn next_hour(&self) -> CivilMinute {
        if self.hour < 23 {
            return CivilMinute {
                hour: self.hour + 1,
                minute: 0,
                ..*self
            };
        }
        self.next_day()
    }

    /// Midnight of the next day.
    fn next_day(&self) -> CivilMinute {
        if self.day < days_in_month(self.year, self.month) {
            return CivilMinute::new(self.year, self.month, self.day + 1, 0, 0);
        }
        self.next_month()
    }

    /// Midnight on the 1st of the next month.
    fn next_month(&self) -> CivilMinute {
        if self.month < 12 {
            CivilMinute::new(self.year, self.month + 1, 1, 0, 0)
        } else {
            CivilMinute::new(self.year + 1, 1, 1, 0, 0)
        }
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` (1-12) of `year`; 0 for an invalid month.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01 (proleptic Gregorian).
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn weekday_of(year: i32, month: u8, day: u8) -> u8 {
    // 1970-01-01 was a Thursday (4).
    let d = days_from_civil(year, month, day);
    ((d + 4).rem_euclid(7)) as u8
}

/// A parsed cron expression.
///
/// Day rule (Vixie cron): a field counts as unrestricted only when its text
/// starts with `*` (so `*/2` is unrestricted too). If both day-of-month and
/// day-of-week are restricted, a day matches when EITHER matches; if one is
/// unrestricted, only the other applies.
#[derive(Clone, Debug)]
pub struct Cron {
    minute: u64,
    hour: u32,
    dom: u32,
    month: u16,
    dow: u8,
    dom_star: bool,
    dow_star: bool,
    /// The (expanded, whitespace-normalized) expression.
    raw: String,
}

/// Equality compares the schedule, not the spelling (`7` equals `0`).
impl PartialEq for Cron {
    fn eq(&self, other: &Cron) -> bool {
        self.minute == other.minute
            && self.hour == other.hour
            && self.dom == other.dom
            && self.month == other.month
            && self.dow == other.dow
            && self.dom_star == other.dom_star
            && self.dow_star == other.dow_star
    }
}

impl Eq for Cron {}

const MONTH_NAMES: &[&str] = &[
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAY_NAMES: &[&str] = &["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const DAY_SHORT: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const DAY_PLURAL: &[&str] = &[
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];
const MONTH_SHORT: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct FieldSpec {
    name: &'static str,
    min: u32,
    max: u32,
    /// Highest value a bare `*` covers.
    star_hi: u32,
    names: &'static [&'static str],
    /// Value of the first name.
    names_base: u32,
}

const MINUTE: FieldSpec = FieldSpec {
    name: "minute",
    min: 0,
    max: 59,
    star_hi: 59,
    names: &[],
    names_base: 0,
};
const HOUR: FieldSpec = FieldSpec {
    name: "hour",
    min: 0,
    max: 23,
    star_hi: 23,
    names: &[],
    names_base: 0,
};
const DOM: FieldSpec = FieldSpec {
    name: "day",
    min: 1,
    max: 31,
    star_hi: 31,
    names: &[],
    names_base: 0,
};
const MONTH: FieldSpec = FieldSpec {
    name: "month",
    min: 1,
    max: 12,
    star_hi: 12,
    names: MONTH_NAMES,
    names_base: 1,
};
const DOW: FieldSpec = FieldSpec {
    name: "weekday",
    min: 0,
    max: 7,
    star_hi: 6,
    names: DAY_NAMES,
    names_base: 0,
};

fn parse_value(tok: &str, spec: &FieldSpec) -> Result<u32, String> {
    if !tok.is_empty() && tok.bytes().all(|b| b.is_ascii_digit()) {
        return tok
            .parse::<u32>()
            .map_err(|_| format!("{}: bad value '{}'", spec.name, tok));
    }
    let lower = tok.to_ascii_lowercase();
    if let Some(i) = spec.names.iter().position(|n| *n == lower) {
        return Ok(i as u32 + spec.names_base);
    }
    Err(format!("{}: bad value '{}'", spec.name, tok))
}

fn check_range(v: u32, spec: &FieldSpec) -> Result<(), String> {
    if v < spec.min || v > spec.max {
        return Err(format!(
            "{}: {} is out of range {}-{}",
            spec.name, v, spec.min, spec.max
        ));
    }
    Ok(())
}

/// Parse one field into a bitset over its values (bit `v` = value `v`).
fn parse_field(text: &str, spec: &FieldSpec) -> Result<u64, String> {
    let mut bits = 0u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(format!("{}: bad step '{}'", spec.name, s));
                }
                let n = s
                    .parse::<u32>()
                    .map_err(|_| format!("{}: bad step '{}'", spec.name, s))?;
                if n == 0 {
                    return Err(format!("{}: step 0", spec.name));
                }
                (r, n)
            }
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (spec.min, spec.star_hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let lo = parse_value(a, spec)?;
            let hi = parse_value(b, spec)?;
            check_range(lo, spec)?;
            check_range(hi, spec)?;
            if lo > hi {
                return Err(format!("{}: range {}-{} is backwards", spec.name, lo, hi));
            }
            (lo, hi)
        } else {
            let a = parse_value(range, spec)?;
            check_range(a, spec)?;
            if item.contains('/') {
                (a, spec.star_hi.max(a))
            } else {
                (a, a)
            }
        };
        let mut v = lo;
        while v <= hi {
            bits |= 1u64 << v;
            v += step;
        }
    }
    Ok(bits)
}

fn expand_macro(m: &str) -> Result<&'static str, String> {
    match m.to_ascii_lowercase().as_str() {
        "@hourly" => Ok("0 * * * *"),
        "@daily" | "@midnight" => Ok("0 0 * * *"),
        "@weekly" => Ok("0 0 * * 0"),
        "@monthly" => Ok("0 0 1 * *"),
        "@yearly" | "@annually" => Ok("0 0 1 1 *"),
        "@weekdays" => Ok("0 0 * * 1-5"),
        "@reboot" => Err("@reboot has no time of day".to_string()),
        _ => Err(format!("unknown macro '{}'", m)),
    }
}

fn bits_to_vec(bits: u64, lo: u32, hi: u32) -> Vec<u32> {
    (lo..=hi).filter(|v| bits & (1u64 << v) != 0).collect()
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{}{}", n, suffix)
}

impl Cron {
    /// 5 fields `min hour dom month dow`, or a macro: `@hourly`, `@daily`,
    /// `@midnight`, `@weekly`, `@monthly`, `@yearly`, `@annually`,
    /// `@weekdays` (= `0 0 * * 1-5`).
    ///
    /// Fields: `*`, numbers, ranges `a-b`, lists `a,b`, steps `*/n`, `a-b/n`
    /// and `a/n` (a to the maximum, step n), month names JAN-DEC, day names
    /// SUN-SAT (case-insensitive); weekday 7 is Sunday. `@reboot` is an error.
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let trimmed = expr.trim();
        let expanded;
        let text = if trimmed.starts_with('@') {
            expanded = expand_macro(trimmed)?;
            expanded
        } else {
            trimmed
        };
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!("expected 5 fields, got {}", fields.len()));
        }
        let minute = parse_field(fields[0], &MINUTE)?;
        let hour = parse_field(fields[1], &HOUR)? as u32;
        let dom = parse_field(fields[2], &DOM)? as u32;
        let month = parse_field(fields[3], &MONTH)? as u16;
        let mut dow = parse_field(fields[4], &DOW)?;
        if dow & (1 << 7) != 0 {
            dow = (dow & !(1 << 7)) | 1;
        }
        Ok(Cron {
            minute,
            hour,
            dom,
            month,
            dow: dow as u8,
            dom_star: fields[2].starts_with('*'),
            dow_star: fields[4].starts_with('*'),
            raw: fields.join(" "),
        })
    }

    fn day_matches(&self, year: i32, month: u8, day: u8) -> bool {
        let dom_ok = self.dom & (1 << day) != 0;
        let dow_ok = self.dow & (1 << weekday_of(year, month, day)) != 0;
        match (self.dom_star, self.dow_star) {
            (true, true) => true,
            (true, false) => dow_ok,
            (false, true) => dom_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }

    pub fn matches(&self, t: &CivilMinute) -> bool {
        t.month >= 1
            && t.month <= 12
            && t.day >= 1
            && t.day <= 31
            && t.hour < 24
            && t.minute < 60
            && self.month & (1 << t.month) != 0
            && self.day_matches(t.year, t.month, t.day)
            && self.hour & (1 << t.hour) != 0
            && self.minute & (1u64 << t.minute) != 0
    }

    /// The first matching minute strictly after `t`. Skips whole months,
    /// days and hours that cannot match. Gives up (None) after searching
    /// 5 years ahead (e.g. `0 0 31 2 *`).
    pub fn next_after(&self, t: &CivilMinute) -> Option<CivilMinute> {
        let limit = t.year + 5;
        let mut cur = t.plus_minute();
        while cur.year <= limit {
            if self.month & (1 << cur.month) == 0 {
                cur = cur.next_month();
            } else if !self.day_matches(cur.year, cur.month, cur.day) {
                cur = cur.next_day();
            } else if self.hour & (1 << cur.hour) == 0 {
                cur = cur.next_hour();
            } else if self.minute & (1u64 << cur.minute) == 0 {
                let rest = self.minute >> cur.minute;
                // `rest` is non-zero below if a later minute exists.
                let later = (self.minute >> (cur.minute + 1)) << (cur.minute + 1);
                if rest == 0 || later == 0 {
                    cur = cur.next_hour();
                } else {
                    cur.minute = later.trailing_zeros() as u8;
                }
            } else {
                return Some(cur);
            }
        }
        None
    }

    /// Short English for the UI; falls back to the expression itself.
    pub fn describe(&self) -> String {
        self.describe_nice().unwrap_or_else(|| self.raw.clone())
    }

    fn describe_nice(&self) -> Option<String> {
        let minutes = bits_to_vec(self.minute, 0, 59);
        let hours = bits_to_vec(u64::from(self.hour), 0, 23);
        let months_all = self.month == 0x1ffe;
        let days_free = self.dom_star && self.dow_star;
        if months_all && days_free && hours.len() == 24 {
            if minutes.len() == 60 {
                return Some("every minute".to_string());
            }
            if minutes.len() == 1 {
                return Some(format!("hourly at :{:02}", minutes[0]));
            }
            for n in 2..=30u32 {
                if 60 % n == 0
                    && minutes.iter().all(|m| m % n == 0)
                    && minutes.len() as u32 == 60 / n
                {
                    return Some(format!("every {} minutes", n));
                }
            }
            return None;
        }
        if minutes.len() != 1 || hours.len() != 1 {
            return None;
        }
        let time = format!("{:02}:{:02}", hours[0], minutes[0]);
        if months_all {
            if days_free {
                return Some(format!("daily {}", time));
            }
            if self.dom_star {
                let days = bits_to_vec(u64::from(self.dow), 0, 6);
                let label = if days == [1, 2, 3, 4, 5] {
                    "weekdays".to_string()
                } else if days == [0, 6] {
                    "weekends".to_string()
                } else if days.len() == 1 {
                    DAY_PLURAL[days[0] as usize].to_string()
                } else {
                    days.iter()
                        .map(|d| DAY_SHORT[*d as usize])
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                return Some(format!("{} {}", label, time));
            }
            if self.dow_star {
                let days = bits_to_vec(u64::from(self.dom), 1, 31);
                if days.len() == 1 {
                    return Some(format!("{} of the month {}", ordinal(days[0]), time));
                }
            }
            return None;
        }
        if self.dow_star {
            let months = bits_to_vec(u64::from(self.month), 1, 12);
            let days = bits_to_vec(u64::from(self.dom), 1, 31);
            if months.len() == 1 && days.len() == 1 && !self.dom_star {
                return Some(format!(
                    "{} {} {}",
                    MONTH_SHORT[months[0] as usize - 1],
                    days[0],
                    time
                ));
            }
        }
        None
    }
}

/// launchd `StartCalendarInterval` semantics: a missing key means "every".
/// Weekday 0 and 7 are both Sunday. Values out of range are an error.
pub fn from_calendar(
    minute: Option<u8>,
    hour: Option<u8>,
    day: Option<u8>,
    weekday: Option<u8>,
    month: Option<u8>,
) -> Result<Cron, String> {
    let f = |v: Option<u8>| v.map_or("*".to_string(), |n| n.to_string());
    Cron::parse(&format!(
        "{} {} {} {} {}",
        f(minute),
        f(hour),
        f(day),
        f(month),
        f(weekday)
    ))
}

const MAX_EVERY_SECS: u64 = 366 * 86_400;

/// "30m", "2h", "1d", "90s", "1h30m"; a bare number is minutes. Minimum is
/// 60 seconds, maximum 366 days.
pub fn parse_every(s: &str) -> Result<u64, String> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return Err("empty interval".to_string());
    }
    let bad = || format!("bad interval '{}'", s.trim());
    let mut total: u64 = 0;
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let n = t.parse::<u64>().map_err(|_| bad())?;
        total = n.saturating_mul(60);
    } else {
        let mut num: Option<u64> = None;
        for c in t.chars() {
            if let Some(d) = c.to_digit(10) {
                num = Some(
                    num.unwrap_or(0)
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u64::from(d)))
                        .ok_or_else(bad)?,
                );
            } else if c.is_whitespace() {
                continue;
            } else {
                let unit = match c {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    'd' => 86_400,
                    _ => return Err(bad()),
                };
                let n = num.take().ok_or_else(bad)?;
                total = total.saturating_add(n.saturating_mul(unit));
            }
        }
        if num.is_some() {
            return Err(bad());
        }
    }
    if total < 60 {
        return Err("at least 1 minute".to_string());
    }
    if total > MAX_EVERY_SECS {
        return Err("at most 366 days".to_string());
    }
    Ok(total)
}

/// "every 30m", "every 2h", "every 1h 30m", "every day", "every 2 days".
pub fn describe_every(secs: u64) -> String {
    if secs > 0 && secs % 86_400 == 0 {
        let d = secs / 86_400;
        return if d == 1 {
            "every day".to_string()
        } else {
            format!("every {} days", d)
        };
    }
    let mut parts = Vec::new();
    let units = [(86_400, "d"), (3600, "h"), (60, "m"), (1, "s")];
    let mut rest = secs;
    for (size, label) in units.iter() {
        let n = rest / size;
        rest %= size;
        if n > 0 {
            parts.push(format!("{}{}", n, label));
        }
    }
    if parts.is_empty() {
        parts.push("0s".to_string());
    }
    format!("every {}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> CivilMinute {
        CivilMinute::new(y, mo, d, h, mi)
    }

    #[test]
    fn weekday_known_dates() {
        assert_eq!(t(2026, 10, 24, 0, 0).weekday(), 6);
        assert_eq!(t(1970, 1, 1, 0, 0).weekday(), 4);
        assert_eq!(t(2000, 2, 29, 0, 0).weekday(), 2);
        assert_eq!(t(2026, 10, 25, 0, 0).weekday(), 0);
    }

    #[test]
    fn month_lengths() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 13), 0);
    }

    #[test]
    fn plus_minute_rolls() {
        assert_eq!(t(2026, 1, 1, 0, 0).plus_minute(), t(2026, 1, 1, 0, 1));
        assert_eq!(t(2026, 1, 1, 0, 59).plus_minute(), t(2026, 1, 1, 1, 0));
        assert_eq!(t(2026, 1, 1, 23, 59).plus_minute(), t(2026, 1, 2, 0, 0));
        assert_eq!(t(2026, 1, 31, 23, 59).plus_minute(), t(2026, 2, 1, 0, 0));
        assert_eq!(t(2026, 12, 31, 23, 59).plus_minute(), t(2027, 1, 1, 0, 0));
        assert_eq!(t(2028, 2, 28, 23, 59).plus_minute(), t(2028, 2, 29, 0, 0));
    }

    #[test]
    fn parse_field_forms() {
        let c = Cron::parse("*/15 1,2 3-5 jan,Mar-MAY sun").unwrap();
        assert_eq!(bits_to_vec(c.minute, 0, 59), vec![0, 15, 30, 45]);
        assert_eq!(bits_to_vec(u64::from(c.hour), 0, 23), vec![1, 2]);
        assert_eq!(bits_to_vec(u64::from(c.dom), 1, 31), vec![3, 4, 5]);
        assert_eq!(bits_to_vec(u64::from(c.month), 1, 12), vec![1, 3, 4, 5]);
        assert_eq!(bits_to_vec(u64::from(c.dow), 0, 6), vec![0]);
        let c = Cron::parse("5/20 * * * MON-fri").unwrap();
        assert_eq!(bits_to_vec(c.minute, 0, 59), vec![5, 25, 45]);
        assert_eq!(bits_to_vec(u64::from(c.dow), 0, 6), vec![1, 2, 3, 4, 5]);
        let c = Cron::parse("10-50/20 * * * *").unwrap();
        assert_eq!(bits_to_vec(c.minute, 0, 59), vec![10, 30, 50]);
    }

    #[test]
    fn macros() {
        assert_eq!(
            Cron::parse("@hourly").unwrap(),
            Cron::parse("0 * * * *").unwrap()
        );
        assert_eq!(
            Cron::parse("@daily").unwrap(),
            Cron::parse("0 0 * * *").unwrap()
        );
        assert_eq!(
            Cron::parse("@midnight").unwrap(),
            Cron::parse("@daily").unwrap()
        );
        assert_eq!(
            Cron::parse("@weekly").unwrap(),
            Cron::parse("0 0 * * 0").unwrap()
        );
        assert_eq!(
            Cron::parse("@monthly").unwrap(),
            Cron::parse("0 0 1 * *").unwrap()
        );
        assert_eq!(
            Cron::parse("@yearly").unwrap(),
            Cron::parse("0 0 1 1 *").unwrap()
        );
        assert_eq!(
            Cron::parse("@annually").unwrap(),
            Cron::parse("@yearly").unwrap()
        );
        assert_eq!(
            Cron::parse("@weekdays").unwrap(),
            Cron::parse("0 0 * * 1-5").unwrap()
        );
        assert_eq!(
            Cron::parse("@reboot").unwrap_err(),
            "@reboot has no time of day"
        );
        assert!(Cron::parse("@sometimes").is_err());
    }

    #[test]
    fn parse_errors() {
        assert_eq!(
            Cron::parse("0 25 * * *").unwrap_err(),
            "hour: 25 is out of range 0-23"
        );
        assert_eq!(
            Cron::parse("* * * *").unwrap_err(),
            "expected 5 fields, got 4"
        );
        assert_eq!(Cron::parse("").unwrap_err(), "expected 5 fields, got 0");
        assert_eq!(Cron::parse("*/0 * * * *").unwrap_err(), "minute: step 0");
        assert_eq!(
            Cron::parse("60 * * * *").unwrap_err(),
            "minute: 60 is out of range 0-59"
        );
        assert_eq!(
            Cron::parse("* * 0 * *").unwrap_err(),
            "day: 0 is out of range 1-31"
        );
        assert_eq!(
            Cron::parse("* * * 13 *").unwrap_err(),
            "month: 13 is out of range 1-12"
        );
        assert_eq!(
            Cron::parse("* * * * 8").unwrap_err(),
            "weekday: 8 is out of range 0-7"
        );
        assert_eq!(
            Cron::parse("5-1 * * * *").unwrap_err(),
            "minute: range 5-1 is backwards"
        );
        assert_eq!(
            Cron::parse("x * * * *").unwrap_err(),
            "minute: bad value 'x'"
        );
        assert_eq!(
            Cron::parse("*/x * * * *").unwrap_err(),
            "minute: bad step 'x'"
        );
        assert!(Cron::parse("1,,2 * * * *").is_err());
        assert!(Cron::parse("* * * foo *").is_err());
    }

    #[test]
    fn matching() {
        let c = Cron::parse("30 7 * * *").unwrap();
        assert!(c.matches(&t(2026, 10, 24, 7, 30)));
        assert!(!c.matches(&t(2026, 10, 24, 7, 31)));
        assert!(!c.matches(&t(2026, 10, 24, 8, 30)));
    }

    #[test]
    fn dom_dow_or_rule() {
        // 13th of the month OR any Friday. 2026-11-13 is a Friday, so use
        // 2026-10-13 (Tuesday) and 2026-10-16 (Friday).
        let c = Cron::parse("0 0 13 * 5").unwrap();
        assert!(c.matches(&t(2026, 10, 13, 0, 0)));
        assert!(c.matches(&t(2026, 10, 16, 0, 0)));
        assert!(!c.matches(&t(2026, 10, 14, 0, 0)));
        // One side `*`: only the other applies.
        let c = Cron::parse("0 0 13 * *").unwrap();
        assert!(!c.matches(&t(2026, 10, 16, 0, 0)));
        let c = Cron::parse("0 0 * * 5").unwrap();
        assert!(!c.matches(&t(2026, 10, 13, 0, 0)));
        // `*/2` still counts as unrestricted.
        let c = Cron::parse("0 0 */2 * 5").unwrap();
        assert!(!c.matches(&t(2026, 10, 3, 0, 0)) || t(2026, 10, 3, 0, 0).weekday() == 5);
        assert!(c.matches(&t(2026, 10, 16, 0, 0)));
    }

    #[test]
    fn dow_seven_is_sunday() {
        assert_eq!(
            Cron::parse("0 0 * * 7").unwrap(),
            Cron::parse("0 0 * * 0").unwrap()
        );
        let c = Cron::parse("0 0 * * 5-7").unwrap();
        assert!(c.matches(&t(2026, 10, 25, 0, 0)));
        assert!(c.matches(&t(2026, 10, 24, 0, 0)));
        assert!(!c.matches(&t(2026, 10, 26, 0, 0)));
    }

    #[test]
    fn next_rollovers() {
        let c = Cron::parse("* * * * *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 12, 31, 23, 59)),
            Some(t(2027, 1, 1, 0, 0))
        );
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 10, 0)),
            Some(t(2026, 10, 24, 10, 15))
        );
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 10, 46)),
            Some(t(2026, 10, 24, 11, 0))
        );
        let c = Cron::parse("10-50/20 * * * *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 10, 10)),
            Some(t(2026, 10, 24, 10, 30))
        );
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 10, 50)),
            Some(t(2026, 10, 24, 11, 10))
        );
        let c = Cron::parse("0 0 * * *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 0, 0)),
            Some(t(2026, 10, 25, 0, 0))
        );
        assert_eq!(
            c.next_after(&t(2026, 10, 31, 12, 0)),
            Some(t(2026, 11, 1, 0, 0))
        );
        let c = Cron::parse("0 0 1 1 *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 12, 0)),
            Some(t(2027, 1, 1, 0, 0))
        );
        let c = Cron::parse("30 2 * * *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 2, 29)),
            Some(t(2026, 10, 24, 2, 30))
        );
        assert_eq!(
            c.next_after(&t(2026, 10, 24, 2, 30)),
            Some(t(2026, 10, 25, 2, 30))
        );
    }

    #[test]
    fn next_leap_day() {
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 1, 1, 0, 0)),
            Some(t(2028, 2, 29, 0, 0))
        );
    }

    #[test]
    fn next_impossible_dates() {
        assert_eq!(
            Cron::parse("0 0 31 2 *")
                .unwrap()
                .next_after(&t(2026, 1, 1, 0, 0)),
            None
        );
        assert_eq!(
            Cron::parse("0 0 30 2 *")
                .unwrap()
                .next_after(&t(2026, 1, 1, 0, 0)),
            None
        );
    }

    #[test]
    fn next_weekdays() {
        // 2026-10-23 is a Friday.
        assert_eq!(t(2026, 10, 23, 8, 0).weekday(), 5);
        let c = Cron::parse("0 7 * * 1-5").unwrap();
        assert_eq!(
            c.next_after(&t(2026, 10, 23, 8, 0)),
            Some(t(2026, 10, 26, 7, 0))
        );
        assert_eq!(
            c.next_after(&t(2026, 10, 23, 6, 0)),
            Some(t(2026, 10, 23, 7, 0))
        );
    }

    #[test]
    fn describe_phrases() {
        let d = |s: &str| Cron::parse(s).unwrap().describe();
        assert_eq!(d("* * * * *"), "every minute");
        assert_eq!(d("*/15 * * * *"), "every 15 minutes");
        assert_eq!(d("5 * * * *"), "hourly at :05");
        assert_eq!(d("@hourly"), "hourly at :00");
        assert_eq!(d("0 7 * * *"), "daily 07:00");
        assert_eq!(d("0 7 * * 1-5"), "weekdays 07:00");
        assert_eq!(d("0 9 * * 1"), "Mondays 09:00");
        assert_eq!(d("0 9 * * 1,3"), "Mon, Wed 09:00");
        assert_eq!(d("0 2 1 * *"), "1st of the month 02:00");
        assert_eq!(d("0 2 22 * *"), "22nd of the month 02:00");
        assert_eq!(d("0 0 1 1 *"), "Jan 1 00:00");
        assert_eq!(d("0 8,17 * * *"), "0 8,17 * * *");
        assert_eq!(d("0,30 8 * * *"), "0,30 8 * * *");
        assert_eq!(d("  0   8 *  * 1,3 "), "Mon, Wed 08:00");
    }

    #[test]
    fn calendar() {
        let c = from_calendar(Some(0), Some(7), None, Some(1), None).unwrap();
        assert_eq!(c, Cron::parse("0 7 * * 1").unwrap());
        let c = from_calendar(None, None, None, None, None).unwrap();
        assert_eq!(c, Cron::parse("* * * * *").unwrap());
        let c = from_calendar(Some(0), Some(0), None, Some(7), None).unwrap();
        assert_eq!(c, Cron::parse("0 0 * * 0").unwrap());
        assert!(from_calendar(Some(60), None, None, None, None).is_err());
        assert!(from_calendar(None, Some(24), None, None, None).is_err());
        assert!(from_calendar(None, None, Some(0), None, None).is_err());
        assert!(from_calendar(None, None, None, Some(8), None).is_err());
        assert!(from_calendar(None, None, None, None, Some(13)).is_err());
    }

    #[test]
    fn every_parse() {
        assert_eq!(parse_every("30m"), Ok(1800));
        assert_eq!(parse_every("2h"), Ok(7200));
        assert_eq!(parse_every("1d"), Ok(86_400));
        assert_eq!(parse_every("90s"), Ok(90));
        assert_eq!(parse_every("1h30m"), Ok(5400));
        assert_eq!(parse_every("1h 30m"), Ok(5400));
        assert_eq!(parse_every("45"), Ok(2700));
        assert_eq!(parse_every(" 2H "), Ok(7200));
        assert_eq!(parse_every("30s").unwrap_err(), "at least 1 minute");
        assert_eq!(parse_every("0").unwrap_err(), "at least 1 minute");
        assert_eq!(parse_every("367d").unwrap_err(), "at most 366 days");
        assert_eq!(parse_every("366d"), Ok(366 * 86_400));
        assert!(parse_every("").is_err());
        assert!(parse_every("abc").is_err());
        assert!(parse_every("5x").is_err());
        assert!(parse_every("h").is_err());
        assert!(parse_every("1h30").is_err());
    }

    #[test]
    fn every_describe() {
        assert_eq!(describe_every(1800), "every 30m");
        assert_eq!(describe_every(7200), "every 2h");
        assert_eq!(describe_every(5400), "every 1h 30m");
        assert_eq!(describe_every(86_400), "every day");
        assert_eq!(describe_every(172_800), "every 2 days");
        assert_eq!(describe_every(90), "every 1m 30s");
        for s in ["30m", "2h", "1h30m", "90s"].iter() {
            let secs = parse_every(s).unwrap();
            let text = describe_every(secs);
            assert_eq!(
                parse_every(text.trim_start_matches("every ")),
                Ok(secs),
                "{}",
                s
            );
        }
    }
}
