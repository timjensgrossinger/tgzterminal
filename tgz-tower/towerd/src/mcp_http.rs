//! A small MCP client for the Streamable HTTP transport: JSON-RPC requests
//! POSTed to one URL, answered as JSON or as a server-sent-event stream.
//! Covers what Tower needs from remote servers like Atlassian's Rovo MCP:
//! `initialize`, `notifications/initialized` and `tools/call`.

use serde_json::{json, Value};
use std::time::Duration;

pub const PROTOCOL: &str = "2025-06-18";

pub struct McpHttp {
    url: String,
    /// Full `Authorization` header value.
    auth: Option<String>,
    session: Option<String>,
    next_id: u64,
    client: reqwest::Client,
}

/// The JSON-RPC message with `id` from an SSE body (`data:` lines, events
/// separated by blank lines).
pub fn find_in_sse(body: &str, id: u64) -> Option<Value> {
    let mut data = String::new();
    let mut found = None;
    let mut flush = |data: &mut String| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v.get("id").and_then(Value::as_u64) == Some(id) && found.is_none() {
                    found = Some(v);
                }
            }
            data.clear();
        }
    };
    for line in body.lines() {
        if line.is_empty() {
            flush(&mut data);
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    flush(&mut data);
    found
}

/// A tool result as JSON: `structuredContent`, else the text content parsed
/// as JSON when it is JSON, else the text itself.
pub fn tool_value(result: &Value) -> Result<Value, String> {
    let text: String = result
        .get("content")
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(if text.is_empty() {
            "the tool failed".into()
        } else {
            text
        });
    }
    if let Some(s) = result.get("structuredContent") {
        return Ok(s.clone());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

impl McpHttp {
    pub async fn connect(url: &str, auth: Option<String>) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        let mut c = Self {
            url: url.to_string(),
            auth,
            session: None,
            next_id: 0,
            client,
        };
        c.rpc(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "tower", "version": env!("CARGO_PKG_VERSION")},
            }),
        )
        .await?;
        c.notify("notifications/initialized").await?;
        Ok(c)
    }

    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .json(body);
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        req
    }

    async fn notify(&mut self, method: &str) -> Result<(), String> {
        let resp = self
            .post(&json!({"jsonrpc": "2.0", "method": method}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{method}: HTTP {}", resp.status()));
        }
        Ok(())
    }

    pub async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let resp = self
            .post(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .map_err(|e| format!("{}: {e}", self.url))?;
        let status = resp.status();
        if let Some(s) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session = Some(s.to_string());
        }
        let sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|t| t.starts_with("text/event-stream"))
            .unwrap_or(false);
        let body = resp.text().await.map_err(|e| e.to_string())?;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(format!(
                "{method}: not authorized (HTTP {status}); check the token with `tower auth jira <site>`"
            ));
        }
        if !status.is_success() {
            return Err(format!(
                "{method}: HTTP {status}: {}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let msg = if sse {
            find_in_sse(&body, id)
                .ok_or_else(|| format!("{method}: no response in the event stream"))?
        } else {
            serde_json::from_str::<Value>(&body).map_err(|e| format!("{method}: bad JSON: {e}"))?
        };
        if let Some(err) = msg.get("error") {
            let m = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            return Err(format!("{method}: {m}"));
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let result = self
            .rpc("tools/call", json!({"name": name, "arguments": args}))
            .await?;
        tool_value(&result).map_err(|e| format!("{name}: {e}"))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn sse_bodies_yield_the_matching_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(find_in_sse(body, 2).unwrap()["result"]["ok"], true);
        assert!(find_in_sse(body, 3).is_none());
    }

    #[test]
    fn tool_results_become_json() {
        let r = json!({"content": [{"type": "text", "text": "{\"issues\": []}"}]});
        assert_eq!(tool_value(&r).unwrap()["issues"], json!([]));
        let r = json!({"content": [{"type": "text", "text": "plain"}]});
        assert_eq!(tool_value(&r).unwrap(), json!("plain"));
        let r = json!({"content": [{"type": "text", "text": "nope"}], "isError": true});
        assert_eq!(tool_value(&r).unwrap_err(), "nope");
        let r = json!({"content": [], "structuredContent": {"a": 1}});
        assert_eq!(tool_value(&r).unwrap()["a"], 1);
    }

    /// A fake Streamable HTTP MCP server. `tools` maps a tool name to the
    /// JSON its text content carries. Odd requests answer as SSE.
    pub async fn fake_server(tools: Vec<(&'static str, Value)>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                let (mut s, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => return,
                };
                n += 1;
                let mut buf = Vec::new();
                let mut tmp = vec![0u8; 65536];
                // Read headers + the Content-Length body.
                loop {
                    let k = s.read(&mut tmp).await.unwrap_or(0);
                    if k == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..k]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = text.find("\r\n\r\n") {
                        let len = text[..i]
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let body = text.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
                let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let auth_ok = text.to_lowercase().contains("authorization: basic");
                let reply = match (req.get("id"), req.get("method").and_then(Value::as_str)) {
                    (None, _) => None,
                    (Some(id), Some("initialize")) => Some(
                        json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": PROTOCOL, "capabilities": {"tools": {}}}}),
                    ),
                    (Some(id), Some("tools/call")) => {
                        let name = req
                            .pointer("/params/name")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let data = tools
                            .iter()
                            .find(|(n, _)| *n == name)
                            .map(|(_, v)| v.clone());
                        Some(match data {
                            Some(v) if auth_ok => {
                                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": v.to_string()}]}})
                            }
                            Some(_) => {
                                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "unauthorized"}], "isError": true}})
                            }
                            None => {
                                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("unknown tool {name}")}})
                            }
                        })
                    }
                    (Some(id), _) => Some(
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "nope"}}),
                    ),
                };
                let resp = match reply {
                    None => {
                        "HTTP/1.1 202 Accepted\r\nconnection: close\r\ncontent-length: 0\r\n\r\n"
                            .to_string()
                    }
                    Some(v) if n % 2 == 1 => {
                        let b = format!("event: message\ndata: {}\n\n", v);
                        format!("HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: text/event-stream\r\nmcp-session-id: s1\r\ncontent-length: {}\r\n\r\n{b}", b.len())
                    }
                    Some(v) => {
                        let b = v.to_string();
                        format!("HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{b}", b.len())
                    }
                };
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        format!("http://{addr}/mcp")
    }

    #[tokio::test]
    async fn talks_to_a_streamable_http_server() {
        let url = fake_server(vec![("echo", json!({"hello": "world"}))]).await;
        let mut c = McpHttp::connect(&url, Some("Basic eDp5".into()))
            .await
            .unwrap();
        assert_eq!(c.session.as_deref(), Some("s1"));
        assert_eq!(
            c.call_tool("echo", json!({})).await.unwrap()["hello"],
            "world"
        );
        assert!(c
            .call_tool("missing", json!({}))
            .await
            .unwrap_err()
            .contains("unknown tool"));
    }
}
