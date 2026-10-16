//! Background agents a Claude session launched and has not yet been told
//! have finished.
//!
//! Claude Code launches subagents in the background by default. The parent
//! then signs off its own turn at once (`end_turn`), its screen shows the
//! prompt, and for as long as the agents run nothing about the parent says
//! work is going on -- so the row used to go quiet in the middle of it. The
//! subagent's own transcript is no help either: it is silent for the whole
//! of a long tool call, a build or a test run.
//!
//! What does say so is the pair of records Claude itself keeps its count of
//! pending background work from (`pendingAgents` in its UI):
//!
//! - launched: a `user` record whose `toolUseResult` has
//!   `"status": "async_launched"` and the agent's `agentId`;
//! - delivered: a `user` record whose `origin.kind` is `"task-notification"`,
//!   carrying `<task-id>…</task-id>` in its content, written when the result
//!   is handed back to the parent.
//!
//! Pending = launched − delivered. Read incrementally: only what was
//! appended since the last scan is parsed (see `incremental`).
//!
//! Blocking file I/O: call it off the GUI thread.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Transcripts whose state is kept; the least recently read goes first.
const CACHE_CAP: usize = 64;

#[derive(Default)]
struct BackgroundState {
    launched: HashSet<String>,
    delivered: HashSet<String>,
}

impl BackgroundState {
    fn pending(&self) -> HashSet<String> {
        self.launched.difference(&self.delivered).cloned().collect()
    }
}

#[derive(Default)]
struct CacheEntry {
    /// Bytes consumed so far: always just past a complete line.
    offset: u64,
    state: BackgroundState,
    last_read: Option<Instant>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, CacheEntry>>> = LazyLock::new(Default::default);

/// The text of a record's message content, whether a plain string or a list
/// of text blocks.
fn message_text(value: &serde_json::Value) -> Option<String> {
    let content = value.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let blocks = content.as_array()?;
    Some(
        blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// `<task-id>…</task-id>` out of a task notification.
fn task_id(text: &str) -> Option<&str> {
    let start = text.find("<task-id>")? + "<task-id>".len();
    let len = text[start..].find("</task-id>")?;
    let id = text[start..start + len].trim();
    (!id.is_empty()).then_some(id)
}

fn fold_record(line: &str, state: &mut BackgroundState) {
    // Nearly every record is neither; skip the JSON parse for those.
    let launch = line.contains("async_launched");
    let notification = line.contains("task-notification");
    if !launch && !notification {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if launch {
        let result = value.get("toolUseResult");
        let launched = result
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
            == Some("async_launched");
        if let Some(agent_id) = result
            .filter(|_| launched)
            .and_then(|r| r.get("agentId"))
            .and_then(|id| id.as_str())
        {
            state.launched.insert(agent_id.to_string());
        }
    }
    if notification {
        let is_notification = value
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(|k| k.as_str())
            == Some("task-notification");
        if is_notification {
            if let Some(id) = message_text(&value).as_deref().and_then(task_id) {
                state.delivered.insert(id.to_string());
            }
        }
    }
}

fn read_pending(transcript: &Path) -> std::io::Result<HashSet<String>> {
    let len = std::fs::metadata(transcript)?.len();
    // Taken out for the duration of the read so the lock is not held across
    // file I/O.
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
    // No cap: a launch near the start of a long session that is still
    // running would otherwise be skipped. Only the first read is long.
    super::incremental::read_appended(transcript, len, &mut entry.offset, u64::MAX, &mut |line| {
        fold_record(line, state)
    })?;
    entry.last_read = Some(Instant::now());
    let pending = entry.state.pending();

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
    Ok(pending)
}

/// Agent ids of the background agents the session behind `transcript`
/// launched and has not been handed the result of. Empty when the
/// transcript cannot be read.
///
/// A transcript is someone else's file: a parser bug on one of them must
/// cost that row its pending count, not the whole scan.
pub fn pending_agents(transcript: &Path) -> HashSet<String> {
    match std::panic::catch_unwind(|| read_pending(transcript)) {
        Ok(Ok(pending)) => pending,
        Ok(Err(_)) => HashSet::new(),
        Err(_) => {
            log::debug!("background agents: transcript reader panicked; skipping");
            HashSet::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // The record shapes Claude Code 2.1.301 writes, trimmed to the fields
    // that matter.
    fn launch(agent_id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":[{{"type":"text","text":"Async agent launched successfully."}}]}}]}},"toolUseResult":{{"isAsync":true,"status":"async_launched","agentId":"{agent_id}","description":"Explore"}}}}"#
        )
    }

    fn notification(agent_id: &str) -> String {
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":"<task-notification>\n<task-id>{agent_id}</task-id>\n<status>completed</status>\n</task-notification>"}},"origin":{{"kind":"task-notification","producer":"session-task"}}}}"#
        )
    }

    fn write_lines(file: &mut std::fs::File, lines: &[String]) {
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn pending_is_launched_minus_delivered() {
        let mut state = BackgroundState::default();
        for line in [launch("a1"), launch("a2"), notification("a1")] {
            fold_record(&line, &mut state);
        }
        assert_eq!(state.pending(), HashSet::from(["a2".to_string()]));
    }

    #[test]
    fn a_queued_notification_or_a_mention_is_not_a_delivery() {
        let mut state = BackgroundState::default();
        fold_record(&launch("a1"), &mut state);
        // The queue record that precedes delivery has no `origin`.
        fold_record(
            r#"{"type":"queue-operation","operation":"enqueue","content":"<task-notification><task-id>a1</task-id></task-notification>"}"#,
            &mut state,
        );
        // Prose that talks about notifications is not one.
        fold_record(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"waiting for the task-notification of a1, status async_launched"}]}}"#,
            &mut state,
        );
        assert_eq!(state.pending(), HashSet::from(["a1".to_string()]));
    }

    #[test]
    fn reads_only_what_was_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        write_lines(&mut file, &[launch("b1"), launch("b2")]);
        assert_eq!(pending_agents(&path).len(), 2);

        write_lines(&mut file, &[notification("b2")]);
        assert_eq!(pending_agents(&path), HashSet::from(["b1".to_string()]));

        // A rewrite (the file shrank) is folded again from the start.
        let mut file = std::fs::File::create(&path).unwrap();
        write_lines(&mut file, &[launch("c1")]);
        assert_eq!(pending_agents(&path), HashSet::from(["c1".to_string()]));
    }

    #[test]
    fn a_missing_transcript_has_nothing_pending() {
        assert!(pending_agents(Path::new("/nonexistent/session.jsonl")).is_empty());
    }
}
