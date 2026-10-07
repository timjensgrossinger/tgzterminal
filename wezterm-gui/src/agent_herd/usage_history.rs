//! Tokens used per day, read from the transcripts already on disk.
//!
//! Nothing is recorded for this: every Claude and Codex transcript line is
//! timestamped by the agent that wrote it, so "today" and "the last week" are
//! a fold over the transcripts touched in that window. The same caveats as
//! [`super::usage`] apply, and the same definition of a token total: input
//! that was not read from the prompt cache, plus output.
//!
//! * Claude: usage is per message, repeated on each of the message's records,
//!   and an early record can carry a partial output count. A message is
//!   counted once, at its largest reported usage, on the day of its first
//!   record. A resumed or forked session copies earlier messages into its own
//!   file, so messages are de-duplicated across files too, not only within
//!   one.
//! * Codex: `token_count` events carry the session's running total, so a day
//!   gets the growth of that total across its events (each event is written
//!   twice; the repeat grows nothing). A resumed session starts a new file
//!   whose first event restates the total it inherited; that one sets the
//!   baseline and adds nothing.
//!
//! Reads are incremental per file (see [`super::incremental`]).
//!
//! Pure apart from the file reads; no GUI, mux or terminal types.

use super::usage::UsageFormat;
use chrono::NaiveDate;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

/// How far back the history reaches, and so the longest graph on offer.
/// Claude Code deletes its transcripts after 30 days unless told otherwise,
/// so there is rarely anything older to find.
pub const HISTORY_DAYS: usize = 30;

/// Directory levels searched below a vendor's session root. Claude nests a
/// session's subagent transcripts three levels down; Codex files sit under
/// `YYYY/MM/DD`.
const MAX_DEPTH: usize = 4;

/// Pause after each transcript read. The first scan reads every recent
/// transcript whole; this keeps it from holding a core and the disk in one
/// uninterrupted burst while the user is working.
const PAUSE_BETWEEN_FILES: Duration = Duration::from_millis(1);

/// Tokens (uncached input + output) per local calendar day.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageHistory {
    pub days: BTreeMap<NaiveDate, u64>,
}

impl UsageHistory {
    /// Tokens on each of the `count` days ending with `today`, oldest first.
    pub fn series(&self, today: NaiveDate, count: usize) -> Vec<u64> {
        (0..count)
            .rev()
            .map(|back| {
                today
                    .checked_sub_signed(chrono::Duration::days(back as i64))
                    .and_then(|day| self.days.get(&day).copied())
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Tokens over the `count` days ending with `today`.
    pub fn total(&self, today: NaiveDate, count: usize) -> u64 {
        self.series(today, count).iter().sum()
    }
}

/// One bar per value, as block characters an eighth of a cell apart. The
/// tallest value fills the cell; a zero day keeps the thinnest bar so the
/// graph's width always shows how many days it covers.
pub fn sparkline(values: &[u64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|value| {
            if max == 0 || *value == 0 {
                BARS[0]
            } else {
                // 1..=7: any non-zero day stands clear of an empty one.
                let step = (*value as u128 * 7).div_ceil(max as u128) as usize;
                BARS[step.clamp(1, 7)]
            }
        })
        .collect()
}

/// The local calendar day of an RFC 3339 timestamp.
fn local_day(timestamp: &str) -> Option<NaiveDate> {
    let at = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    Some(at.with_timezone(&chrono::Local).date_naive())
}

/// A message id, hashed: the history keeps one entry per message for a
/// month of transcripts, and the ids themselves are never needed again.
type MessageKey = u64;

fn message_key(id: &str) -> MessageKey {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[derive(Clone, Debug, Default)]
struct FileState {
    /// Bytes consumed so far: always just past a complete line.
    offset: u64,
    /// Claude: the day each message was first seen and its largest usage.
    /// Kept per message, not summed, so files can be de-duplicated against
    /// each other.
    messages: HashMap<MessageKey, (NaiveDate, u64)>,
    /// Codex: tokens per day.
    days: BTreeMap<NaiveDate, u64>,
    /// Codex: the running total as of the last event.
    running_total: u64,
    /// Codex: the model has produced something in this file.
    seen_output: bool,
    /// Codex: a populated `token_count` has been read from this file.
    seen_count: bool,
}

#[derive(Deserialize)]
struct ClaudeRecord {
    timestamp: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn fold_claude(line: &str, state: &mut FileState, day_of: &dyn Fn(&str) -> Option<NaiveDate>) {
    if !line.contains("\"usage\"") {
        return;
    }
    let Ok(record) = serde_json::from_str::<ClaudeRecord>(line) else {
        return;
    };
    let Some(message) = record.message else {
        return;
    };
    let (Some(id), Some(usage)) = (message.id, message.usage) else {
        return;
    };
    let tokens = usage.input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.output_tokens.unwrap_or(0);
    match state.messages.get_mut(&message_key(&id)) {
        // A message stays on the day it was first seen, so one that
        // straddles midnight is not split or moved by its later records.
        Some((_, counted)) => *counted = (*counted).max(tokens),
        None => {
            if let Some(day) = record.timestamp.as_deref().and_then(day_of) {
                state.messages.insert(message_key(&id), (day, tokens));
            }
        }
    }
}

#[derive(Deserialize)]
struct CodexRecord {
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    info: Option<CodexTokenInfo>,
}

#[derive(Deserialize)]
struct CodexTokenInfo {
    total_token_usage: Option<CodexTokenUsage>,
}

#[derive(Deserialize)]
struct CodexTokenUsage {
    input_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Whether a Codex record is the model saying or thinking something.
fn is_codex_model_output(line: &str) -> bool {
    line.contains("\"agent_message\"")
        || line.contains("\"role\":\"assistant\"")
        || line.contains("\"type\":\"reasoning\"")
}

fn fold_codex(line: &str, state: &mut FileState, day_of: &dyn Fn(&str) -> Option<NaiveDate>) {
    if !line.contains("\"token_count\"") {
        if !state.seen_count && is_codex_model_output(line) {
            state.seen_output = true;
        }
        return;
    }
    let Ok(record) = serde_json::from_str::<CodexRecord>(line) else {
        return;
    };
    let Some(payload) = record.payload else {
        return;
    };
    if payload.kind.as_deref() != Some("token_count") {
        return;
    }
    // `info` is null on the events that only report rate limits.
    let Some(total) = payload.info.and_then(|info| info.total_token_usage) else {
        return;
    };
    let total = total
        .input_tokens
        .unwrap_or(0)
        .saturating_sub(total.cached_input_tokens.unwrap_or(0))
        + total.output_tokens.unwrap_or(0);
    let first = !state.seen_count;
    state.seen_count = true;
    let grew = total.saturating_sub(state.running_total);
    state.running_total = state.running_total.max(total);
    // A total reported before the model has produced anything in this file
    // was inherited from the session this one resumes; its own file has
    // already counted it.
    if grew == 0 || (first && !state.seen_output) {
        return;
    }
    if let Some(day) = record.timestamp.as_deref().and_then(day_of) {
        *state.days.entry(day).or_default() += grew;
    }
}

fn fold_line(
    format: UsageFormat,
    line: &str,
    state: &mut FileState,
    day_of: &dyn Fn(&str) -> Option<NaiveDate>,
) {
    match format {
        UsageFormat::Claude => fold_claude(line, state, day_of),
        UsageFormat::Codex => fold_codex(line, state, day_of),
    }
}

/// The per-day totals over a set of files' states. A Claude message present
/// in several files is counted once, at its largest usage, on the earliest
/// day any file saw it.
fn merge<'a>(states: impl Iterator<Item = &'a FileState>) -> UsageHistory {
    let mut history = UsageHistory::default();
    let mut messages: HashMap<MessageKey, (NaiveDate, u64)> = HashMap::new();
    for state in states {
        for (day, tokens) in &state.days {
            *history.days.entry(*day).or_default() += tokens;
        }
        for (key, (day, tokens)) in &state.messages {
            messages
                .entry(*key)
                .and_modify(|(seen_day, seen_tokens)| {
                    *seen_day = (*seen_day).min(*day);
                    *seen_tokens = (*seen_tokens).max(*tokens);
                })
                .or_insert((*day, *tokens));
        }
    }
    for (day, tokens) in messages.into_values() {
        *history.days.entry(day).or_default() += tokens;
    }
    history.days.retain(|_, tokens| *tokens > 0);
    history
}

/// Every `.jsonl` under `root` modified at or after `since`, at most
/// `MAX_DEPTH` directories down. Unreadable directories are skipped.
fn recent_transcripts(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let mut found = vec![];
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && metadata.modified().is_ok_and(|modified| modified >= since)
            {
                found.push(path);
            }
        }
    }
    found
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, FileState>>> = LazyLock::new(Default::default);

fn cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, FileState>> {
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Bring `state` up to date with `path`. Returns whether anything was read.
fn read_file(path: &Path, format: UsageFormat, state: &mut FileState) -> std::io::Result<bool> {
    let len = std::fs::metadata(path)?.len();
    if state.offset > len {
        // The file shrank: it was rewritten, so fold it again from the start.
        *state = FileState::default();
    }
    if state.offset == len {
        return Ok(false);
    }
    let mut offset = state.offset;
    super::incremental::read_appended(path, len, &mut offset, u64::MAX, &mut |line| {
        fold_line(format, line, state, &local_day)
    })?;
    state.offset = offset;
    Ok(true)
}

/// Tokens per day over the last [`HISTORY_DAYS`], across every Claude and
/// Codex transcript under `home` that was written to in that time.
///
/// The first call reads those transcripts whole; later calls stat each one
/// and read only what was appended, which for an idle transcript is nothing.
///
/// Blocking file I/O: call it off the GUI thread.
pub fn scan_history(home: &Path, now: SystemTime) -> UsageHistory {
    let since = now
        .checked_sub(Duration::from_secs(HISTORY_DAYS as u64 * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let codex = home.join(".codex");
    let sources = [
        (home.join(".claude").join("projects"), UsageFormat::Claude),
        (codex.join("sessions"), UsageFormat::Codex),
        (codex.join("archived_sessions"), UsageFormat::Codex),
    ];

    let mut seen: HashSet<PathBuf> = HashSet::new();
    for (root, format) in sources.iter() {
        for path in recent_transcripts(root, since) {
            // Taken out for the read so the lock is not held across file I/O.
            let mut state = cache().remove(&path).unwrap_or_default();
            // A transcript is someone else's file: a parser bug on one of
            // them costs that file's tokens, not the whole history.
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                read_file(&path, *format, &mut state)
            }));
            match read {
                Ok(Ok(read_something)) => {
                    cache().insert(path.clone(), state);
                    seen.insert(path);
                    if read_something {
                        std::thread::sleep(PAUSE_BETWEEN_FILES);
                    }
                }
                Ok(Err(_)) | Err(_) => {}
            }
        }
    }
    let mut cache = cache();
    // Transcripts that aged out of the window are not read again.
    cache.retain(|path, _| seen.contains(path));
    merge(cache.values())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    /// The UTC day, so the tests do not depend on the machine's time zone.
    fn utc_day(timestamp: &str) -> Option<NaiveDate> {
        Some(
            chrono::DateTime::parse_from_rfc3339(timestamp)
                .ok()?
                .naive_utc()
                .date(),
        )
    }

    fn claude(timestamp: &str, id: &str, input: u64, cache_write: u64, output: u64) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "type": "assistant",
            "message": {
                "id": id,
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": cache_write,
                    "cache_read_input_tokens": 500_000,
                    "output_tokens": output,
                },
            },
        })
        .to_string()
    }

    fn codex(timestamp: &str, input: u64, cached: u64, output: u64) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": cached,
                    "output_tokens": output,
                } },
            },
        })
        .to_string()
    }

    /// The model speaking, as Codex records it before a turn's token count.
    fn codex_output(timestamp: &str) -> String {
        serde_json::json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": { "type": "agent_message", "message": "…" },
        })
        .to_string()
    }

    fn state(format: UsageFormat, lines: &[String]) -> FileState {
        let mut state = FileState::default();
        for line in lines {
            fold_line(format, line, &mut state, &utc_day);
        }
        state
    }

    fn fold(format: UsageFormat, lines: &[String]) -> BTreeMap<NaiveDate, u64> {
        merge(std::iter::once(&state(format, lines))).days
    }

    #[test]
    fn claude_messages_land_on_their_day_once() {
        let days = fold(
            UsageFormat::Claude,
            &[
                claude("2026-10-05T10:00:00Z", "m1", 10, 5, 20),
                claude("2026-10-05T10:00:01Z", "m1", 10, 5, 20),
                claude("2026-10-06T09:00:00Z", "m2", 1, 0, 2),
            ],
        );
        assert_eq!(days.get(&day("2026-10-05")), Some(&35));
        assert_eq!(days.get(&day("2026-10-06")), Some(&3));
    }

    #[test]
    fn a_claude_message_keeps_its_first_day_and_largest_usage() {
        // Records either side of midnight, the first with a partial output
        // count and a stale repeat after the full one: counted once, on the
        // first day, at the largest usage.
        let days = fold(
            UsageFormat::Claude,
            &[
                claude("2026-10-05T23:59:59Z", "m1", 10, 0, 1),
                claude("2026-10-06T00:00:01Z", "m1", 10, 0, 90),
                claude("2026-10-06T00:00:02Z", "m1", 10, 0, 1),
            ],
        );
        assert_eq!(days.get(&day("2026-10-05")), Some(&100));
        assert_eq!(days.get(&day("2026-10-06")), None);
    }

    #[test]
    fn a_claude_message_copied_into_another_file_is_counted_once() {
        // A resumed session's file starts with the messages it inherited.
        let original = state(
            UsageFormat::Claude,
            &[claude("2026-10-05T10:00:00Z", "m1", 10, 0, 90)],
        );
        let resumed = state(
            UsageFormat::Claude,
            &[
                claude("2026-10-06T08:00:00Z", "m1", 10, 0, 90),
                claude("2026-10-06T08:05:00Z", "m2", 5, 0, 5),
            ],
        );
        let days = merge(vec![&original, &resumed].into_iter()).days;
        assert_eq!(days.get(&day("2026-10-05")), Some(&100));
        assert_eq!(days.get(&day("2026-10-06")), Some(&10));
    }

    #[test]
    fn codex_days_get_the_growth_of_the_running_total() {
        let days = fold(
            UsageFormat::Codex,
            &[
                codex_output("2026-10-05T09:59:00Z"),
                codex("2026-10-05T10:00:00Z", 1_000, 800, 50),
                codex("2026-10-05T11:00:00Z", 2_000, 1_500, 100),
                // Codex writes each event twice; the repeat adds nothing.
                codex("2026-10-05T11:00:00Z", 2_000, 1_500, 100),
                codex("2026-10-06T09:00:00Z", 3_000, 2_000, 400),
            ],
        );
        assert_eq!(days.get(&day("2026-10-05")), Some(&600));
        assert_eq!(days.get(&day("2026-10-06")), Some(&800));
    }

    #[test]
    fn a_resumed_codex_file_does_not_recount_what_it_inherited() {
        // No model output yet in this file: the first total is the one the
        // earlier file ended on.
        let days = fold(
            UsageFormat::Codex,
            &[
                codex("2026-10-06T09:00:00Z", 2_000, 1_500, 100),
                codex_output("2026-10-06T09:01:00Z"),
                codex("2026-10-06T09:01:30Z", 2_400, 1_600, 150),
            ],
        );
        assert_eq!(days.get(&day("2026-10-06")), Some(&350));
    }

    #[test]
    fn records_without_usage_or_timestamp_are_skipped() {
        let no_timestamp = serde_json::json!({
            "message": { "id": "m1", "usage": { "input_tokens": 5, "output_tokens": 5 } },
        })
        .to_string();
        assert!(fold(
            UsageFormat::Claude,
            &[
                no_timestamp,
                "garbage \"usage\"".to_string(),
                "{}".to_string()
            ]
        )
        .is_empty());
        let rate_limits = serde_json::json!({
            "timestamp": "2026-10-05T10:00:00Z",
            "payload": { "type": "token_count", "info": null },
        })
        .to_string();
        assert!(fold(UsageFormat::Codex, &[rate_limits]).is_empty());
    }

    #[test]
    fn series_and_total_cover_the_days_up_to_today() {
        let history = UsageHistory {
            days: vec![
                (day("2026-10-01"), 5),
                (day("2026-10-05"), 10),
                (day("2026-10-07"), 3),
            ]
            .into_iter()
            .collect(),
        };
        let today = day("2026-10-07");
        assert_eq!(history.series(today, 3), vec![10, 0, 3]);
        assert_eq!(history.total(today, 1), 3);
        assert_eq!(history.total(today, 7), 18);
        assert_eq!(history.total(today, 3), 13);
    }

    #[test]
    fn sparkline_scales_to_the_tallest_day() {
        assert_eq!(sparkline(&[0, 0, 0]), "▁▁▁");
        assert_eq!(sparkline(&[0, 50, 100]), "▁▅█");
        // A small non-zero day still stands clear of an empty one.
        assert_eq!(sparkline(&[0, 1, 1_000_000]), "▁▂█");
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn scan_reads_both_vendors_and_ignores_stale_files() {
        let home = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let stamp = chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339();
        let today = local_day(&stamp).unwrap();

        let claude_dir = home.path().join(".claude/projects/-r/session/subagents");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            home.path().join(".claude/projects/-r/a.jsonl"),
            format!("{}\n", claude(&stamp, "m1", 10, 0, 5)),
        )
        .unwrap();
        std::fs::write(
            claude_dir.join("agent.jsonl"),
            format!("{}\n", claude(&stamp, "m2", 1, 0, 1)),
        )
        .unwrap();
        // The same message again, in a second session's file.
        std::fs::write(
            home.path().join(".claude/projects/-r/b.jsonl"),
            format!("{}\n", claude(&stamp, "m1", 10, 0, 5)),
        )
        .unwrap();
        for (dir, input) in [("sessions/2026/10/07", 100), ("archived_sessions", 1_000)] {
            let dir = home.path().join(".codex").join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("rollout-x.jsonl"),
                format!(
                    "{}\n{}\n",
                    codex_output(&stamp),
                    codex(&stamp, input, 60, 10)
                ),
            )
            .unwrap();
            // Not a transcript.
            std::fs::write(dir.join("notes.txt"), "token_count").unwrap();
        }

        let history = scan_history(home.path(), now);
        assert_eq!(history.days.get(&today), Some(&(15 + 2 + 50 + 950)));
        // A second scan reads nothing new and reaches the same answer.
        assert_eq!(scan_history(home.path(), now), history);

        // Scanning from a point after every file's window finds nothing.
        let later = now + Duration::from_secs((HISTORY_DAYS as u64 + 1) * 24 * 60 * 60);
        assert!(scan_history(home.path(), later).days.is_empty());
    }
}
