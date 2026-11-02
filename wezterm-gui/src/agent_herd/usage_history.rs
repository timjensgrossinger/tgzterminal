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
//! * Gemini: each `gemini` message carries its own tokens. A message can be
//!   appended again, or re-listed by a `$set` checkpoint, so a repeated id
//!   replaces the earlier figure. `input` includes `cached`.
//! * Copilot: `session.shutdown` carries the per-model totals of that run, so
//!   a session's tokens only appear once it ends, and a resumed session adds
//!   a further shutdown of its own.
//! * OpenCode: its store records running totals per session, so a session's
//!   whole total lands on the day it was last updated.
//!
//! The Gemini and Copilot shapes come from third-party parsers of those files
//! rather than a published schema. Anything unrecognised counts as nothing.
//!
//! Reads are incremental per file (see [`super::incremental`]).
//!
//! Pure apart from the file reads; no GUI, mux or terminal types.

use super::vendor::SessionRoot;
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
    pub days: BTreeMap<NaiveDate, DayTokens>,
    /// OpenCode's own cost and token totals, which its store records per
    /// session; `None` when there is no OpenCode store.
    pub opencode: Option<OpencodeSpend>,
}

/// One day's tokens, per vendor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DayTokens {
    pub claude: u64,
    pub codex: u64,
    pub opencode: u64,
    pub gemini: u64,
    pub copilot: u64,
}

impl DayTokens {
    pub fn total(&self) -> u64 {
        self.claude + self.codex + self.opencode + self.gemini + self.copilot
    }
}

/// Which vendor's transcript shapes a history file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryFormat {
    Claude,
    Codex,
    Gemini,
    Copilot,
}

/// Cost and tokens over some span.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spend {
    /// US dollars, in millionths: kept integral so the history stays `Eq`.
    pub cost_micros: u64,
    pub tokens: u64,
}

impl Spend {
    fn from_store((cost, tokens): (f64, u64)) -> Self {
        Self {
            cost_micros: (cost * 1_000_000.).round().max(0.) as u64,
            tokens,
        }
    }

    fn add(&mut self, other: &Self) {
        self.cost_micros += other.cost_micros;
        self.tokens += other.tokens;
    }

    pub fn cost_usd(&self) -> f64 {
        self.cost_micros as f64 / 1_000_000.
    }
}

/// OpenCode sessions updated today, and in the last seven days.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpencodeSpend {
    pub today: Spend,
    pub week: Spend,
}

/// Local midnight `days_back` days before `now`, in Unix milliseconds.
fn local_midnight_ms(now: SystemTime, days_back: i64) -> Option<i64> {
    let now = chrono::DateTime::<chrono::Local>::from(now);
    let day = now.date_naive() - chrono::Duration::days(days_back);
    let midnight = day.and_hms_opt(0, 0, 0)?;
    let local = midnight.and_local_timezone(chrono::Local).earliest()?;
    Some(local.timestamp_millis())
}

/// What one OpenCode store said, and the versions of the files it said it of.
#[derive(Clone)]
struct OpencodeScan {
    stamp: (Option<SystemTime>, Option<SystemTime>),
    /// The local day the figures were counted relative to.
    today: NaiveDate,
    spend: OpencodeSpend,
    days: BTreeMap<NaiveDate, u64>,
}

/// Per home. Opening a store whose write-ahead log has grown can take many
/// seconds, far worse over a WSL share, so an unchanged store is not reopened.
static OPENCODE_SCANS: LazyLock<Mutex<HashMap<PathBuf, OpencodeScan>>> =
    LazyLock::new(Default::default);

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

fn opencode_scan(home: &Path, now: SystemTime) -> Option<OpencodeScan> {
    let db = super::opencode::opencode_db(home);
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    let stamp = (modified(&db), modified(Path::new(&wal)));
    stamp.0?;
    let today = chrono::DateTime::<chrono::Local>::from(now).date_naive();
    let mut scans = OPENCODE_SCANS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(scan) = scans.get(home) {
        if scan.stamp == stamp && scan.today == today {
            return Some(scan.clone());
        }
    }
    let today_ms = local_midnight_ms(now, 0)?;
    let week_ms = local_midnight_ms(now, 6)?;
    let month_ms = local_midnight_ms(now, HISTORY_DAYS as i64 - 1)?;
    let (spend, sessions) =
        super::opencode::spend_and_sessions_since(home, &[today_ms, week_ms], month_ms)?;
    let mut days: BTreeMap<NaiveDate, u64> = BTreeMap::new();
    for (updated_ms, tokens) in sessions {
        if let Some(at) = chrono::DateTime::from_timestamp_millis(updated_ms) {
            *days
                .entry(at.with_timezone(&chrono::Local).date_naive())
                .or_default() += tokens;
        }
    }
    let scan = OpencodeScan {
        stamp,
        today,
        spend: OpencodeSpend {
            today: Spend::from_store(*spend.first()?),
            week: Spend::from_store(*spend.get(1)?),
        },
        days,
    };
    scans.insert(home.to_path_buf(), scan.clone());
    Some(scan)
}

impl UsageHistory {
    /// Each of the `count` days ending with `today`, oldest first, per
    /// vendor.
    pub fn series_split(&self, today: NaiveDate, count: usize) -> Vec<DayTokens> {
        (0..count)
            .rev()
            .map(|back| {
                today
                    .checked_sub_signed(chrono::Duration::days(back as i64))
                    .and_then(|day| self.days.get(&day).copied())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Tokens on each of the `count` days ending with `today`, oldest first.
    pub fn series(&self, today: NaiveDate, count: usize) -> Vec<u64> {
        self.series_split(today, count)
            .iter()
            .map(DayTokens::total)
            .collect()
    }

    /// Tokens over the `count` days ending with `today`.
    pub fn total(&self, today: NaiveDate, count: usize) -> u64 {
        self.series(today, count).iter().sum()
    }
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
    /// Gemini: the day and tokens of each message, as last listed.
    gemini_messages: HashMap<MessageKey, (NaiveDate, u64)>,
    /// Copilot: tokens per day.
    copilot_days: BTreeMap<NaiveDate, u64>,
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
    // Keeps the Codex quota known after its rollouts age out of the herd's
    // 15-minute window; the store keeps whichever sample is newest.
    super::quota::record_codex_line(line);
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

#[derive(Deserialize)]
struct GeminiMessage {
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    tokens: Option<GeminiTokens>,
}

#[derive(Deserialize)]
struct GeminiTokens {
    input: Option<u64>,
    cached: Option<u64>,
    output: Option<u64>,
    thoughts: Option<u64>,
    tool: Option<u64>,
}

#[derive(Deserialize)]
struct GeminiLine {
    #[serde(rename = "$set")]
    set: Option<GeminiCheckpoint>,
}

#[derive(Deserialize)]
struct GeminiCheckpoint {
    messages: Option<Vec<GeminiMessage>>,
}

fn fold_gemini_message(
    message: GeminiMessage,
    state: &mut FileState,
    day_of: &dyn Fn(&str) -> Option<NaiveDate>,
) {
    if message.kind.as_deref() != Some("gemini") {
        return;
    }
    let GeminiMessage {
        id,
        tokens,
        timestamp,
        ..
    } = message;
    let (Some(id), Some(tokens)) = (id, tokens) else {
        return;
    };
    let counted = tokens
        .input
        .unwrap_or(0)
        .saturating_sub(tokens.cached.unwrap_or(0))
        + tokens.tool.unwrap_or(0)
        + tokens.output.unwrap_or(0)
        + tokens.thoughts.unwrap_or(0);
    let key = message_key(&id);
    // Keeps the day a repeat was first seen, as Claude's messages do.
    let day = state
        .gemini_messages
        .get(&key)
        .map(|(day, _)| *day)
        .or_else(|| timestamp.as_deref().and_then(day_of));
    if let Some(day) = day {
        state.gemini_messages.insert(key, (day, counted));
    }
}

fn fold_gemini(line: &str, state: &mut FileState, day_of: &dyn Fn(&str) -> Option<NaiveDate>) {
    if !line.contains("\"tokens\"") {
        return;
    }
    if let Ok(GeminiLine {
        set: Some(GeminiCheckpoint {
            messages: Some(messages),
        }),
    }) = serde_json::from_str::<GeminiLine>(line)
    {
        for message in messages {
            fold_gemini_message(message, state, day_of);
        }
        return;
    }
    if let Ok(message) = serde_json::from_str::<GeminiMessage>(line) {
        fold_gemini_message(message, state, day_of);
    }
}

#[derive(Deserialize)]
struct CopilotEvent {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    data: Option<CopilotShutdown>,
}

#[derive(Deserialize)]
struct CopilotShutdown {
    #[serde(rename = "modelMetrics")]
    model_metrics: Option<HashMap<String, CopilotModelMetrics>>,
}

#[derive(Deserialize)]
struct CopilotModelMetrics {
    usage: Option<CopilotUsage>,
}

#[derive(Deserialize)]
struct CopilotUsage {
    #[serde(rename = "inputTokens")]
    input: Option<u64>,
    #[serde(rename = "outputTokens")]
    output: Option<u64>,
    #[serde(rename = "cacheWriteTokens")]
    cache_write: Option<u64>,
}

fn fold_copilot(line: &str, state: &mut FileState, day_of: &dyn Fn(&str) -> Option<NaiveDate>) {
    if !line.contains("\"session.shutdown\"") {
        return;
    }
    let Ok(event) = serde_json::from_str::<CopilotEvent>(line) else {
        return;
    };
    if event.kind.as_deref() != Some("session.shutdown") {
        return;
    }
    let Some(metrics) = event.data.and_then(|data| data.model_metrics) else {
        return;
    };
    let tokens: u64 = metrics
        .values()
        .filter_map(|model| model.usage.as_ref())
        .map(|usage| {
            usage.input.unwrap_or(0) + usage.cache_write.unwrap_or(0) + usage.output.unwrap_or(0)
        })
        .sum();
    if tokens == 0 {
        return;
    }
    if let Some(day) = event.timestamp.as_deref().and_then(day_of) {
        *state.copilot_days.entry(day).or_default() += tokens;
    }
}

fn fold_line(
    format: HistoryFormat,
    line: &str,
    state: &mut FileState,
    day_of: &dyn Fn(&str) -> Option<NaiveDate>,
) {
    match format {
        HistoryFormat::Claude => fold_claude(line, state, day_of),
        HistoryFormat::Codex => fold_codex(line, state, day_of),
        HistoryFormat::Gemini => fold_gemini(line, state, day_of),
        HistoryFormat::Copilot => fold_copilot(line, state, day_of),
    }
}

/// Fold one file's per-message figures into `into`: a message present in
/// several files is counted once, at its largest usage, on the earliest day
/// any file saw it.
fn merge_messages(
    into: &mut HashMap<MessageKey, (NaiveDate, u64)>,
    messages: &HashMap<MessageKey, (NaiveDate, u64)>,
) {
    for (key, (day, tokens)) in messages {
        into.entry(*key)
            .and_modify(|(seen_day, seen_tokens)| {
                *seen_day = (*seen_day).min(*day);
                *seen_tokens = (*seen_tokens).max(*tokens);
            })
            .or_insert((*day, *tokens));
    }
}

/// The per-day totals over a set of files' states.
fn merge<'a>(states: impl Iterator<Item = &'a FileState>) -> UsageHistory {
    let mut history = UsageHistory::default();
    let mut claude: HashMap<MessageKey, (NaiveDate, u64)> = HashMap::new();
    let mut gemini: HashMap<MessageKey, (NaiveDate, u64)> = HashMap::new();
    for state in states {
        for (day, tokens) in &state.days {
            history.days.entry(*day).or_default().codex += tokens;
        }
        for (day, tokens) in &state.copilot_days {
            history.days.entry(*day).or_default().copilot += tokens;
        }
        merge_messages(&mut claude, &state.messages);
        merge_messages(&mut gemini, &state.gemini_messages);
    }
    for (day, tokens) in claude.into_values() {
        history.days.entry(day).or_default().claude += tokens;
    }
    for (day, tokens) in gemini.into_values() {
        history.days.entry(day).or_default().gemini += tokens;
    }
    history.days.retain(|_, tokens| tokens.total() > 0);
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
fn read_file(path: &Path, format: HistoryFormat, state: &mut FileState) -> std::io::Result<bool> {
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

/// Tokens per day over the last [`HISTORY_DAYS`], across every transcript
/// under each of `roots` that was written to in that time.
///
/// The first call reads those transcripts whole; later calls stat each one
/// and read only what was appended, which for an idle transcript is nothing.
///
/// Blocking file I/O: call it off the GUI thread.
pub fn scan_history(roots: &[SessionRoot], now: SystemTime) -> UsageHistory {
    let since = now
        .checked_sub(Duration::from_secs(HISTORY_DAYS as u64 * 24 * 60 * 60))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut opencode: Option<OpencodeSpend> = None;
    let mut opencode_days: BTreeMap<NaiveDate, u64> = BTreeMap::new();
    for root in roots {
        let home = &root.home;
        let codex = home.join(".codex");
        let sources = [
            (home.join(".claude").join("projects"), HistoryFormat::Claude),
            (codex.join("sessions"), HistoryFormat::Codex),
            (codex.join("archived_sessions"), HistoryFormat::Codex),
            (home.join(".gemini").join("tmp"), HistoryFormat::Gemini),
            (
                home.join(".copilot").join("session-state"),
                HistoryFormat::Copilot,
            ),
        ];
        for (source, format) in sources.iter() {
            for path in recent_transcripts(source, since) {
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
        if let Some(scan) = opencode_scan(home, now) {
            let total = opencode.get_or_insert_with(Default::default);
            total.today.add(&scan.spend.today);
            total.week.add(&scan.spend.week);
            for (day, tokens) in scan.days {
                *opencode_days.entry(day).or_default() += tokens;
            }
        }
    }
    let cache = {
        let mut cache = cache();
        // Transcripts that aged out of the window are not read again. Those
        // under a root not scanned this time are kept: a WSL distro that
        // stopped still counts, and is not read whole when it starts again.
        cache.retain(|path, _| {
            seen.contains(path) || !roots.iter().any(|root| path.starts_with(&root.home))
        });
        merge(cache.values())
    };
    let mut history = UsageHistory { opencode, ..cache };
    for (day, tokens) in opencode_days {
        if tokens > 0 {
            history.days.entry(day).or_default().opencode += tokens;
        }
    }
    history
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan_history` folds every cached transcript, including those under
    /// roots it was not given, so tests that scan take turns on an empty cache.
    fn scan_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache().clear();
        guard
    }

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

    fn state(format: HistoryFormat, lines: &[String]) -> FileState {
        let mut state = FileState::default();
        for line in lines {
            fold_line(format, line, &mut state, &utc_day);
        }
        state
    }

    fn totals(history: UsageHistory) -> BTreeMap<NaiveDate, u64> {
        history
            .days
            .into_iter()
            .map(|(day, tokens)| (day, tokens.total()))
            .collect()
    }

    fn fold(format: HistoryFormat, lines: &[String]) -> BTreeMap<NaiveDate, u64> {
        totals(merge(std::iter::once(&state(format, lines))))
    }

    #[test]
    fn claude_messages_land_on_their_day_once() {
        let days = fold(
            HistoryFormat::Claude,
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
            HistoryFormat::Claude,
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
            HistoryFormat::Claude,
            &[claude("2026-10-05T10:00:00Z", "m1", 10, 0, 90)],
        );
        let resumed = state(
            HistoryFormat::Claude,
            &[
                claude("2026-10-06T08:00:00Z", "m1", 10, 0, 90),
                claude("2026-10-06T08:05:00Z", "m2", 5, 0, 5),
            ],
        );
        let days = totals(merge(vec![&original, &resumed].into_iter()));
        assert_eq!(days.get(&day("2026-10-05")), Some(&100));
        assert_eq!(days.get(&day("2026-10-06")), Some(&10));
    }

    #[test]
    fn codex_days_get_the_growth_of_the_running_total() {
        let days = fold(
            HistoryFormat::Codex,
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
            HistoryFormat::Codex,
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
            HistoryFormat::Claude,
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
        assert!(fold(HistoryFormat::Codex, &[rate_limits]).is_empty());
    }

    #[test]
    fn each_vendor_keeps_its_own_share_of_a_day() {
        let claude = state(
            HistoryFormat::Claude,
            &[claude("2026-10-05T09:00:00Z", "m1", 10, 0, 30)],
        );
        let codex = state(
            HistoryFormat::Codex,
            &[
                serde_json::json!({"type": "response_item", "payload": {"role": "assistant"}})
                    .to_string(),
                codex("2026-10-05T10:00:00Z", 100, 0, 20),
            ],
        );
        let history = merge(vec![&claude, &codex].into_iter());
        assert_eq!(
            history.days.get(&day("2026-10-05")),
            Some(&DayTokens {
                claude: 40,
                codex: 120,
                ..Default::default()
            })
        );
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
            .map(|(day, claude)| {
                (
                    day,
                    DayTokens {
                        claude,
                        ..Default::default()
                    },
                )
            })
            .collect(),
            ..Default::default()
        };
        let today = day("2026-10-07");
        assert_eq!(history.series(today, 3), vec![10, 0, 3]);
        assert_eq!(history.total(today, 1), 3);
        assert_eq!(history.total(today, 7), 18);
        assert_eq!(history.total(today, 3), 13);
    }

    #[test]
    fn scan_reads_both_vendors_and_ignores_stale_files() {
        let _lock = scan_lock();
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

        let history = scan_history(&[SessionRoot::host(home.path())], now);
        assert_eq!(
            history.days.get(&today).map(DayTokens::total),
            Some(15 + 2 + 50 + 950)
        );
        // A second scan reads nothing new and reaches the same answer.
        assert_eq!(
            scan_history(&[SessionRoot::host(home.path())], now),
            history
        );

        // Scanning from a point after every file's window finds nothing.
        let later = now + Duration::from_secs((HISTORY_DAYS as u64 + 1) * 24 * 60 * 60);
        assert!(scan_history(&[SessionRoot::host(home.path())], later)
            .days
            .is_empty());
    }

    fn jsonl(path: PathBuf, lines: &[String]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn gemini_counts_a_repeated_message_once_and_cached_input_not_at_all() {
        let mut state = FileState::default();
        let message = serde_json::json!({
            "id": "g1", "type": "gemini", "timestamp": "2026-10-07T10:00:00.000Z",
            "tokens": {"input": 12000, "output": 40, "cached": 9000, "thoughts": 300,
                       "tool": 0, "total": 12340},
        });
        fold_gemini(&message.to_string(), &mut state, &utc_day);
        fold_gemini(&message.to_string(), &mut state, &utc_day);
        // The same message re-listed by a checkpoint.
        let checkpoint = serde_json::json!({"$set": {"messages": [message]}});
        fold_gemini(&checkpoint.to_string(), &mut state, &utc_day);
        // A prompt carries no tokens of its own.
        fold_gemini(
            r#"{"id":"u1","type":"user","timestamp":"2026-10-07T10:00:00.000Z"}"#,
            &mut state,
            &utc_day,
        );
        let history = merge(std::iter::once(&state));
        assert_eq!(history.days[&day("2026-10-07")].gemini, 3_340);
    }

    #[test]
    fn copilot_sums_each_shutdown_across_its_models() {
        let mut state = FileState::default();
        let shutdown = |input: u64| {
            serde_json::json!({
                "type": "session.shutdown",
                "timestamp": "2026-10-07T10:00:00.000Z",
                "data": {"modelMetrics": {
                    "a": {"usage": {"inputTokens": input, "outputTokens": 5,
                                    "cacheReadTokens": 1000, "cacheWriteTokens": 2}},
                    "b": {"usage": {"inputTokens": 1, "outputTokens": 1}},
                }},
            })
            .to_string()
        };
        fold_copilot(&shutdown(10), &mut state, &utc_day);
        fold_copilot(&shutdown(20), &mut state, &utc_day);
        fold_copilot(
            r#"{"type":"assistant.message","data":{"outputTokens":99}}"#,
            &mut state,
            &utc_day,
        );
        let history = merge(std::iter::once(&state));
        assert_eq!(history.days[&day("2026-10-07")].copilot, 19 + 29);
    }

    #[test]
    fn scan_sums_its_roots_and_keeps_one_that_was_not_scanned() {
        let _lock = scan_lock();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let stamp = chrono::DateTime::<chrono::Utc>::from(now).to_rfc3339();
        let today = local_day(&stamp).unwrap();
        jsonl(
            first.path().join(".claude/projects/-r/a.jsonl"),
            &[claude(&stamp, "roots-1", 10, 0, 5)],
        );
        jsonl(
            second.path().join(".claude/projects/-r/a.jsonl"),
            &[claude(&stamp, "roots-2", 20, 0, 5)],
        );
        let roots = [
            SessionRoot::host(first.path()),
            SessionRoot::wsl(second.path().to_path_buf(), "Ubuntu"),
        ];
        assert_eq!(scan_history(&roots, now).days[&today].claude, 15 + 25);
        // The distro stopped: its figures stay.
        assert_eq!(scan_history(&roots[..1], now).days[&today].claude, 15 + 25);
    }
}
