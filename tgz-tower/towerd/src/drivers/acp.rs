//! ACP driver: an Agent Client Protocol agent (OpenCode, ...) as a child process.
//!
//! Hand-written JSON-RPC over stdio, covering what Tower needs:
//! `initialize`, `session/new` (with the `tower` MCP server),
//! `session/set_config_option` for the model, `session/prompt`,
//! `session/cancel`, `session/update` notifications and
//! `session/request_permission` requests. Tower advertises no fs/terminal
//! capabilities, so the agent uses its own tools.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum AcpCmd {
    Prompt(String),
    Cancel,
    Permission {
        rpc_id: Value,
        option_id: Option<String>,
    },
    Stop,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    /// `allow_once`, `allow_always`, `reject_once`, `reject_always`.
    pub kind: String,
}

#[derive(Debug, PartialEq)]
pub enum AcpEvent {
    Ready,
    Text {
        message_id: Option<String>,
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
    },
    ToolDone {
        id: String,
        failed: bool,
    },
    TurnEnd {
        stop_reason: String,
        /// Tokens this turn used, when the agent reports them.
        usage: Option<tower_proto::Usage>,
    },
    /// The session's cumulative cost so far, in USD.
    Cost(f64),
    Permission {
        rpc_id: Value,
        title: String,
        options: Vec<PermOption>,
        /// The tool call as JSON (command, paths, URLs), for policy checks.
        input: String,
    },
    Error(String),
    Exited,
}

pub struct AcpSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub model: Option<String>,
    /// Session config options to set after the model, e.g. `mode`.
    pub options: Vec<(String, String)>,
    /// Options set only when the agent offers them (effort, agent).
    pub optional: Vec<(String, String)>,
    /// `mcpServers` entries: the `tower` server and the role's own; none for
    /// one-shot sessions like triage.
    pub mcp: Vec<Value>,
    pub stderr_log: PathBuf,
    /// The role declares `auth = "api_key"`: pick the agent's API-key sign-in
    /// method from `authMethods` (and refuse if it has none) instead of
    /// letting it fall back to a plan login. Otherwise Tower calls no
    /// `authenticate` at all: the agent signs in its own way.
    pub api_key_auth: bool,
    /// The model must be set (local-only work): if the agent refuses it,
    /// end the session before any prompt rather than run on its default.
    pub model_required: bool,
}

/// The id of an API-key sign-in among an agent's `authMethods`.
pub fn pick_api_key_method(methods: &Value) -> Option<String> {
    methods.as_array()?.iter().find_map(|m| {
        let id = m.get("id").and_then(Value::as_str)?;
        let text = format!(
            "{} {} {}",
            id,
            m.get("name").and_then(Value::as_str).unwrap_or(""),
            m.get("description").and_then(Value::as_str).unwrap_or("")
        )
        .to_lowercase();
        let api_key = ["api key", "api-key", "api_key", "apikey"]
            .iter()
            .any(|k| text.contains(k));
        api_key.then(|| id.to_string())
    })
}

/// `TOWER_DEBUG_ACP=1`: log JSON-RPC method names, ids and update kinds
/// (never contents) to towerd's stderr.
fn trace(dir: &str, v: &Value) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var("TOWER_DEBUG_ACP").as_deref() == Ok("1")) {
        return;
    }
    let method = v.get("method").and_then(Value::as_str).unwrap_or("");
    let id = v.get("id").map(|i| i.to_string()).unwrap_or_default();
    let kind = v
        .pointer("/params/update/sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or("");
    let err = if v.get("error").is_some() {
        " error"
    } else {
        ""
    };
    eprintln!("tower acp {dir} {method} {id} {kind}{err}");
}

enum Pending {
    Init,
    Authenticate,
    NewSession,
    /// A `session/set_config_option` (or `session/set_model`) for this id.
    SetOption(String),
    Prompt,
}

pub fn spawn<F>(spec: AcpSpec, emit: F) -> mpsc::UnboundedSender<AcpCmd>
where
    F: Fn(AcpEvent) + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        if let Err(err) = run(spec, rx, &emit).await {
            emit(AcpEvent::Error(format!("{err:#}")));
        }
        emit(AcpEvent::Exited);
    });
    tx
}

struct Conn {
    stdin: ChildStdin,
    next_id: u64,
    pending: HashMap<u64, Pending>,
}

impl Conn {
    async fn write(&mut self, v: &Value) -> anyhow::Result<()> {
        trace("->", v);
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value, kind: Pending) -> anyhow::Result<()> {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, kind);
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
    }
}

/// Parse one `session/update` into an event, if Tower cares about it.
pub fn parse_update(update: &Value) -> Option<AcpEvent> {
    match update.get("sessionUpdate")?.as_str()? {
        "agent_message_chunk" => {
            let c = update.get("content")?;
            if c.get("type").and_then(Value::as_str) != Some("text") {
                return None;
            }
            Some(AcpEvent::Text {
                message_id: update
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                text: c.get("text")?.as_str()?.to_string(),
            })
        }
        "tool_call" => Some(AcpEvent::ToolCall {
            id: update.get("toolCallId")?.as_str()?.to_string(),
            title: update
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
        }),
        "usage_update" => parse_cost(update.get("cost")?).map(AcpEvent::Cost),
        "tool_call_update" => match update.get("status").and_then(Value::as_str) {
            Some(s @ ("completed" | "failed")) => Some(AcpEvent::ToolDone {
                id: update.get("toolCallId")?.as_str()?.to_string(),
                failed: s == "failed",
            }),
            _ => None,
        },
        _ => None,
    }
}

/// `{"inputTokens": .., "outputTokens": ..}` from a prompt result.
pub fn parse_turn_usage(v: &Value) -> Option<tower_proto::Usage> {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let u = tower_proto::Usage {
        input_tokens: n("inputTokens") + n("cachedReadTokens") + n("cachedWriteTokens"),
        output_tokens: n("outputTokens") + n("thoughtTokens"),
        cost_usd: None,
    };
    (u.total() > 0).then_some(u)
}

/// A cost as a bare number or `{"amount": n, "currency": "USD"}`.
pub fn parse_cost(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Object(o) => {
            let cur = o.get("currency").and_then(Value::as_str).unwrap_or("USD");
            if !cur.eq_ignore_ascii_case("usd") {
                return None;
            }
            o.get("amount").and_then(Value::as_f64)
        }
        _ => None,
    }
}

/// One prompt, one answer: run an ACP agent for a single turn and return
/// what it said. Every permission request is refused, so the agent works
/// with what is in the prompt (used for triage).
pub async fn ask_once(
    spec: AcpSpec,
    prompt: String,
    timeout: std::time::Duration,
) -> Result<(String, tower_proto::Usage), String> {
    let (etx, mut erx) = mpsc::unbounded_channel();
    let cmd = spawn(spec, move |ev| {
        let _ = etx.send(ev);
    });
    let _ = cmd.send(AcpCmd::Prompt(prompt));
    let deadline = tokio::time::Instant::now() + timeout;
    let mut text = String::new();
    let mut usage = tower_proto::Usage::default();
    let mut last_error: Option<String> = None;
    let result = loop {
        match tokio::time::timeout_at(deadline, erx.recv()).await {
            Err(_) => break Err("the agent did not answer in time".to_string()),
            Ok(None) | Ok(Some(AcpEvent::Exited)) => {
                break Err(last_error.unwrap_or_else(|| "the agent exited".into()));
            }
            Ok(Some(AcpEvent::Text { text: t, .. })) => text.push_str(&t),
            Ok(Some(AcpEvent::Permission { rpc_id, .. })) => {
                let _ = cmd.send(AcpCmd::Permission {
                    rpc_id,
                    option_id: None,
                });
            }
            Ok(Some(AcpEvent::Error(e))) => last_error = Some(e),
            Ok(Some(AcpEvent::TurnEnd { usage: u, .. })) => {
                if let Some(u) = u {
                    usage = u;
                }
                break match last_error {
                    Some(e) if text.trim().is_empty() => Err(e),
                    _ => Ok((text, usage)),
                };
            }
            Ok(Some(_)) => {}
        }
    };
    let _ = cmd.send(AcpCmd::Stop);
    result
}

pub fn parse_permission(rpc_id: Value, params: &Value) -> AcpEvent {
    let title = params
        .pointer("/toolCall/title")
        .and_then(Value::as_str)
        .unwrap_or("Run a tool")
        .to_string();
    let options = params
        .get("options")
        .and_then(Value::as_array)
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    Some(PermOption {
                        id: o.get("optionId")?.as_str()?.to_string(),
                        name: o
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string(),
                        kind: o
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let input = params
        .get("toolCall")
        .map(|t| t.to_string())
        .unwrap_or_default();
    AcpEvent::Permission {
        rpc_id,
        title,
        options,
        input: tower_core::shorten(&input, 8000),
    }
}

fn error_text(v: &Value) -> String {
    v.pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error")
        .to_string()
}

async fn run<F: Fn(AcpEvent)>(
    spec: AcpSpec,
    mut cmds: mpsc::UnboundedReceiver<AcpCmd>,
    emit: &F,
) -> anyhow::Result<()> {
    let program = spec
        .argv
        .first()
        .ok_or_else(|| anyhow::anyhow!("role has an empty command"))?;
    if let Some(dir) = spec.stderr_log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let stderr = std::fs::File::create(&spec.stderr_log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut child = Command::new(program)
        .args(&spec.argv[1..])
        .current_dir(&spec.cwd)
        .envs(spec.env.iter().cloned())
        // An agent is a fresh session, even when towerd was started from
        // inside a Claude Code session.
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_CHILD_SESSION")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start {program}: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let mut conn = Conn {
        stdin: child.stdin.take().expect("piped stdin"),
        next_id: 0,
        pending: HashMap::new(),
    };
    conn.request(
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "tower", "version": env!("CARGO_PKG_VERSION")},
        }),
        Pending::Init,
    )
    .await?;

    let mut session: Option<String> = None;
    let mut settings: VecDeque<(String, String)> = VecDeque::new();
    let mut waiting_option = false;
    let mut ready = false;
    let mut in_turn = false;
    let mut queued: VecDeque<String> = VecDeque::new();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let line = match line? {
                    Some(l) => l,
                    None => break,
                };
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                trace("<-", &msg);
                if let Some(method) = msg.get("method").and_then(Value::as_str) {
                    match (msg.get("id"), method) {
                        (Some(id), "session/request_permission") => {
                            emit(parse_permission(id.clone(), msg.get("params").unwrap_or(&Value::Null)));
                        }
                        (Some(id), _) => {
                            // We advertised no client capabilities beyond permissions.
                            conn.write(&json!({"jsonrpc": "2.0", "id": id,
                                "error": {"code": -32601, "message": "method not supported by Tower"}})).await?;
                        }
                        (None, "session/update") => {
                            if let Some(ev) = msg.pointer("/params/update").and_then(parse_update) {
                                emit(ev);
                            }
                        }
                        (None, _) => {}
                    }
                    continue;
                }
                let id = match msg.get("id").and_then(Value::as_u64) {
                    Some(id) => id,
                    None => continue,
                };
                let failed = msg.get("error").is_some();
                match conn.pending.remove(&id) {
                    Some(Pending::Init) => {
                        if failed {
                            anyhow::bail!("initialize failed: {}", error_text(&msg));
                        }
                        if spec.api_key_auth {
                            let methods = msg.pointer("/result/authMethods").cloned().unwrap_or(Value::Null);
                            let Some(method) = pick_api_key_method(&methods) else {
                                anyhow::bail!(
                                    "the role says auth = \"api_key\", but this agent offers no API-key sign-in"
                                );
                            };
                            conn.request("authenticate", json!({"methodId": method}), Pending::Authenticate)
                                .await?;
                            continue;
                        }
                        conn.request(
                            "session/new",
                            json!({"cwd": spec.cwd.display().to_string(), "mcpServers": spec.mcp.clone()}),
                            Pending::NewSession,
                        )
                        .await?;
                    }
                    Some(Pending::Authenticate) => {
                        if failed {
                            anyhow::bail!("API-key sign-in failed: {}", error_text(&msg));
                        }
                        conn.request(
                            "session/new",
                            json!({"cwd": spec.cwd.display().to_string(), "mcpServers": spec.mcp.clone()}),
                            Pending::NewSession,
                        )
                        .await?;
                    }
                    Some(Pending::NewSession) => {
                        if failed {
                            anyhow::bail!("could not open a session: {}", error_text(&msg));
                        }
                        let sid = msg.pointer("/result/sessionId").and_then(Value::as_str).unwrap_or("").to_string();
                        session = Some(sid.clone());
                        let offered: Vec<String> = msg
                            .pointer("/result/configOptions")
                            .and_then(Value::as_array)
                            .map(|o| o.iter().filter_map(|x| x.get("id").and_then(Value::as_str).map(str::to_string)).collect())
                            .unwrap_or_default();
                        // Model first, then the role's options, one at a time.
                        if let Some(model) = &spec.model {
                            if offered.iter().any(|o| o == "model") {
                                settings.push_back(("model".into(), model.clone()));
                            } else {
                                conn.request(
                                    "session/set_model",
                                    json!({"sessionId": sid, "modelId": model}),
                                    Pending::SetOption("model".into()),
                                )
                                .await?;
                                waiting_option = true;
                            }
                        }
                        settings.extend(spec.options.iter().cloned());
                        settings.extend(spec.optional.iter().filter(|(id, _)| offered.contains(id)).cloned());
                    }
                    Some(Pending::SetOption(id)) => {
                        waiting_option = false;
                        if failed {
                            emit(AcpEvent::Error(format!("could not set {id}: {}", error_text(&msg))));
                            if id == "model" && spec.model_required {
                                emit(AcpEvent::Error(
                                    "stopped before the first prompt: it would have run on the agent's own default model".into(),
                                ));
                                break;
                            }
                        }
                    }
                    Some(Pending::Prompt) => {
                        in_turn = false;
                        if failed {
                            emit(AcpEvent::Error(error_text(&msg)));
                            emit(AcpEvent::TurnEnd { stop_reason: "error".into(), usage: None });
                        } else {
                            let reason = msg.pointer("/result/stopReason").and_then(Value::as_str).unwrap_or("end_turn");
                            emit(AcpEvent::TurnEnd {
                                stop_reason: reason.to_string(),
                                usage: msg.pointer("/result/usage").and_then(parse_turn_usage),
                            });
                        }
                    }
                    None => {}
                }
                if !ready && !waiting_option {
                    if let Some(sid) = session.clone() {
                        match settings.pop_front() {
                            Some((id, value)) => {
                                conn.request(
                                    "session/set_config_option",
                                    json!({"sessionId": sid, "configId": id, "value": value}),
                                    Pending::SetOption(id),
                                )
                                .await?;
                                waiting_option = true;
                            }
                            None => {
                                ready = true;
                                emit(AcpEvent::Ready);
                            }
                        }
                    }
                }
                if ready && !in_turn {
                    if let (Some(sid), Some(text)) = (session.clone(), queued.pop_front()) {
                        in_turn = true;
                        conn.request(
                            "session/prompt",
                            json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]}),
                            Pending::Prompt,
                        )
                        .await?;
                    }
                }
            }
            cmd = cmds.recv() => {
                match cmd {
                    None | Some(AcpCmd::Stop) => break,
                    Some(AcpCmd::Prompt(text)) => {
                        match (&session, ready && !in_turn) {
                            (Some(sid), true) => {
                                in_turn = true;
                                let sid = sid.clone();
                                conn.request(
                                    "session/prompt",
                                    json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]}),
                                    Pending::Prompt,
                                )
                                .await?;
                            }
                            _ => queued.push_back(text),
                        }
                    }
                    Some(AcpCmd::Cancel) => {
                        if let Some(sid) = &session {
                            let sid = sid.clone();
                            conn.write(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}})).await?;
                        }
                    }
                    Some(AcpCmd::Permission { rpc_id, option_id }) => {
                        let outcome = match option_id {
                            Some(id) => json!({"outcome": "selected", "optionId": id}),
                            None => json!({"outcome": "cancelled"}),
                        };
                        conn.write(&json!({"jsonrpc": "2.0", "id": rpc_id, "result": {"outcome": outcome}})).await?;
                    }
                }
            }
        }
    }
    let _ = child.kill().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_methods_are_found_by_id_name_or_description() {
        let m = json!([
            {"id": "chatgpt", "name": "Sign in with ChatGPT"},
            {"id": "openai-api-key", "name": "Use OPENAI_API_KEY"}
        ]);
        assert_eq!(pick_api_key_method(&m).as_deref(), Some("openai-api-key"));
        let m = json!([{"id": "k", "name": "Key", "description": "Uses your API key from the environment"}]);
        assert_eq!(pick_api_key_method(&m).as_deref(), Some("k"));
        assert_eq!(
            pick_api_key_method(&json!([{"id": "oauth-personal", "name": "Log in with Google"}])),
            None
        );
        assert_eq!(pick_api_key_method(&Value::Null), None);
    }

    /// A fake ACP agent offering `methods`; writes the chosen sign-in to `log`.
    fn fake(dir: &std::path::Path, methods: &str, log: &std::path::Path) -> Vec<String> {
        let script = dir.join("fake_auth.py");
        std::fs::write(
            &script,
            format!(
                r#"import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m or "method" not in m: continue
    i, meth = m["id"], m["method"]
    def out(x): sys.stdout.write(json.dumps(x) + "\n"); sys.stdout.flush()
    if meth == "initialize": out({{"jsonrpc": "2.0", "id": i, "result": {{"protocolVersion": 1, "authMethods": {methods}}}}})
    elif meth == "authenticate":
        open({log:?}, "w").write(m["params"]["methodId"])
        out({{"jsonrpc": "2.0", "id": i, "result": {{}}}})
    elif meth == "session/new": out({{"jsonrpc": "2.0", "id": i, "result": {{"sessionId": "s"}}}})
    elif meth == "session/prompt":
        out({{"jsonrpc": "2.0", "method": "session/update", "params": {{"sessionId": "s", "update": {{"sessionUpdate": "agent_message_chunk", "content": {{"type": "text", "text": "ok"}}}}}}}})
        out({{"jsonrpc": "2.0", "id": i, "result": {{"stopReason": "end_turn"}}}})
    else: out({{"jsonrpc": "2.0", "id": i, "result": {{}}}})
"#,
                log = log.display().to_string()
            ),
        )
        .unwrap();
        vec!["python3".into(), "-I".into(), script.display().to_string()]
    }

    fn spec(argv: Vec<String>, dir: &std::path::Path, api_key_auth: bool) -> AcpSpec {
        AcpSpec {
            argv,
            cwd: dir.to_path_buf(),
            env: vec![],
            model: None,
            options: vec![],
            optional: vec![],
            mcp: vec![],
            stderr_log: dir.join("stderr.log"),
            api_key_auth,
            model_required: false,
        }
    }

    #[tokio::test]
    async fn api_key_roles_pick_the_api_key_sign_in_and_others_pick_none() {
        let d = std::env::temp_dir().join(format!("tower-acp-auth-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        let log = d.join("chosen");
        let both = r#"[{"id": "chatgpt", "name": "Sign in with ChatGPT"}, {"id": "openai-api-key", "name": "API key"}]"#;
        let t = std::time::Duration::from_secs(20);

        let r = ask_once(spec(fake(&d, both, &log), &d, true), "hi".into(), t).await;
        assert_eq!(r.unwrap().0, "ok");
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "openai-api-key");

        // A plan-login role: Tower triggers no sign-in at all.
        std::fs::remove_file(&log).unwrap();
        let r = ask_once(spec(fake(&d, both, &log), &d, false), "hi".into(), t).await;
        assert!(r.is_ok());
        assert!(!log.exists(), "no authenticate call");

        // api_key declared, but the agent only has a plan login: refused.
        let plan_only = r#"[{"id": "oauth", "name": "Log in"}]"#;
        let r = ask_once(spec(fake(&d, plan_only, &log), &d, true), "hi".into(), t).await;
        assert!(r.unwrap_err().contains("no API-key sign-in"));
        assert!(!log.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn updates_map_to_events() {
        let u = json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"type": "text", "text": "pon"}});
        assert_eq!(
            parse_update(&u),
            Some(AcpEvent::Text {
                message_id: Some("m1".into()),
                text: "pon".into()
            })
        );
        let u = json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "edit src/a.rs", "status": "pending"});
        assert_eq!(
            parse_update(&u),
            Some(AcpEvent::ToolCall {
                id: "t1".into(),
                title: "edit src/a.rs".into()
            })
        );
        let u = json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "in_progress"});
        assert_eq!(parse_update(&u), None);
        let u =
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "failed"});
        assert_eq!(
            parse_update(&u),
            Some(AcpEvent::ToolDone {
                id: "t1".into(),
                failed: true
            })
        );
        assert_eq!(
            parse_update(&json!({"sessionUpdate": "usage_update"})),
            None
        );
    }

    #[test]
    fn permission_requests_keep_their_options() {
        let p = json!({"sessionId": "s", "toolCall": {"toolCallId": "t", "title": "bash: git stash"},
            "options": [{"optionId": "a", "name": "Allow", "kind": "allow_once"},
                        {"optionId": "r", "name": "Reject", "kind": "reject_once"}]});
        match parse_permission(json!(7), &p) {
            AcpEvent::Permission {
                rpc_id,
                title,
                options,
                input,
            } => {
                assert!(input.contains("git stash"));
                assert_eq!(rpc_id, json!(7));
                assert_eq!(title, "bash: git stash");
                assert_eq!(options.len(), 2);
                assert_eq!(options[1].kind, "reject_once");
            }
            other => panic!("{other:?}"),
        }
    }
}
