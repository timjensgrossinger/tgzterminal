//! Reading an answering CLI's stdout (TGZTerminal Ask panel).
//!
//! `claude -p --output-format stream-json --verbose --include-partial-messages`
//! writes one JSON object per line. The ones used here (shapes as observed
//! from Claude Code 2.1.332, see `fixtures/claude_stream.jsonl`):
//!
//! * `stream_event` / `content_block_delta` / `text_delta` — answer text as
//!   it is generated;
//! * `stream_event` / `content_block_start` with a `thinking` block — the
//!   model is thinking (no text is shown for it);
//! * `assistant` messages carrying a complete `tool_use` block — a file the
//!   model looked at (the partial `input_json_delta`s are ignored: the
//!   complete input arrives in the `assistant` message anyway);
//! * `user` messages carrying a `tool_result` with `is_error` — that look
//!   was refused (a deny rule) or failed;
//! * `result` — the end: success or error, the final text, cost, duration.
//!
//! Anything else is ignored, so a newer CLI adding event types does not
//! break the panel. Plain-text providers are read as text deltas.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum AssistEvent {
    /// More answer text.
    Text(String),
    /// The model is thinking.
    Thinking,
    /// The model looked at something.
    Tool { id: String, label: String },
    /// That look was refused or failed.
    ToolFailed { id: String },
    /// The run ended. `text` is the authoritative final answer when the CLI
    /// reports one (it replaces the streamed deltas).
    Done {
        ok: bool,
        text: Option<String>,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
    },
}

/// One line of `claude -p --output-format stream-json` output.
pub fn parse_claude_line(line: &str, cwd: Option<&str>) -> Vec<AssistEvent> {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return vec![];
    };
    match value.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let event = &value["event"];
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => {
                    let delta = &event["delta"];
                    if delta.get("type").and_then(Value::as_str) == Some("text_delta") {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                return vec![AssistEvent::Text(text.to_string())];
                            }
                        }
                    }
                    vec![]
                }
                Some("content_block_start")
                    if event["content_block"].get("type").and_then(Value::as_str)
                        == Some("thinking") =>
                {
                    vec![AssistEvent::Thinking]
                }
                _ => vec![],
            }
        }
        Some("assistant") => content(&value)
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|block| AssistEvent::Tool {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                label: tool_label(
                    block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                    &block["input"],
                    cwd,
                ),
            })
            .collect(),
        Some("user") => content(&value)
            .iter()
            .filter(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block.get("is_error").and_then(Value::as_bool) == Some(true)
            })
            .map(|block| AssistEvent::ToolFailed {
                id: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
        Some("result") => vec![AssistEvent::Done {
            ok: value.get("is_error").and_then(Value::as_bool) != Some(true)
                && value.get("subtype").and_then(Value::as_str) == Some("success"),
            text: value
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string),
            cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
            duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        }],
        _ => vec![],
    }
}

fn content(value: &Value) -> Vec<Value> {
    value["message"]["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// "read src/api/routes.js", "grep 'ETag' in src", "glob **/*.ts".
fn tool_label(name: &str, input: &Value, cwd: Option<&str>) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("");
    let path = |p: &str| shorten(p, cwd);
    match name {
        "Read" => format!("read {}", path(field("file_path"))),
        "Grep" => {
            let place = field("path");
            if place.is_empty() {
                format!("grep '{}'", field("pattern"))
            } else {
                format!("grep '{}' in {}", field("pattern"), path(place))
            }
        }
        "Glob" => format!("glob {}", field("pattern")),
        other => other.to_ascii_lowercase(),
    }
}

/// A path relative to the pane's directory when it lies inside it.
fn shorten(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd {
        let cwd = cwd.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(cwd) {
            if let Some(rest) = rest.strip_prefix('/') {
                return rest.to_string();
            }
            if rest.is_empty() {
                return ".".to_string();
            }
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/claude_stream.jsonl");

    fn events() -> Vec<AssistEvent> {
        FIXTURE
            .lines()
            .flat_map(|line| parse_claude_line(line, Some("/tmp/proj")))
            .collect()
    }

    #[test]
    fn the_recorded_run_reads_as_tools_then_text_then_done() {
        let events = events();
        let tools: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                AssistEvent::Tool { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            vec!["grep '^version\\s*=' in Cargo.toml", "read .env"]
        );

        let failed = events
            .iter()
            .filter(|event| matches!(event, AssistEvent::ToolFailed { .. }))
            .count();
        assert_eq!(failed, 1, "the .env read was denied");

        let streamed: String = events
            .iter()
            .filter_map(|event| match event {
                AssistEvent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(streamed.contains("0.3.1"));

        match events.last() {
            Some(AssistEvent::Done {
                ok, text, cost_usd, ..
            }) => {
                assert!(ok);
                assert_eq!(text.as_deref().map(str::trim), Some(streamed.trim()));
                assert!(cost_usd.is_some());
            }
            other => panic!("{:?}", other),
        }
        assert!(events.contains(&AssistEvent::Thinking));
    }

    #[test]
    fn garbage_and_unknown_events_are_ignored() {
        assert!(parse_claude_line("not json", None).is_empty());
        assert!(parse_claude_line(r#"{"type":"rate_limit_event"}"#, None).is_empty());
        assert!(parse_claude_line(r#"{"type":"system","subtype":"init"}"#, None).is_empty());
    }

    #[test]
    fn an_error_result_is_not_ok() {
        let line = r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#;
        assert_eq!(
            parse_claude_line(line, None),
            vec![AssistEvent::Done {
                ok: false,
                text: None,
                cost_usd: None,
                duration_ms: None
            }]
        );
    }
}
