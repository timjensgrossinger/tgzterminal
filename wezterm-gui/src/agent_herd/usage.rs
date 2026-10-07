//! Token usage and model of a session, read from its transcript.
//!
//! Only the vendors whose transcripts record usage are covered:
//!
//! * Claude writes one record per content block of an assistant message, each
//!   repeating that message's `usage`, and an early one can carry a partial
//!   output count. Usage is therefore kept per message id, at its largest
//!   report, and summed across ids; summing records would count a message
//!   once per block.
//! * Codex writes `token_count` events whose `total_token_usage` is already
//!   the session total, so the latest one wins and nothing is summed.
//!
//! "Input" here is what the session sent that was not served from the prompt
//! cache: a long session re-reads its whole cached context on every turn, and
//! counting those reads would bury the number under them. For Claude that is
//! almost entirely cache writes: its transcripts record the uncached
//! `input_tokens` as a placeholder of a few tokens.
//!
//! These are the figures the transcripts hold, not a bill. In particular a
//! Claude transcript's output count is widely reported to leave out thinking
//! tokens.
//!
//! The read is incremental (see [`super::incremental`]): the first call folds
//! the whole file, later calls only what was appended. Vendor files are
//! undocumented internals, so every field is optional and a record that does
//! not parse is skipped.
//!
//! Pure apart from the file read; no GUI, mux or terminal types.

use serde::Deserialize;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Transcripts whose read state is kept. Past this the least recently read
/// one is dropped, and would be folded from the start if it came back.
const CACHE_CAP: usize = 64;

/// Which vendor's record shapes to look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageFormat {
    Claude,
    Codex,
}

/// What a transcript says about its session so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub model: Option<String>,
    /// Tokens sent that were not read from the prompt cache.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// When the transcript's first record was written.
    pub started_at: Option<SystemTime>,
}

#[derive(Clone, Debug, Default)]
struct UsageState {
    /// Claude: `(input, output)` per message id, as last reported.
    per_message: HashMap<String, (u64, u64)>,
    input: u64,
    output: u64,
    /// Whether any usage record was seen: zero tokens and no record differ.
    seen: bool,
    model: Option<String>,
    started_at: Option<SystemTime>,
}

impl UsageState {
    fn totals(&self) -> UsageTotals {
        UsageTotals {
            model: self.model.clone(),
            input_tokens: self.seen.then_some(self.input),
            output_tokens: self.seen.then_some(self.output),
            started_at: self.started_at,
        }
    }
}

#[derive(Deserialize)]
struct ClaudeRecord {
    message: Option<ClaudeMessage>,
}

#[derive(Deserialize)]
struct ClaudeMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn fold_claude_record(line: &str, state: &mut UsageState) {
    // Most records are user turns and tool results; skip them unparsed.
    if !line.contains("\"usage\"") {
        return;
    }
    let Ok(record) = serde_json::from_str::<ClaudeRecord>(line) else {
        return;
    };
    let Some(message) = record.message else {
        return;
    };
    // `<synthetic>` marks a message the client made up (an API error notice).
    if let Some(model) = message
        .model
        .filter(|m| !m.is_empty() && !m.starts_with('<'))
    {
        state.model = Some(model);
    }
    let (Some(id), Some(usage)) = (message.id, message.usage) else {
        return;
    };
    let input = usage.input_tokens.unwrap_or(0) + usage.cache_creation_input_tokens.unwrap_or(0);
    let output = usage.output_tokens.unwrap_or(0);
    // An early record of a message can carry a partial output count, and a
    // later one can repeat it, so the largest report is the one kept.
    let (old_input, old_output) = state.per_message.get(&id).copied().unwrap_or((0, 0));
    if state.per_message.contains_key(&id) && input + output <= old_input + old_output {
        return;
    }
    state.per_message.insert(id, (input, output));
    state.input = state.input - old_input + input;
    state.output = state.output - old_output + output;
    state.seen = true;
}

#[derive(Deserialize)]
struct CodexRecord {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
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

fn fold_codex_record(line: &str, state: &mut UsageState) {
    // The first record dates the session; after that only two kinds matter.
    let wanted = state.started_at.is_none()
        || line.contains("\"token_count\"")
        || line.contains("\"turn_context\"");
    if !wanted {
        return;
    }
    let Ok(record) = serde_json::from_str::<CodexRecord>(line) else {
        return;
    };
    if state.started_at.is_none() {
        state.started_at = record
            .timestamp
            .as_deref()
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .and_then(|at| u64::try_from(at.timestamp_millis()).ok())
            .map(|millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis));
    }
    let Some(payload) = record.payload else {
        return;
    };
    if record.kind.as_deref() == Some("turn_context") {
        if let Some(model) = payload.model.filter(|m| !m.is_empty()) {
            state.model = Some(model);
        }
        return;
    }
    if payload.kind.as_deref() != Some("token_count") {
        return;
    }
    // `info` is null on the events that only report rate limits.
    let Some(total) = payload.info.and_then(|info| info.total_token_usage) else {
        return;
    };
    // Codex counts cached tokens inside `input_tokens`.
    state.input = total
        .input_tokens
        .unwrap_or(0)
        .saturating_sub(total.cached_input_tokens.unwrap_or(0));
    state.output = total.output_tokens.unwrap_or(0);
    state.seen = true;
}

fn fold_record(format: UsageFormat, line: &str, state: &mut UsageState) {
    match format {
        UsageFormat::Claude => fold_claude_record(line, state),
        UsageFormat::Codex => fold_codex_record(line, state),
    }
}

#[derive(Default)]
struct CacheEntry {
    /// Bytes consumed so far: always just past a complete line.
    offset: u64,
    state: UsageState,
    last_read: Option<Instant>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, CacheEntry>>> = LazyLock::new(Default::default);

fn read_totals(transcript: &Path, format: UsageFormat) -> std::io::Result<UsageTotals> {
    let len = std::fs::metadata(transcript)?.len();
    // Taken out for the duration of the read so the lock is not held across
    // file I/O. A second reader of the same path in that window starts from
    // zero and reaches the same totals.
    let mut entry = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(transcript)
        .unwrap_or_default();
    if entry.offset > len {
        // The file shrank: it was rewritten, so fold it again from the start.
        entry = CacheEntry::default();
    }
    let state = &mut entry.state;
    // No cap: a total that left out the start of a long session would be
    // wrong, and only the first read of a transcript is ever a long one.
    super::incremental::read_appended(transcript, len, &mut entry.offset, u64::MAX, &mut |line| {
        fold_record(format, line, state)
    })?;
    entry.last_read = Some(Instant::now());
    let totals = entry.state.totals();

    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= CACHE_CAP {
        let oldest = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_read)
            .map(|(path, _)| path.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    cache.insert(transcript.to_path_buf(), entry);
    Ok(totals)
}

/// Usage for the session behind `transcript`, or `None` when it cannot be
/// read.
///
/// A transcript is someone else's file: a parser bug on one of them must cost
/// that row its token count, not the whole scan.
///
/// Blocking file I/O: call it off the GUI thread.
pub fn totals_for(transcript: &Path, format: UsageFormat) -> Option<UsageTotals> {
    let read = std::panic::catch_unwind(|| read_totals(transcript, format));
    match read {
        Ok(Ok(totals)) => Some(totals),
        Ok(Err(_)) => None,
        Err(_) => {
            log::debug!("usage: transcript reader panicked; skipping its token count");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn claude_line(id: &str, model: &str, input: u64, cache_write: u64, output: u64) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": id,
                "model": model,
                "content": [{ "type": "text", "text": "…" }],
                "usage": {
                    "input_tokens": input,
                    "cache_creation_input_tokens": cache_write,
                    "cache_read_input_tokens": 900_000,
                    "output_tokens": output,
                },
            },
        })
        .to_string()
    }

    fn codex_tokens(input: u64, cached: u64, output: u64) -> String {
        serde_json::json!({
            "timestamp": "2026-08-30T00:20:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                    },
                    "last_token_usage": { "input_tokens": 1, "output_tokens": 1 },
                },
            },
        })
        .to_string()
    }

    fn fold_all(format: UsageFormat, lines: &[String]) -> UsageTotals {
        let mut state = UsageState::default();
        for line in lines {
            fold_record(format, line, &mut state);
        }
        state.totals()
    }

    #[test]
    fn claude_counts_each_message_once_however_many_records_repeat_it() {
        let totals = fold_all(
            UsageFormat::Claude,
            &[
                claude_line("msg_1", "claude-opus", 10, 100, 50),
                claude_line("msg_1", "claude-opus", 10, 100, 50),
                claude_line("msg_1", "claude-opus", 10, 100, 50),
                claude_line("msg_2", "claude-opus", 5, 0, 20),
            ],
        );
        // Cache reads (900k per record) are not part of "input".
        assert_eq!(totals.input_tokens, Some(115));
        assert_eq!(totals.output_tokens, Some(70));
        assert_eq!(totals.model.as_deref(), Some("claude-opus"));
    }

    #[test]
    fn claude_takes_the_largest_usage_for_a_message() {
        let totals = fold_all(
            UsageFormat::Claude,
            &[
                claude_line("msg_1", "m", 10, 0, 1),
                claude_line("msg_1", "m", 10, 0, 400),
                claude_line("msg_1", "m", 10, 0, 1),
            ],
        );
        assert_eq!(totals.input_tokens, Some(10));
        assert_eq!(totals.output_tokens, Some(400));
    }

    #[test]
    fn claude_ignores_records_without_usage_and_synthetic_models() {
        let user = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": "what is my \"usage\" so far?" },
        })
        .to_string();
        let totals = fold_all(
            UsageFormat::Claude,
            &[
                user,
                "not json at all \"usage\"".to_string(),
                claude_line("msg_1", "claude-opus", 1, 0, 2),
                claude_line("msg_2", "<synthetic>", 0, 0, 0),
            ],
        );
        assert_eq!(totals.input_tokens, Some(1));
        assert_eq!(totals.output_tokens, Some(2));
        assert_eq!(totals.model.as_deref(), Some("claude-opus"));
    }

    #[test]
    fn no_usage_record_means_unknown_not_zero() {
        let totals = fold_all(UsageFormat::Claude, &["{\"type\":\"user\"}".to_string()]);
        assert_eq!(totals, UsageTotals::default());
    }

    #[test]
    fn codex_total_is_the_latest_event_not_a_sum() {
        let meta = serde_json::json!({
            "timestamp": "2026-08-30T00:17:59.000Z",
            "type": "session_meta",
            "payload": { "cwd": "/r" },
        })
        .to_string();
        let context = serde_json::json!({
            "timestamp": "2026-08-30T00:18:00.000Z",
            "type": "turn_context",
            "payload": { "model": "gpt-5-codex", "cwd": "/r" },
        })
        .to_string();
        let rate_limits_only = serde_json::json!({
            "timestamp": "2026-08-30T00:19:00.000Z",
            "type": "event_msg",
            "payload": { "type": "token_count", "info": null },
        })
        .to_string();
        let totals = fold_all(
            UsageFormat::Codex,
            &[
                meta,
                context,
                codex_tokens(1_000, 800, 50),
                codex_tokens(5_000, 4_000, 300),
                rate_limits_only,
            ],
        );
        assert_eq!(totals.input_tokens, Some(1_000));
        assert_eq!(totals.output_tokens, Some(300));
        assert_eq!(totals.model.as_deref(), Some("gpt-5-codex"));
        let started = totals
            .started_at
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        assert_eq!(started.as_secs(), 1_788_049_079);
    }

    #[test]
    fn totals_for_reads_incrementally_and_survives_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, format!("{}\n", claude_line("msg_1", "m", 10, 0, 5))).unwrap();
        let first = totals_for(&path, UsageFormat::Claude).unwrap();
        assert_eq!(first.input_tokens, Some(10));
        assert_eq!(first.output_tokens, Some(5));

        // Appended: a complete record and one still being written.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", claude_line("msg_2", "m", 3, 0, 7)).unwrap();
        write!(file, "{{\"message\":{{\"id\":\"msg_3\",\"usage\"").unwrap();
        let second = totals_for(&path, UsageFormat::Claude).unwrap();
        assert_eq!(second.input_tokens, Some(13));
        assert_eq!(second.output_tokens, Some(12));

        // Rewritten shorter: counted again from the start.
        std::fs::write(&path, format!("{}\n", claude_line("msg_9", "m", 1, 0, 1))).unwrap();
        let third = totals_for(&path, UsageFormat::Claude).unwrap();
        assert_eq!(third.input_tokens, Some(1));
        assert_eq!(third.output_tokens, Some(1));
    }

    #[test]
    fn totals_for_a_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            totals_for(&dir.path().join("nope.jsonl"), UsageFormat::Codex),
            None
        );
    }
}
