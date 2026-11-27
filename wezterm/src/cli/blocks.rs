//! `wezterm cli get-blocks` and `wezterm cli mcp` (TGZTerminal, experimental
//! `assist.mcp`).
//!
//! Both read a pane's recent command blocks — command, output, exit code —
//! through the `GetCommandBlocks` PDU, which the GUI answers only while
//! `assist.mcp.enabled` is on and with secrets masked by the same rules as
//! the screen. `mcp` is a stdio MCP server (newline-delimited JSON-RPC, the
//! same hand-rolled shape as Tower's) so an agent running in one pane can
//! ask what failed in another. It reads; it only types into a pane when
//! `assist.mcp.allow_insert` is on, and then it pastes without pressing
//! Enter.

use crate::cli::CliOutputFormatKind;
use clap::Parser;
use codec::{CommandBlockText, GetCommandBlocks};
use mux::pane::PaneId;
use serde_json::{json, Value};
use smol::io::{AsyncBufReadExt, AsyncWriteExt};
use smol::stream::StreamExt;
use wezterm_client::client::Client;

#[derive(Debug, Parser, Clone)]
pub struct GetBlocks {
    /// The pane to read. The default is the current pane (WEZTERM_PANE).
    #[arg(long)]
    pane_id: Option<PaneId>,

    /// How many of the newest command blocks.
    #[arg(long, default_value = "3")]
    last: usize,

    /// Only commands that exited non-zero.
    #[arg(long)]
    failed_only: bool,

    /// "table" or "json".
    #[arg(long = "format", default_value = "table")]
    format: CliOutputFormatKind,
}

impl GetBlocks {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;
        let blocks = fetch(&client, pane_id, self.last, self.failed_only).await?;
        match self.format {
            CliOutputFormatKind::Json => {
                println!("{}", serde_json::to_string_pretty(&blocks_json(&blocks))?);
            }
            CliOutputFormatKind::Table => {
                for block in &blocks {
                    let status = match block.exit_status {
                        Some(code) => format!("exit {code}"),
                        None if block.guessed => "exit ? (guessed boundary)".to_string(),
                        None => "exit ?".to_string(),
                    };
                    println!("$ {}    [{}]", block.command, status);
                    if !block.output.is_empty() {
                        println!("{}", block.output);
                    }
                    println!();
                }
            }
        }
        Ok(())
    }
}

async fn fetch(
    client: &Client,
    pane_id: PaneId,
    last: usize,
    failed_only: bool,
) -> anyhow::Result<Vec<CommandBlockText>> {
    Ok(client
        .get_command_blocks(GetCommandBlocks {
            pane_id,
            last,
            failed_only,
        })
        .await?
        .blocks)
}

fn blocks_json(blocks: &[CommandBlockText]) -> Value {
    Value::Array(
        blocks
            .iter()
            .map(|block| {
                json!({
                    "command": block.command,
                    "output": block.output,
                    "exit_status": block.exit_status,
                    "guessed": block.guessed,
                    "head_clipped": block.head_clipped,
                })
            })
            .collect(),
    )
}

#[derive(Debug, Parser, Clone)]
pub struct Mcp {}

fn tool_schemas(allow_insert: bool) -> Value {
    let mut tools = vec![
        json!({
            "name": "terminal_list_panes",
            "description": "List the panes of the user's terminal (TGZTerminal): id, tab, title, working directory, and which one you are running in.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "terminal_last_blocks",
            "description": "Read the most recent commands of a terminal pane with their output and exit code. Without pane_id it reads the other pane in your tab. Secrets are masked.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane_id": {"type": "integer", "description": "Pane to read; see terminal_list_panes."},
                    "last": {"type": "integer", "description": "How many commands (default 3, at most 50)."},
                    "failed_only": {"type": "boolean", "description": "Only commands that exited non-zero."}
                }
            },
        }),
    ];
    if allow_insert {
        tools.push(json!({
            "name": "terminal_insert_command",
            "description": "Paste a command at a pane's shell prompt for the user to review. It never presses Enter: the user decides whether it runs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane_id": {"type": "integer"},
                    "command": {"type": "string"}
                },
                "required": ["pane_id", "command"]
            },
        }));
    }
    Value::Array(tools)
}

/// The pane this server's agent runs in, from the environment.
fn own_pane() -> Option<PaneId> {
    std::env::var("WEZTERM_PANE").ok()?.parse().ok()
}

struct PaneInfo {
    pane_id: PaneId,
    tab_id: mux::tab::TabId,
    title: String,
    cwd: String,
    is_active: bool,
    tab_title: String,
}

async fn list_panes(client: &Client) -> anyhow::Result<Vec<PaneInfo>> {
    let panes = client.list_panes().await?;
    let mut out = Vec::new();
    for (tabroot, tab_title) in panes.tabs.into_iter().zip(panes.tab_titles.iter()) {
        let mut cursor = tabroot.into_tree().cursor();
        loop {
            if let Some(entry) = cursor.leaf_mut() {
                out.push(PaneInfo {
                    pane_id: entry.pane_id,
                    tab_id: entry.tab_id,
                    title: entry.title.clone(),
                    cwd: entry
                        .working_dir
                        .as_ref()
                        .map(|url| url.url.path().to_string())
                        .unwrap_or_default(),
                    is_active: entry.is_active_pane,
                    tab_title: tab_title.clone(),
                });
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(_) => break,
            }
        }
    }
    Ok(out)
}

/// Without a pane id: the other pane in the caller's tab, preferring the
/// active one, then the first.
fn default_target(panes: &[PaneInfo], own: Option<PaneId>) -> Result<PaneId, String> {
    let own = own.ok_or("pass pane_id (see terminal_list_panes): WEZTERM_PANE is not set")?;
    let tab = panes
        .iter()
        .find(|pane| pane.pane_id == own)
        .map(|pane| pane.tab_id)
        .ok_or("pass pane_id (see terminal_list_panes): your pane was not found")?;
    let others: Vec<&PaneInfo> = panes
        .iter()
        .filter(|pane| pane.tab_id == tab && pane.pane_id != own)
        .collect();
    others
        .iter()
        .find(|pane| pane.is_active)
        .or_else(|| others.first())
        .map(|pane| pane.pane_id)
        .ok_or_else(|| {
            "your tab has no other pane: pass pane_id (see terminal_list_panes)".to_string()
        })
}

async fn call_tool(
    client: &Client,
    name: &str,
    args: &Value,
    allow_insert: bool,
) -> Result<String, String> {
    let own = own_pane();
    let pane_arg = args
        .get("pane_id")
        .and_then(Value::as_u64)
        .map(|id| id as PaneId);
    match name {
        "terminal_list_panes" => {
            let panes = list_panes(client).await.map_err(|err| format!("{err:#}"))?;
            let items: Vec<Value> = panes
                .iter()
                .map(|pane| {
                    json!({
                        "pane_id": pane.pane_id,
                        "tab_id": pane.tab_id,
                        "tab_title": pane.tab_title,
                        "title": pane.title,
                        "cwd": pane.cwd,
                        "is_active": pane.is_active,
                        "is_you": Some(pane.pane_id) == own,
                    })
                })
                .collect();
            serde_json::to_string_pretty(&items).map_err(|err| err.to_string())
        }
        "terminal_last_blocks" => {
            let pane_id = match pane_arg {
                Some(id) => id,
                None => {
                    let panes = list_panes(client).await.map_err(|err| format!("{err:#}"))?;
                    default_target(&panes, own)?
                }
            };
            let last = args
                .get("last")
                .and_then(Value::as_u64)
                .unwrap_or(3)
                .clamp(1, 50) as usize;
            let failed_only = args
                .get("failed_only")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let blocks = fetch(client, pane_id, last, failed_only)
                .await
                .map_err(|err| format!("{err:#}"))?;
            let body = json!({"pane_id": pane_id, "blocks": blocks_json(&blocks)});
            serde_json::to_string_pretty(&body).map_err(|err| err.to_string())
        }
        "terminal_insert_command" if allow_insert => {
            let pane_id = pane_arg.ok_or("pane_id is required")?;
            if Some(pane_id) == own {
                return Err("that is your own pane".to_string());
            }
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .ok_or("command is required")?;
            // Never Enter, never other control characters: the user runs it.
            let command: String = command
                .trim_end_matches(['\r', '\n'])
                .chars()
                .filter(|c| !c.is_control() || *c == '\t')
                .collect();
            if command.is_empty() {
                return Err("the command is empty".to_string());
            }
            client
                .send_paste(codec::SendPaste {
                    pane_id,
                    data: command,
                })
                .await
                .map_err(|err| format!("{err:#}"))?;
            Ok(format!(
                "Pasted at the prompt of pane {pane_id}; the user decides whether to run it."
            ))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

/// One MCP JSON-RPC message; `None` for notifications.
async fn handle(client: &Client, msg: &Value, allow_insert: bool) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let result = match method {
        "initialize" => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18");
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tgzterminal", "version": config::wezterm_version()},
                "instructions": "Read the user's terminal: terminal_last_blocks returns the latest commands of a pane with output and exit codes (secrets masked), so you can see what failed without asking the user to paste it."
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_schemas(allow_insert)}),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (ok, text) = match call_tool(client, name, &args, allow_insert).await {
                Ok(text) => (true, text),
                Err(text) => (false, text),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": !ok})
        }
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("method not found: {method}")}}))
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

impl Mcp {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let allow_insert = config::configuration().assist.mcp.allow_insert;
        let stdin = smol::io::BufReader::new(smol::Unblock::new(std::io::stdin()));
        let mut stdout = smol::Unblock::new(std::io::stdout());
        let mut lines = stdin.lines();
        while let Some(line) = lines.next().await {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => handle(&client, &msg, allow_insert).await,
                Err(err) => Some(json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": format!("parse error: {err}")}})),
            };
            if let Some(reply) = reply {
                let mut text = reply.to_string();
                text.push('\n');
                stdout.write_all(text.as_bytes()).await?;
                stdout.flush().await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(pane_id: PaneId, tab_id: mux::tab::TabId, is_active: bool) -> PaneInfo {
        PaneInfo {
            pane_id,
            tab_id,
            title: String::new(),
            cwd: String::new(),
            is_active,
            tab_title: String::new(),
        }
    }

    #[test]
    fn the_default_target_is_the_other_pane_in_your_tab() {
        let panes = [pane(1, 7, true), pane(2, 7, false), pane(3, 8, true)];
        assert_eq!(default_target(&panes, Some(1)), Ok(2));
        // Another tab's active pane is never picked.
        assert!(default_target(&[pane(1, 7, true), pane(3, 8, true)], Some(1)).is_err());
        assert!(default_target(&panes, None).is_err());
    }

    #[test]
    fn insert_is_offered_only_when_allowed() {
        let names = |tools: Value| -> Vec<String> {
            tools
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(!names(tool_schemas(false)).contains(&"terminal_insert_command".to_string()));
        assert!(names(tool_schemas(true)).contains(&"terminal_insert_command".to_string()));
    }
}
