//! Reading "what is this agent doing" out of an agent's own JSONL transcript.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`. The interesting
//! entries are `type: "assistant"` with a `message.content` array holding
//! `tool_use` and `text` blocks; everything else (`user`, `system`,
//! `attachment`, and the bookkeeping types) says nothing about what the agent
//! is doing.
//!
//! Like the rest of [`crate::agent_herd`] this treats the format as
//! **undocumented internals**: every field is optional, every parse failure
//! degrades one event rather than the read, and an unreadable file simply
//! yields no activity.
//!
//! These files reach megabytes, so nothing here ever reads one whole: only a
//! bounded tail — or, for the session labels in [`super::sessions`], a bounded
//! head — is parsed, and callers are expected to skip the read entirely while
//! the file's `(mtime, len)` is unchanged.

use super::{HerdActivity, HerdContent, HerdEvent, HerdEventKind};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

/// How much of a transcript's tail to read on the first attempt.
pub const TAIL_WINDOW: u64 = 64 * 1024;

/// Hard ceiling on tail reading. A final assistant message routinely exceeds
/// the initial window — one very long JSONL line — so the window grows until it
/// contains what was asked for. This caps that growth so a pathological
/// transcript can never be slurped whole.
pub const MAX_TAIL_WINDOW: u64 = 4 * 1024 * 1024;

/// Longest tool summary or prose snippet kept per event. Renderers truncate
/// again to fit their column; this only stops a pathological line from being
/// carried around in memory.
const MAX_EVENT_TEXT: usize = 160;

/// Read the last `max_lines` complete lines of a possibly-huge line-delimited
/// file, oldest first.
///
/// Reads the final [`TAIL_WINDOW`] bytes and, when that window starts mid-file,
/// discards its leading partial line. The window grows (up to
/// [`MAX_TAIL_WINDOW`]) while it holds fewer lines than asked for and the start
/// of the file has not been reached.
///
/// Returns an empty vec only when the file is empty, unreadable, or its single
/// trailing line is larger than the ceiling. A final line with no terminating
/// newline is returned as is; callers parse it and degrade gracefully if the
/// writer was mid-append.
pub fn tail_lines(path: &Path, max_lines: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    if len == 0 {
        return Vec::new();
    }

    let mut window = TAIL_WINDOW;
    loop {
        let start = len.saturating_sub(window);
        if file.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::with_capacity((len - start) as usize);
        if file.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&buf);

        // Past the leading partial line, if we started mid-file.
        let usable: Option<&str> = if start > 0 {
            text.find('\n').map(|idx| &text[idx + 1..])
        } else {
            Some(&text[..])
        };

        let lines: Vec<String> = usable
            .map(|usable| {
                let mut lines: Vec<String> = usable
                    .lines()
                    .rev()
                    .filter(|line| !line.trim().is_empty())
                    .take(max_lines)
                    .map(str::to_string)
                    .collect();
                lines.reverse();
                lines
            })
            .unwrap_or_default();

        // Whole file read, or ceiling reached: this is as good as it gets.
        if start == 0 || window >= MAX_TAIL_WINDOW || lines.len() >= max_lines {
            return lines;
        }
        window = window.saturating_mul(8).min(MAX_TAIL_WINDOW);
    }
}

/// How many lines of a transcript's head a session label may cost.
///
/// Claude Code writes its own `ai-title` early but not at a fixed offset: across
/// the transcripts on hand the first one landed between lines 12 and 247, so the
/// budget is generous in *lines*. It deliberately is not a byte budget — a single
/// line holding a pasted file or a large tool result routinely runs past a
/// megabyte, which would starve a byte window long before the title.
pub const HEAD_LINES: usize = 300;

/// Hard ceiling on head reading, guarding the pathological case where the head
/// lines are themselves enormous.
pub const MAX_HEAD_BYTES: u64 = 4 * 1024 * 1024;

/// Longest session label kept, in words. Enough to read as a description and
/// short enough for a dropdown row.
const MAX_LABEL_WORDS: usize = 10;

/// Default minimum length for a usable description.
const MIN_LABEL_CHARS: usize = 8;

/// Read up to the first `max_lines` complete lines of a line-delimited file.
///
/// The mirror of [`tail_lines`] for readers that want a file's opening entries,
/// stopping at whichever of `max_lines` or [`MAX_HEAD_BYTES`] comes first. Blank
/// lines are skipped so they do not consume budget. A trailing line with no
/// newline is returned as is.
pub fn head_lines(path: &Path, max_lines: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    // Bounding the reader rather than the line count alone keeps one absurd line
    // from being materialized in full.
    let mut reader = BufReader::new(file.take(MAX_HEAD_BYTES));
    let mut lines = Vec::new();
    let mut buf = String::new();
    while lines.len() < max_lines {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            // Invalid UTF-8 anywhere in the window ends the read; the caller
            // falls back to whatever it collected.
            Err(_) => break,
        }
        let line = buf.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        lines.push(line.to_string());
    }
    lines
}

/// Reduce a raw agent prompt to a short one-line description.
///
/// Prompts arrive wrapped in machinery the user never typed: injected
/// `<system-reminder>` context, the `<local-command-caveat>` banner that heads a
/// resumed session, and slash commands rendered as
/// `<command-name>/plan</command-name><command-args>…</command-args>`. The args
/// of a slash command are the real ask, so they are unwrapped rather than
/// stripped; everything else tag-shaped is dropped.
///
/// Returns `None` when nothing usable survives, which is the signal to keep
/// looking at later messages.
///
/// The length floor exists because callers scanning a transcript need to skip
/// throwaway turns (`ok`, `yes`) and keep looking. Callers with exactly one
/// candidate should use [`describe_prompt_min`] with a floor of 1 instead —
/// there, a two-character prompt really is the best description available.
pub fn describe_prompt(text: &str) -> Option<String> {
    describe_prompt_min(text, MIN_LABEL_CHARS)
}

/// [`describe_prompt`] with an explicit minimum length.
pub fn describe_prompt_min(text: &str, min_chars: usize) -> Option<String> {
    let stripped = strip_tag_blocks(text, "system-reminder");
    let stripped = strip_tag_blocks(&stripped, "local-command-caveat");
    let unwrapped = match inner_text(&stripped, "command-args") {
        Some(args) => args,
        None => stripped,
    };
    // Anything still tag-shaped is machinery, not prose.
    let without_tags = strip_tags(&unwrapped);
    let collapsed = collapse_whitespace(&without_tags);
    let trimmed = trim_to_words(&collapsed, MAX_LABEL_WORDS);
    (trimmed.chars().count() >= min_chars).then_some(trimmed)
}

/// Shorten an already-clean description to `max_words`, appending an ellipsis
/// only when something was actually dropped.
pub fn trim_to_words(text: &str, max_words: usize) -> String {
    let mut words = text.split_whitespace();
    let kept: Vec<&str> = words.by_ref().take(max_words).collect();
    let mut out = kept.join(" ");
    if words.next().is_some() {
        out.push('…');
    }
    clamp(&out)
}

/// Remove every `<tag>…</tag>` block, including its contents.
///
/// An unterminated opening tag drops the rest of the text: a truncated injected
/// block is machinery too, and keeping its tail would put raw context into a
/// menu row.
fn strip_tag_blocks(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        match after.find(&close) {
            Some(end) => rest = &after[end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Contents of the first `<tag>…</tag>` pair, if both ends are present.
fn inner_text(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].to_string())
}

/// Drop anything that looks like a lone markup tag.
///
/// The length bound keeps this from eating ordinary prose that merely uses `<`
/// and `>` as comparisons — a real tag name is short.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        match after.find('>') {
            Some(end) if end <= 40 => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(&rest[..start + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Read what an agent has been doing, newest event last.
///
/// `max_events` bounds both the tail read and the returned log. Reading more
/// lines than events is deliberate: most transcript lines are `user` tool
/// results and bookkeeping, which produce nothing, so a 1:1 budget would
/// usually come back nearly empty.
pub fn read_activity(path: &Path, max_events: usize) -> HerdActivity {
    if max_events == 0 {
        return HerdActivity::default();
    }
    let mut events = Vec::new();
    for line in tail_lines(path, max_events.saturating_mul(4)) {
        events.extend(parse_line(&line));
    }
    if events.len() > max_events {
        events.drain(..events.len() - max_events);
    }

    // A tool call with nothing after it is the one still in flight; prose after
    // it means the agent already moved on, and claiming otherwise is how a
    // finished agent ends up looking busy.
    let current = events
        .last()
        .filter(|event| event.kind == HerdEventKind::Tool)
        .cloned();

    HerdActivity {
        current,
        recent: events,
        subagent_tree: Vec::new(),
    }
}

/// Read common agent transcript shapes when no vendor-specific parser exists.
///
/// Codex rollouts and OpenCode message files use different envelopes, but both
/// commonly expose `function_call`, `tool_use`, `parts`, `text`, or `content`.
/// This deliberately extracts only short display events; unknown records stay
/// invisible instead of polluting the insight row with raw protocol data.
pub fn read_generic_activity(path: &Path, max_events: usize) -> HerdActivity {
    if max_events == 0 {
        return HerdActivity::default();
    }
    let mut events = Vec::new();
    for line in tail_lines(path, max_events.saturating_mul(4)) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        collect_generic_events(&value, &mut events);
    }
    if events.len() > max_events {
        events.drain(..events.len() - max_events);
    }
    let current = events
        .last()
        .filter(|event| event.kind == HerdEventKind::Tool)
        .cloned();
    HerdActivity {
        current,
        recent: events,
        subagent_tree: Vec::new(),
    }
}

fn collect_generic_events(value: &serde_json::Value, events: &mut Vec<HerdEvent>) {
    let Some(object) = value.as_object() else {
        return;
    };
    let at = object
        .get("timestamp")
        .or_else(|| object.get("created_at"))
        .and_then(|value| value.as_str())
        .and_then(parse_timestamp);
    let record_type = object.get("type").and_then(|value| value.as_str());
    let tool_name = object
        .get("name")
        .or_else(|| object.get("tool_name"))
        .or_else(|| object.get("tool"))
        .and_then(|value| value.as_str());
    let arguments = object
        .get("input")
        .or_else(|| object.get("arguments"))
        .or_else(|| object.get("args"))
        .or_else(|| object.get("state").and_then(|state| state.get("input")));

    if matches!(
        record_type,
        Some("function_call" | "custom_tool_call" | "tool_use" | "tool")
    ) || (tool_name.is_some() && arguments.is_some())
    {
        events.push(HerdEvent {
            at,
            kind: HerdEventKind::Tool,
            content: HerdContent::ToolArgs {
                name: tool_name.unwrap_or("tool").to_string(),
                args: arguments.cloned().unwrap_or(serde_json::Value::Null),
            },
            tool_use_id: object
                .get("id")
                .or_else(|| object.get("call_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            parent_id: None,
        });
        return;
    }

    if let Some(command) = object.get("command").and_then(|value| value.as_str()) {
        events.push(HerdEvent {
            at,
            kind: HerdEventKind::Tool,
            content: HerdContent::ToolArgs {
                name: "shell".to_string(),
                args: serde_json::json!({ "command": command }),
            },
            tool_use_id: None,
            parent_id: None,
        });
        return;
    }

    if let Some(text) = generic_text(object) {
        events.push(HerdEvent {
            at,
            kind: HerdEventKind::Assistant,
            content: HerdContent::SingleLine(trim_to_event_text(&text)),
            tool_use_id: None,
            parent_id: None,
        });
        return;
    }

    for key in [
        "item", "items", "parts", "content", "message", "msg", "event", "payload",
    ] {
        match object.get(key) {
            Some(value) if value.is_array() => {
                for child in value.as_array().into_iter().flatten() {
                    collect_generic_events(child, events);
                }
            }
            Some(value) if value.is_object() => collect_generic_events(value, events),
            _ => {}
        }
    }
}

fn generic_text(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    for key in ["text", "message", "preview", "summary", "description"] {
        if let Some(text) = object.get(key).and_then(|value| value.as_str()) {
            if !text.trim().is_empty() {
                return Some(text.to_string());
            }
        }
    }
    object
        .get("content")
        .and_then(|value| value.as_str())
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
}

fn trim_to_event_text(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars().take(MAX_EVENT_TEXT).collect()
}

/// Turn one JSONL line into the events it describes, if any.
fn parse_line(line: &str) -> Vec<HerdEvent> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    let line_type = value.get("type").and_then(|v| v.as_str());

    match line_type {
        Some("assistant") => parse_assistant_line(&value),
        Some("user") => parse_user_line(&value),
        _ => Vec::new(),
    }
}

/// Parse an assistant JSONL line into events (tool_use, text, thinking).
fn parse_assistant_line(value: &serde_json::Value) -> Vec<HerdEvent> {
    let at = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp);

    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
    else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("tool_use") => {
                let tool_use_id = block.get("id").and_then(|v| v.as_str()).map(str::to_string);
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let input = block
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                events.push(HerdEvent {
                    at,
                    kind: HerdEventKind::Tool,
                    content: HerdContent::ToolArgs { name, args: input },
                    tool_use_id,
                    parent_id: None,
                });
            }
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                if text.lines().count() > 1 || text.len() > MAX_EVENT_TEXT {
                    if !text.trim().is_empty() {
                        events.push(HerdEvent {
                            at,
                            kind: HerdEventKind::Assistant,
                            content: HerdContent::MultiLine(text.to_string()),
                            tool_use_id: None,
                            parent_id: None,
                        });
                    }
                } else if let Some(snippet) = first_sentence(text) {
                    events.push(HerdEvent {
                        at,
                        kind: HerdEventKind::Assistant,
                        content: HerdContent::SingleLine(snippet),
                        tool_use_id: None,
                        parent_id: None,
                    });
                }
            }
            Some("thinking") => {
                let thinking_text = block
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                if !thinking_text.is_empty() {
                    events.push(HerdEvent {
                        at,
                        kind: HerdEventKind::Thinking,
                        content: HerdContent::MultiLine(thinking_text.to_string()),
                        tool_use_id: None,
                        parent_id: None,
                    });
                }
            }
            _ => {}
        }
    }
    events
}

/// Parse a user JSONL line for tool_result blocks.
fn parse_user_line(value: &serde_json::Value) -> Vec<HerdEvent> {
    let at = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp);

    let Some(blocks) = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
    else {
        return Vec::new();
    };

    let mut events = Vec::new();
    for block in blocks {
        if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
            continue;
        }

        let tool_use_id = block
            .get("tool_use_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let output = extract_tool_result_content(block.get("content"));
        let truncated = output.len() > 4096;
        let output = if truncated {
            output[..4096].to_string()
        } else {
            output
        };

        events.push(HerdEvent {
            at,
            kind: HerdEventKind::ToolResult,
            content: HerdContent::ToolResult { output, truncated },
            tool_use_id,
            parent_id: None,
        });
    }
    events
}

/// Extract text content from a tool_result block, which can be a string or array.
fn extract_tool_result_content(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };

    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text);
                }
            }
            parts.join("\n")
        }
        _ => content.to_string(),
    }
}

/// The one argument worth showing for a tool call.
///
/// Keyed by tool name where the useful field is known, with a generic fallback
/// so a tool this build has never heard of — an MCP tool, a future built-in —
/// still reads as something rather than a bare name.
#[allow(dead_code)]
fn tool_argument(name: &str, input: Option<&serde_json::Value>) -> Option<String> {
    let input = input?;
    let string = |key: &str| input.get(key).and_then(|v| v.as_str()).map(str::trim);

    let value = match name {
        "Bash" | "BashOutput" => string("description").or_else(|| string("command")),
        "Read" | "Edit" | "Write" | "NotebookEdit" => {
            return string("file_path").map(short_path);
        }
        "Glob" => string("pattern"),
        "Grep" => string("pattern").or_else(|| string("query")),
        "Task" | "Agent" => {
            let kind = string("subagent_type");
            let what = string("description");
            return match (kind, what) {
                (Some(kind), Some(what)) => Some(format!("{kind}: {what}")),
                (Some(kind), None) => Some(kind.to_string()),
                (None, what) => what.map(str::to_string),
            };
        }
        "WebFetch" => return string("url").map(short_url),
        "WebSearch" => string("query"),
        _ => string("description")
            .or_else(|| string("query"))
            .or_else(|| string("pattern"))
            .or_else(|| string("command"))
            .or_else(|| string("prompt"))
            .or_else(|| return_file_path(input)),
    };

    value
        .map(collapse_whitespace)
        .filter(|value| !value.is_empty())
}

/// Generic fallback for the `_` arm, kept separate so the `or_else` chain there
/// stays one type.
#[allow(dead_code)]
fn return_file_path(input: &serde_json::Value) -> Option<&str> {
    input.get("file_path").and_then(|v| v.as_str())
}

/// Last two path components, so a row shows `render/sidebar.rs` rather than an
/// absolute path that no column can fit.
#[allow(dead_code)]
fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// Host plus a hint of the path, which is all a row has room for.
#[allow(dead_code)]
fn short_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    clamp(without_scheme)
}

/// First sentence of an assistant message, as a single line.
fn first_sentence(text: &str) -> Option<String> {
    let text = collapse_whitespace(text);
    if text.is_empty() {
        return None;
    }
    // Sentence end, but only when it actually ends something — "sidebar.rs" and
    // "0.5" must not split.
    let mut end = None;
    let bytes = text.as_bytes();
    for (idx, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let next = bytes.get(idx + ch.len_utf8());
            if next.is_none() || next == Some(&b' ') {
                end = Some(idx + ch.len_utf8());
                break;
            }
        }
    }
    let sentence = match end {
        Some(end) => &text[..end],
        None => &text[..],
    };
    Some(clamp(sentence))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clamp(text: &str) -> String {
    if text.chars().count() <= MAX_EVENT_TEXT {
        return text.to_string();
    }
    let mut out: String = text
        .chars()
        .take(MAX_EVENT_TEXT.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// Parse an ISO-8601 timestamp into a `SystemTime`.
///
/// Claude writes UTC with a `Z` suffix; anything else that `chrono` can read is
/// accepted too, and anything it cannot simply leaves the event undated rather
/// than dropping it.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    let parsed = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    Some(SystemTime::from(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_herd::HerdContent;
    use std::time::Duration;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn assistant(timestamp: &str, blocks: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{timestamp}","message":{{"content":[{blocks}]}}}}"#
        )
    }

    #[test]
    fn tail_lines_returns_the_last_lines_oldest_first() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        write(&path, "a\nb\nc\nd\n");

        assert_eq!(tail_lines(&path, 2), vec!["c".to_string(), "d".to_string()]);
        assert_eq!(tail_lines(&path, 99).len(), 4);
        assert!(tail_lines(&path, 0).is_empty());
    }

    #[test]
    fn tail_lines_ignores_a_trailing_partial_write() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        // No trailing newline: the last line is whatever was flushed so far.
        write(&path, "a\nb\npartial");

        assert_eq!(tail_lines(&path, 1), vec!["partial".to_string()]);
    }

    #[test]
    fn tail_lines_grows_past_a_line_longer_than_the_initial_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        let huge = "x".repeat((TAIL_WINDOW as usize) * 2);
        write(&path, &format!("first\n{huge}\n"));

        let lines = tail_lines(&path, 2);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "first");
        assert_eq!(lines[1].len(), huge.len());
    }

    #[test]
    fn a_missing_or_empty_transcript_yields_no_activity() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope.jsonl");
        assert!(read_activity(&missing, 10).is_empty());

        let empty = temp.path().join("empty.jsonl");
        write(&empty, "");
        assert!(read_activity(&empty, 10).is_empty());
    }

    #[test]
    fn tool_calls_are_humanized_per_tool() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        write(
            &path,
            &[
                assistant(
                    "2026-07-30T10:00:00.000Z",
                    r#"{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"/a/b/render/sidebar.rs"}}"#,
                ),
                assistant(
                    "2026-07-30T10:00:01.000Z",
                    r#"{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"cargo check","description":"Type-check the workspace"}}"#,
                ),
                assistant(
                    "2026-07-30T10:00:02.000Z",
                    r#"{"type":"tool_use","id":"tu3","name":"Task","input":{"subagent_type":"Explore","description":"map the herd"}}"#,
                ),
                assistant(
                    "2026-07-30T10:00:03.000Z",
                    r#"{"type":"tool_use","id":"tu4","name":"Grep","input":{"pattern":"open_agent_herd"}}"#,
                ),
                assistant(
                    "2026-07-30T10:00:04.000Z",
                    r#"{"type":"tool_use","id":"tu5","name":"McpThing","input":{"query":"widgets"}}"#,
                ),
            ]
            .join("\n"),
        );

        let activity = read_activity(&path, 10);
        assert_eq!(activity.recent.len(), 5);
        // Tool events now carry ToolArgs; verify tool_use_id is extracted.
        assert_eq!(activity.recent[0].tool_use_id.as_deref(), Some("tu1"));
        assert_eq!(activity.recent[1].tool_use_id.as_deref(), Some("tu2"));
        assert!(matches!(
            activity.recent[0].content,
            HerdContent::ToolArgs { .. }
        ));
    }

    #[test]
    fn prose_is_kept_as_one_sentence_and_tool_results_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        write(
            &path,
            &[
                assistant(
                    "2026-07-30T10:00:00.000Z",
                    r#"{"type":"text","text":"Fixed the scroll bug in sidebar.rs. Now running the tests."}"#,
                ),
                r#"{"type":"user","timestamp":"2026-07-30T10:00:01.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]}}"#.to_string(),
                r#"{"type":"system","timestamp":"2026-07-30T10:00:02.000Z"}"#.to_string(),
            ]
            .join("\n"),
        );

        let activity = read_activity(&path, 10);
        // 1 assistant text + 1 tool_result from user line
        assert_eq!(activity.recent.len(), 2);
        assert_eq!(
            activity.recent[0].display_text(),
            "Fixed the scroll bug in sidebar.rs."
        );
        assert_eq!(activity.recent[0].kind, HerdEventKind::Assistant);
        assert_eq!(activity.recent[1].kind, HerdEventKind::ToolResult);
        assert_eq!(activity.recent[1].tool_use_id.as_deref(), Some("tu1"));
    }

    #[test]
    fn current_is_the_trailing_tool_call_only() {
        let temp = tempfile::tempdir().unwrap();
        let running = temp.path().join("running.jsonl");
        write(
            &running,
            &assistant(
                "2026-07-30T10:00:00.000Z",
                r#"{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"cargo build"}}"#,
            ),
        );
        let activity = read_activity(&running, 10);
        assert!(activity.current.is_some());
        assert_eq!(
            activity.current.as_ref().unwrap().tool_use_id.as_deref(),
            Some("tu1")
        );

        let spoke_after = temp.path().join("spoke.jsonl");
        write(
            &spoke_after,
            &[
                assistant(
                    "2026-07-30T10:00:00.000Z",
                    r#"{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"cargo build"}}"#,
                ),
                assistant(
                    "2026-07-30T10:00:05.000Z",
                    r#"{"type":"text","text":"Done."}"#,
                ),
            ]
            .join("\n"),
        );
        assert!(read_activity(&spoke_after, 10).current.is_none());
    }

    #[test]
    fn malformed_lines_degrade_one_event_not_the_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        write(
            &path,
            &[
                "{not json at all".to_string(),
                r#"{"type":"assistant"}"#.to_string(),
                r#"{"type":"assistant","message":{"content":"not an array"}}"#.to_string(),
                assistant(
                    "not a timestamp",
                    r#"{"type":"tool_use","id":"tu1","name":"Read","input":{}}"#,
                ),
            ]
            .join("\n"),
        );

        let activity = read_activity(&path, 10);
        assert_eq!(activity.recent.len(), 1);
        assert_eq!(activity.recent[0].kind, HerdEventKind::Tool);
        assert!(activity.recent[0].at.is_none());
        assert_eq!(activity.recent[0].tool_use_id.as_deref(), Some("tu1"));
    }

    #[test]
    fn the_event_log_is_capped_to_the_newest_events() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("t.jsonl");
        let lines: Vec<String> = (0..20)
            .map(|idx| {
                assistant(
                    "2026-07-30T10:00:00.000Z",
                    &format!(
                        r#"{{"type":"tool_use","id":"tu{idx}","name":"Read","input":{{"file_path":"f{idx}.rs"}}}}"#
                    ),
                )
            })
            .collect();
        write(&path, &lines.join("\n"));

        let activity = read_activity(&path, 3);
        assert_eq!(activity.recent.len(), 3);
        // Last event should be f19
        if let HerdContent::ToolArgs { args, .. } = &activity.recent[2].content {
            assert_eq!(
                args.get("file_path").and_then(|v| v.as_str()),
                Some("f19.rs")
            );
        } else {
            panic!("Expected ToolArgs content");
        }
    }

    #[test]
    fn headline_only_claims_now_for_a_working_agent_with_a_fresh_event() {
        let now = SystemTime::now();
        let fresh = HerdActivity {
            current: Some(HerdEvent {
                at: Some(now - Duration::from_secs(5)),
                kind: HerdEventKind::Tool,
                content: HerdContent::SingleLine("Bash cargo build".to_string()),
                tool_use_id: None,
                parent_id: None,
            }),
            recent: Vec::new(),
            subagent_tree: Vec::new(),
        };
        assert_eq!(
            fresh.headline(super::super::HerdStatus::Working, now),
            Some(("now", "Bash cargo build".to_string()))
        );
        assert_eq!(
            fresh.headline(super::super::HerdStatus::Idle, now),
            Some(("last", "Bash cargo build".to_string()))
        );

        let stale = HerdActivity {
            current: Some(HerdEvent {
                at: Some(now - Duration::from_secs(600)),
                kind: HerdEventKind::Tool,
                content: HerdContent::SingleLine("Bash cargo build".to_string()),
                tool_use_id: None,
                parent_id: None,
            }),
            recent: Vec::new(),
            subagent_tree: Vec::new(),
        };
        assert_eq!(
            stale.headline(super::super::HerdStatus::Working, now),
            Some(("last", "Bash cargo build".to_string()))
        );
    }
}
