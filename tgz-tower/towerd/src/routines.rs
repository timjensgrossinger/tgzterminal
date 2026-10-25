//! Read-only list of the user's other scheduled jobs: launchd user agents
//! (macOS) and the crontab (Unix). Nothing here writes, loads, unloads or
//! runs a job, and a plist's `EnvironmentVariables` is never read, because
//! it is where secrets end up.

// Windows has neither launchd nor cron: the readers are unused there.
#![cfg_attr(windows, allow(dead_code, unused_imports))]

use crate::clock;
use jiff::tz::TimeZone;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tower_core::cron::{describe_every, from_calendar, Cron};
use tower_proto::RoutineRow;

/// What `launchctl list` says about one label.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LaunchStatus {
    pub pid: Option<u32>,
    pub last_exit: Option<i32>,
}

pub struct Collected {
    pub rows: Vec<RoutineRow>,
    /// Empty, or why part of the list is missing.
    pub note: String,
}

/// Longest `program` and crontab `label` kept, in characters.
const PROGRAM_MAX: usize = 120;
const LABEL_MAX: usize = 80;

/// Everything we can list. Blocking: runs `plutil`, `launchctl` and
/// `crontab` with a 5 second limit each.
pub fn collect(home: &Path, now: u64) -> Collected {
    #[cfg(windows)]
    {
        let _ = (home, now);
        Collected {
            rows: Vec::new(),
            note: "Not available on Windows".to_string(),
        }
    }
    #[cfg(not(windows))]
    {
        let tz = clock::local_tz();
        let mut rows = Vec::new();
        let mut notes = Vec::new();
        #[cfg(target_os = "macos")]
        rows.extend(collect_launchd(home, now, &tz));
        match run_cmd("crontab", &["-l"]) {
            Ok(out) => {
                if out.code == Some(0) {
                    rows.extend(parse_crontab(&out.stdout, home, now, &tz));
                } else if !out.stderr.to_ascii_lowercase().contains("no crontab") {
                    notes.push(format!("crontab: {}", out.stderr.trim()));
                }
            }
            Err(e) => notes.push(format!("crontab: {}", e)),
        }
        sort_rows(&mut rows);
        Collected {
            rows,
            note: notes.join("; "),
        }
    }
}

fn sort_rows(rows: &mut [RoutineRow]) {
    rows.sort_by(|a, b| {
        let src = |r: &RoutineRow| u8::from(r.source != "launchd");
        (!a.yours, src(a), a.label.as_str()).cmp(&(!b.yours, src(b), b.label.as_str()))
    });
}

#[cfg(target_os = "macos")]
fn collect_launchd(home: &Path, now: u64, tz: &TimeZone) -> Vec<RoutineRow> {
    let dir = home.join("Library/LaunchAgents");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "plist"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    let status = match run_cmd("launchctl", &["list"]) {
        Ok(out) if out.code == Some(0) => parse_launchctl_list(&out.stdout),
        _ => HashMap::new(),
    };
    let mut rows = Vec::new();
    for file in files {
        let path = file.to_string_lossy().to_string();
        let Ok(out) = run_cmd("plutil", &["-convert", "json", "-o", "-", &path]) else {
            continue;
        };
        if out.code != Some(0) {
            continue;
        }
        let Ok(plist) = serde_json::from_str::<serde_json::Value>(&out.stdout) else {
            continue;
        };
        let label = plist.get("Label").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(row) = launchd_row(&plist, &file, status.get(label), home, now, tz, &file_mtime)
        {
            rows.push(row);
        }
    }
    rows
}

fn file_mtime(p: &Path) -> Option<u64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    m.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

struct CmdOut {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run a command with a 5 second limit, stdin closed. Killed on timeout.
fn run_cmd(program: &str, args: &[&str]) -> Result<CmdOut, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {}", program, e))?;
    fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    }
    let out_t = drain(child.stdout.take());
    let err_t = drain(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{} timed out", program));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("{}: {}", program, e)),
        }
    };
    Ok(CmdOut {
        code: status.code(),
        stdout: out_t.join().unwrap_or_default(),
        stderr: err_t.join().unwrap_or_default(),
    })
}

/// `launchctl list`: a `PID  Status  Label` header, then tab separated
/// rows. PID `-` means not running; Status is the last exit status.
pub fn parse_launchctl_list(out: &str) -> HashMap<String, LaunchStatus> {
    let mut map = HashMap::new();
    for line in out.lines() {
        let mut cols = line.splitn(3, '\t');
        let (Some(pid), Some(status), Some(label)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        if pid.trim() == "PID" {
            continue;
        }
        let label = label.trim();
        if label.is_empty() {
            continue;
        }
        map.insert(
            label.to_string(),
            LaunchStatus {
                pid: pid.trim().parse().ok(),
                last_exit: status.trim().parse().ok(),
            },
        );
    }
    map
}

/// Replace a leading home folder with `~`.
fn shorten_home(s: &str, home: &Path) -> String {
    let h = home.to_string_lossy();
    let h = h.trim_end_matches('/');
    if h.is_empty() {
        return s.to_string();
    }
    match s.strip_prefix(h) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{}", rest),
        _ => s.to_string(),
    }
}

/// Keep at most `max` characters; a cut ends in `…`.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

fn under_home(s: &str, home: &Path) -> bool {
    let h = home.to_string_lossy();
    let h = h.trim_end_matches('/');
    !h.is_empty() && s.strip_prefix(h).is_some_and(|r| r.starts_with('/'))
}

fn small_u8(v: &serde_json::Value, key: &str) -> Option<u8> {
    v.get(key)?.as_u64().and_then(|n| u8::try_from(n).ok())
}

fn calendar_entries(plist: &serde_json::Value) -> Vec<Cron> {
    let Some(v) = plist.get("StartCalendarInterval") else {
        return Vec::new();
    };
    let dicts: Vec<&serde_json::Value> = match v {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(_) => vec![v],
        _ => Vec::new(),
    };
    dicts
        .into_iter()
        .filter_map(|d| {
            from_calendar(
                small_u8(d, "Minute"),
                small_u8(d, "Hour"),
                small_u8(d, "Day"),
                small_u8(d, "Weekday"),
                small_u8(d, "Month"),
            )
            .ok()
        })
        .collect()
}

/// One plist, already converted to JSON.
pub fn launchd_row(
    plist: &serde_json::Value,
    file: &Path,
    status: Option<&LaunchStatus>,
    home: &Path,
    now: u64,
    tz: &TimeZone,
    mtime: &dyn Fn(&Path) -> Option<u64>,
) -> Option<RoutineRow> {
    let label = plist.get("Label")?.as_str()?.to_string();
    let flag = |k: &str| plist.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let nonempty = |k: &str| match plist.get(k) {
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => !o.is_empty(),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        _ => false,
    };

    // argv: ProgramArguments, else Program.
    let argv: Vec<String> = match plist.get("ProgramArguments").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        _ => plist
            .get("Program")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };
    let first = plist
        .get("Program")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| argv.first().cloned())
        .unwrap_or_default();
    // Vendor updaters install into ~/Library; your own scripts live elsewhere.
    let yours = under_home(&first, home) && !under_home(&first, &home.join("Library"));
    let program = truncate(
        &argv
            .iter()
            .map(|a| shorten_home(a, home))
            .collect::<Vec<_>>()
            .join(" "),
        PROGRAM_MAX,
    );

    let interval = plist
        .get("StartInterval")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0);
    let calendar = calendar_entries(plist);
    let keep_alive = match plist.get("KeepAlive") {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Object(_)) => true,
        _ => false,
    };

    let mut parts = Vec::new();
    if let Some(secs) = interval {
        parts.push(describe_every(secs));
    }
    if let Some(c) = calendar.first() {
        let mut t = c.describe();
        if calendar.len() > 1 {
            t.push_str(&format!(" (+{} more)", calendar.len() - 1));
        }
        parts.push(t);
    }
    if flag("RunAtLoad") {
        parts.push("at login".to_string());
    }
    if keep_alive {
        parts.push("keeps running".to_string());
    }
    if nonempty("WatchPaths") || nonempty("QueueDirectories") {
        parts.push("when files change".to_string());
    }
    let schedule_text = if parts.is_empty() {
        "on demand".to_string()
    } else {
        parts.join(", ")
    };

    let next_fire_unix = calendar
        .iter()
        .filter_map(|c| clock::next_fire(c, now, tz))
        .min();

    // Logs: the newer of stdout/stderr by mtime.
    let out_path = plist.get("StandardOutPath").and_then(|v| v.as_str());
    let err_path = plist.get("StandardErrorPath").and_then(|v| v.as_str());
    let timed: Vec<(&str, u64)> = [out_path, err_path]
        .into_iter()
        .flatten()
        .filter_map(|p| mtime(Path::new(p)).map(|t| (p, t)))
        .collect();
    let last_activity_unix = timed.iter().map(|(_, t)| *t).max();
    let log_path = timed
        .iter()
        .max_by_key(|(_, t)| *t)
        .map(|(p, _)| p.to_string())
        .or_else(|| out_path.map(str::to_string));

    let loaded = status.is_some();
    let disabled = flag("Disabled");
    let stalled = match (interval, last_activity_unix) {
        (Some(secs), Some(last)) => loaded && !disabled && now > last.saturating_add(3 * secs),
        _ => false,
    };

    Some(RoutineRow {
        source: "launchd".to_string(),
        label,
        schedule_text,
        next_fire_unix,
        loaded,
        disabled,
        pid: status.and_then(|s| s.pid),
        last_exit: status.and_then(|s| s.last_exit),
        last_activity_unix,
        stalled,
        program,
        log_path,
        file_path: Some(file.to_string_lossy().to_string()),
        yours,
    })
}

/// Split off the first whitespace separated token.
fn split_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

fn is_env_line(line: &str) -> bool {
    if line.starts_with(|c: char| c.is_ascii_digit() || c == '*' || c == '@') {
        return false;
    }
    match line.split_once('=') {
        Some((name, _)) => {
            let name = name.trim_end();
            !name.is_empty() && !name.contains(char::is_whitespace)
        }
        None => false,
    }
}

/// `crontab -l` text. Environment lines are skipped, their values never kept.
pub fn parse_crontab(text: &str, home: &Path, now: u64, tz: &TimeZone) -> Vec<RoutineRow> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || is_env_line(line) {
            continue;
        }
        let (spec, command) = if line.starts_with('@') {
            let (m, rest) = split_token(line);
            (m.to_string(), rest)
        } else {
            let mut rest = line;
            let mut fields = Vec::new();
            for _ in 0..5 {
                let (t, r) = split_token(rest);
                if t.is_empty() {
                    break;
                }
                fields.push(t);
                rest = r;
            }
            (fields.join(" "), rest)
        };
        let cmd = command
            .split_whitespace()
            .map(|t| shorten_home(t, home))
            .collect::<Vec<_>>()
            .join(" ");
        let (schedule_text, next_fire_unix) = if spec.eq_ignore_ascii_case("@reboot") {
            ("at boot".to_string(), None)
        } else {
            match Cron::parse(&spec) {
                Ok(c) => (c.describe(), clock::next_fire(&c, now, tz)),
                Err(e) => (format!("can't read: {}", e), None),
            }
        };
        let label_src = if cmd.is_empty() {
            shorten_home(line, home)
        } else {
            cmd.clone()
        };
        rows.push(RoutineRow {
            source: "cron".to_string(),
            label: truncate(&label_src, LABEL_MAX),
            schedule_text,
            next_fire_unix,
            loaded: true,
            program: truncate(&cmd, PROGRAM_MAX),
            yours: true,
            ..RoutineRow::default()
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_792_000_000; // 2026-10-14T... fixed
    const HOME: &str = "/Users/test";

    fn tz() -> TimeZone {
        TimeZone::get("Europe/Vienna").unwrap()
    }

    fn row(
        plist: serde_json::Value,
        st: Option<LaunchStatus>,
        mt: Option<u64>,
    ) -> Option<RoutineRow> {
        launchd_row(
            &plist,
            Path::new("/Users/test/Library/LaunchAgents/x.plist"),
            st.as_ref(),
            Path::new(HOME),
            NOW,
            &tz(),
            &move |_| mt,
        )
    }

    fn loaded() -> Option<LaunchStatus> {
        Some(LaunchStatus {
            pid: None,
            last_exit: Some(0),
        })
    }

    #[test]
    fn launchctl_list_parses_pid_and_status() {
        let out = "PID\tStatus\tLabel\n-\t0\tcom.a\n123\t-9\tcom.b\n-\t78\tcom.c.d\n";
        let m = parse_launchctl_list(out);
        assert_eq!(m.len(), 3);
        assert_eq!(
            m["com.a"],
            LaunchStatus {
                pid: None,
                last_exit: Some(0)
            }
        );
        assert_eq!(
            m["com.b"],
            LaunchStatus {
                pid: Some(123),
                last_exit: Some(-9)
            }
        );
        assert_eq!(m["com.c.d"].last_exit, Some(78));
    }

    #[test]
    fn interval_job_with_logs_and_stall() {
        let plist = json!({
            "Label": "com.me.sync",
            "ProgramArguments": ["/Users/test/bin/sync", "--all"],
            "StartInterval": 900,
            "StandardOutPath": "/tmp/out.log",
        });
        let fresh = row(plist.clone(), loaded(), Some(NOW - 100)).unwrap();
        assert_eq!(fresh.schedule_text, "every 15m");
        assert_eq!(fresh.next_fire_unix, None);
        assert!(!fresh.stalled);
        assert_eq!(fresh.last_activity_unix, Some(NOW - 100));
        assert_eq!(fresh.log_path.as_deref(), Some("/tmp/out.log"));
        assert!(fresh.loaded);

        let old = row(plist.clone(), loaded(), Some(NOW - 3 * 900 - 1)).unwrap();
        assert!(old.stalled);
        let not_loaded = row(plist.clone(), None, Some(NOW - 10_000)).unwrap();
        assert!(!not_loaded.stalled && !not_loaded.loaded);
        let mut dis = plist;
        dis["Disabled"] = json!(true);
        let dis = row(dis, loaded(), Some(NOW - 10_000)).unwrap();
        assert!(dis.disabled && !dis.stalled);
    }

    #[test]
    fn newer_log_wins() {
        let plist = json!({
            "Label": "a", "Program": "/bin/x",
            "StandardOutPath": "/tmp/o", "StandardErrorPath": "/tmp/e",
        });
        let r = launchd_row(
            &plist,
            Path::new("/p.plist"),
            None,
            Path::new(HOME),
            NOW,
            &tz(),
            &|p| Some(if p == Path::new("/tmp/e") { 20 } else { 10 }),
        )
        .unwrap();
        assert_eq!(r.log_path.as_deref(), Some("/tmp/e"));
        assert_eq!(r.last_activity_unix, Some(20));
        assert_eq!(r.file_path.as_deref(), Some("/p.plist"));
    }

    #[test]
    fn calendar_dict() {
        let plist = json!({
            "Label": "c", "Program": "/bin/x",
            "StartCalendarInterval": {"Hour": 7, "Minute": 0},
        });
        let r = row(plist, loaded(), None).unwrap();
        assert_eq!(
            r.schedule_text,
            Cron::parse("0 7 * * *").unwrap().describe()
        );
        let next = r.next_fire_unix.unwrap();
        assert!(next > NOW && next - NOW <= 86_400);
    }

    #[test]
    fn calendar_array_counts_more() {
        let plist = json!({
            "Label": "c", "Program": "/bin/x",
            "StartCalendarInterval": [
                {"Hour": 7, "Minute": 0},
                {"Hour": 12, "Minute": 30},
                {"Hour": 18, "Minute": 0, "Weekday": 5},
            ],
        });
        let r = row(plist, loaded(), None).unwrap();
        assert!(
            r.schedule_text.ends_with(" (+2 more)"),
            "{}",
            r.schedule_text
        );
        let first = clock::next_fire(&Cron::parse("0 7 * * *").unwrap(), NOW, &tz()).unwrap();
        let second = clock::next_fire(&Cron::parse("30 12 * * *").unwrap(), NOW, &tz()).unwrap();
        assert!(r.next_fire_unix.unwrap() <= first.min(second));
    }

    #[test]
    fn run_at_load_keep_alive_and_watch() {
        let r = row(
            json!({"Label": "a", "Program": "/x", "RunAtLoad": true}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.schedule_text, "at login");
        let r = row(
            json!({"Label": "a", "Program": "/x", "KeepAlive": {"SuccessfulExit": false}}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.schedule_text, "keeps running");
        let r = row(
            json!({"Label": "a", "Program": "/x", "KeepAlive": false}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.schedule_text, "on demand");
        let r = row(
            json!({"Label": "a", "Program": "/x", "WatchPaths": ["/tmp/a"], "RunAtLoad": true}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.schedule_text, "at login, when files change");
        let r = row(
            json!({"Label": "a", "Program": "/x", "QueueDirectories": ["/tmp/q"]}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.schedule_text, "when files change");
    }

    #[test]
    fn program_vs_arguments_and_shortening() {
        let r = row(
            json!({"Label": "a", "Program": "/usr/bin/true"}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.program, "/usr/bin/true");
        assert!(!r.yours);
        let r = row(
            json!({"Label": "a", "ProgramArguments": ["/Users/test/bin/go", "/Users/test/data", "/Users/testing/x"]}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.program, "~/bin/go ~/data /Users/testing/x");
        assert!(r.yours);
        let r = row(
            json!({"Label": "u", "Program": "/Users/test/Library/Application Support/Vendor/updater"}),
            None,
            None,
        )
        .unwrap();
        assert!(!r.yours, "a vendor updater in ~/Library is not yours");
        let long = "x".repeat(300);
        let r = row(
            json!({"Label": "a", "ProgramArguments": ["/bin/sh", long]}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.program.chars().count(), 120);
        assert!(r.program.ends_with('…'));
    }

    #[test]
    fn missing_label_is_none() {
        assert!(row(json!({"Program": "/x"}), None, None).is_none());
    }

    #[test]
    fn environment_variables_never_leak() {
        let r = row(
            json!({
                "Label": "a", "Program": "/Users/test/x",
                "EnvironmentVariables": {"API_TOKEN": "s3cr3t-value"},
            }),
            loaded(),
            None,
        )
        .unwrap();
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("s3cr3t-value") && !s.contains("API_TOKEN"));
    }

    #[test]
    fn crontab_lines() {
        let text = "# m\n\nMAILTO=me@example.com\nPATH=/usr/bin:/bin\n\
                    @reboot /Users/test/start.sh\n\
                    0 7 * * 1-5 /bin/zsh /Users/test/s.sh\n\
                    nonsense line here\n";
        let rows = parse_crontab(text, Path::new(HOME), NOW, &tz());
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].schedule_text, "at boot");
        assert_eq!(rows[0].next_fire_unix, None);
        assert_eq!(rows[0].label, "~/start.sh");
        assert_eq!(rows[1].source, "cron");
        assert_eq!(rows[1].program, "/bin/zsh ~/s.sh");
        assert_eq!(
            rows[1].schedule_text,
            Cron::parse("0 7 * * 1-5").unwrap().describe()
        );
        assert!(rows[1].next_fire_unix.unwrap() > NOW);
        assert!(rows[1].loaded && rows[1].yours);
        assert!(
            rows[2].schedule_text.starts_with("can't read: "),
            "{}",
            rows[2].schedule_text
        );
        let all = serde_json::to_string(&rows).unwrap();
        assert!(!all.contains("example.com"));
    }

    #[test]
    fn sorting_puts_yours_then_launchd_first() {
        let mk = |source: &str, label: &str, yours: bool| RoutineRow {
            source: source.into(),
            label: label.into(),
            yours,
            ..RoutineRow::default()
        };
        let mut rows = vec![
            mk("cron", "b", false),
            mk("launchd", "z", false),
            mk("cron", "a", true),
            mk("launchd", "y", true),
        ];
        sort_rows(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(order, ["y", "a", "z", "b"]);
    }
}
