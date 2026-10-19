//! Agent-side commands: `tower hook <event>` (Claude Code hooks) and
//! `tower mcp` (the team tools as a stdio MCP server). Both are short-lived
//! clients of towerd using blocking std IO; neither may ever print to stdout
//! except MCP protocol messages, since Claude reads hook stdout as context.

use crate::transport::round_trip;
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::time::Duration;
use tower_proto::{ClientMsg, ServerMsg};

/// What Tower put in the agent's environment: its id, the endpoint and its
/// own token (which only allows this agent's hook events and tool calls).
#[derive(Clone)]
struct Ctx {
    agent_id: String,
    sock: PathBuf,
    token: String,
}

fn env_ctx() -> Option<Ctx> {
    let get = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
    Some(Ctx {
        agent_id: get("TOWER_AGENT_ID")?,
        sock: PathBuf::from(get("TOWER_SOCK")?),
        token: get("TOWER_AGENT_TOKEN")?,
    })
}

/// `tower hook <event>`: forward the hook payload, never block Claude.
pub fn hook(event: &str) -> anyhow::Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    let Some(ctx) = env_ctx() else {
        return Ok(());
    };
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let msg = ClientMsg::Hook {
        agent_id: ctx.agent_id.clone(),
        event: event.to_string(),
        payload,
    };
    // Errors are swallowed on purpose: a hook failure must not disturb the agent.
    let _ = round_trip(&ctx.sock, &ctx.token, "hook", &msg, Duration::from_secs(2));
    Ok(())
}

pub fn tool_schemas() -> Value {
    let s = |d: &str| json!({"type": "string", "description": d});
    let list = |d: &str| json!({"type": "array", "items": {"type": "string"}, "description": d});
    json!([
        {
            "name": "tower_flight",
            "description": "Your flight's brief, or the handoff packet you are working on.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "tower_roster",
            "description": "Team roles, their model ladders and which agents are live.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "tower_handoff",
            "description": "Hand a self-contained, well-scoped piece of work to another role (usually a smaller, cheaper model). The packet is all the receiver gets. The result comes back to you later as a [Tower] message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to_role": s("Role id from tower_roster, e.g. \"ground\"."),
                    "goal": s("One or two sentences: what to achieve."),
                    "context": list("Files, functions or facts the receiver needs."),
                    "constraints": list("What not to touch or change."),
                    "acceptance": list("How the receiver can tell it is done (at least one).")
                },
                "required": ["to_role", "goal", "acceptance"]
            }
        },
        {
            "name": "tower_return",
            "description": "Report the result of the handoff packet you were given back to its sender. Call once, when done.",
            "inputSchema": {"type": "object", "properties": {"summary": s("What changed and how you verified it.")}, "required": ["summary"]}
        },
        {
            "name": "tower_post",
            "description": "Post a short note on the team frequency. @role or @agent-id reaches that agent.",
            "inputSchema": {"type": "object", "properties": {"text": s("The note.")}, "required": ["text"]}
        },
        {
            "name": "tower_ask_human",
            "description": "Ask the human a question. Returns immediately; the answer arrives later as a [Tower] message.",
            "inputSchema": {"type": "object", "properties": {"question": s("The question.")}, "required": ["question"]}
        },
        {
            "name": "tower_decide",
            "description": "Ask the human to make a decision you should not make alone: a design choice, scope, a trade-off, anything with consequences. Give 2-4 concrete options and your recommendation. Returns immediately; the decision arrives later as a [Tower] message. Keep working on what does not depend on it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": s("The decision to make, in one sentence."),
                    "options": list("2-4 options, each a short label, optionally followed by ' - ' and its trade-off."),
                    "recommendation": s("The option you recommend and why, briefly."),
                    "context": s("What the human needs to know to decide.")
                },
                "required": ["question", "options"]
            }
        },
        {
            "name": "tower_complete",
            "description": "End your flight. outcome: \"resolved\" (done and verified; gates run), \"proposed\" (you could not or must not do it yourself, e.g. no access to a server: the summary is the exact solution for a human to carry out) or \"declined\" (it cannot or should not be done; say why). List any documents or prototypes you produced in artifacts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": s("What you did and how you verified it, or the proposed steps, or why it is declined."),
                    "outcome": {"type": "string", "enum": ["resolved", "proposed", "declined"], "description": "Default: resolved."},
                    "artifacts": list("Files you produced for the human (design docs, HTML prototypes), relative to your working directory.")
                }
            }
        }
    ])
}

/// Handle one MCP JSON-RPC message; `None` for notifications.
pub fn mcp_handle(
    msg: &Value,
    call: &mut dyn FnMut(&str, &Value) -> (bool, String),
) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = id?;
    let result = match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tower", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Team tools for agents launched by Tower in TGZTerminal."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_schemas()}),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (ok, text) = call(name, &args);
            json!({"content": [{"type": "text", "text": text}], "isError": !ok})
        }
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {method}")}}))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// `tower mcp`: newline-delimited JSON-RPC on stdin/stdout.
pub fn mcp() -> anyhow::Result<()> {
    let ctx = env_ctx();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut call = |name: &str, args: &Value| -> (bool, String) {
        let Some(ctx) = ctx.clone() else {
            return (
                false,
                "This agent was not launched by Tower (TOWER_AGENT_ID / TOWER_AGENT_TOKEN are unset).".into(),
            );
        };
        let msg = ClientMsg::Tool {
            agent_id: ctx.agent_id.clone(),
            name: name.to_string(),
            args: args.clone(),
        };
        match round_trip(&ctx.sock, &ctx.token, "mcp", &msg, Duration::from_secs(30)) {
            Ok(ServerMsg::ToolResult { ok, text }) => (ok, text),
            Ok(ServerMsg::Error { message }) => (false, message),
            Ok(other) => (false, format!("unexpected reply {other:?}")),
            Err(err) => (false, format!("Tower is not reachable: {err:#}")),
        }
    };
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(reply) = mcp_handle(&msg, &mut call) {
            writeln!(out, "{}", serde_json::to_string(&reply)?)?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_lists_tools_and_forwards_calls() {
        let mut seen = Vec::new();
        let mut call = |name: &str, args: &Value| {
            seen.push((name.to_string(), args.clone()));
            (true, "ok".to_string())
        };
        let init = mcp_handle(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
            &mut call,
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(mcp_handle(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            &mut call
        )
        .is_none());
        let list = mcp_handle(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            &mut call,
        )
        .unwrap();
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"tower_handoff".to_string()));
        let r = mcp_handle(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "tower_post", "arguments": {"text": "hi"}}}),
            &mut call,
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "ok");
        let r = mcp_handle(
            &json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            &mut call,
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
        drop(call);
        assert_eq!(
            seen,
            vec![("tower_post".to_string(), json!({"text": "hi"}))]
        );
    }
}
