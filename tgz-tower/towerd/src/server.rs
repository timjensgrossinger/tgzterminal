//! The local endpoint: hello + token, then requests in and replies,
//! snapshots and GUI commands out. A Unix socket (0600) on Unix, a named pipe
//! on Windows.
//!
//! Two kinds of credentials:
//! - the master token (a 0600 file next to the socket) for the Tower view and
//!   the CLI: full access;
//! - a per-agent token, handed to each agent's `tower hook` / `tower mcp`
//!   through its environment: only that agent's hook events and tool calls.
//!   An agent therefore cannot release its own handoffs or approve its own
//!   landing through Tower's protocol.

use crate::engine::{AgentTokens, Engine, Input};
use crate::paths::Paths;
use anyhow::Context;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use tower_proto::{decode_line, encode_line, ClientMsg, ServerMsg, Snapshot, PROTOCOL_VERSION};

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Shared {
    tx: mpsc::UnboundedSender<Input>,
    snaps: watch::Receiver<Arc<Snapshot>>,
    master: Arc<String>,
    /// For `tower mcp` in ordinary panes: only ever `Access::Pane`.
    pane: Arc<String>,
    agents: AgentTokens,
}

/// Who is on the other end of a connection.
#[derive(Clone, Debug, PartialEq)]
enum Access {
    /// The Tower view or the CLI.
    Full,
    /// One agent's shims: hook events and tool calls for this agent only.
    Agent(String),
    /// The `tower mcp` server of a CLI in an ordinary pane: the delegation
    /// tools only. It shows the master token (it runs as you, like the
    /// Tower view), but asks for no more than this.
    Pane,
}

/// Write the master token and start the engine.
fn start(paths: &Paths, demo: bool) -> anyhow::Result<Shared> {
    std::fs::create_dir_all(&paths.data)
        .with_context(|| format!("create {}", paths.data.display()))?;
    let fresh = || {
        format!(
            "{:016x}{:016x}",
            crate::paths::random_u64(),
            crate::paths::random_u64()
        )
    };
    let token = fresh();
    write_secret(&paths.token(), &token)?;
    let pane = fresh();
    write_secret(&paths.pane_token(), &pane)?;
    let agents = AgentTokens::default();
    let (tx, rx) = mpsc::unbounded_channel();
    let (engine, snaps) = Engine::new(paths.clone(), demo, tx.clone(), Arc::clone(&agents));
    tokio::spawn(engine.run(rx));
    eprintln!("tower: listening on {}", paths.socket.display());
    Ok(Shared {
        tx,
        snaps,
        master: Arc::new(token),
        pane: Arc::new(pane),
        agents,
    })
}

/// Write a token file only its owner can read, from the first byte on.
fn write_secret(path: &std::path::Path, text: &str) -> anyhow::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(())
}

fn spawn_conn<R, W>(r: R, w: W, shared: &Shared)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let shared = shared.clone();
    tokio::spawn(async move {
        if let Err(err) = serve(r, w, shared).await {
            eprintln!("tower: connection ended: {err:#}");
        }
    });
}

#[cfg(unix)]
pub async fn run(paths: Paths, demo: bool) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::{UnixListener, UnixStream};

    std::fs::create_dir_all(&paths.data)
        .with_context(|| format!("create {}", paths.data.display()))?;
    if UnixStream::connect(&paths.socket).await.is_ok() {
        eprintln!(
            "tower: a daemon is already running on {}",
            paths.socket.display()
        );
        return Ok(());
    }
    let _ = std::fs::remove_file(&paths.socket);
    let listener = UnixListener::bind(&paths.socket)
        .with_context(|| format!("bind {}", paths.socket.display()))?;
    std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;
    let shared = start(&paths, demo)?;

    let cleanup = paths.clone();
    tokio::spawn(async move {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { if let Some(t) = term.as_mut() { t.recv().await; } else { std::future::pending::<()>().await } } => {}
        }
        let _ = std::fs::remove_file(&cleanup.socket);
        let _ = std::fs::remove_file(cleanup.token());
        std::process::exit(0);
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let (r, w) = stream.into_split();
        spawn_conn(r, w, &shared);
    }
}

#[cfg(windows)]
pub async fn run(paths: Paths, demo: bool) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let name = paths.socket.display().to_string();
    // The first instance claims the name; a second daemon fails here.
    let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("tower: a daemon is already running on {name}");
            return Ok(());
        }
        Err(e) => return Err(e).with_context(|| format!("create pipe {name}")),
    };
    let shared = start(&paths, demo)?;
    let cleanup = paths.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = std::fs::remove_file(cleanup.token());
        std::process::exit(0);
    });
    loop {
        server.connect().await?;
        // Hand the connected instance off and open the next one.
        let connected = std::mem::replace(&mut server, ServerOptions::new().create(&name)?);
        let (r, w) = tokio::io::split(connected);
        spawn_conn(r, w, &shared);
    }
}

async fn send<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &ServerMsg) -> anyhow::Result<()> {
    w.write_all(encode_line(msg)?.as_bytes()).await?;
    Ok(())
}

/// Decide what a hello is allowed to do.
fn authorize(token: &str, client: &str, shared: &Shared) -> Option<Access> {
    if token == shared.master.as_str() {
        return Some(if client == "pane" {
            Access::Pane
        } else {
            Access::Full
        });
    }
    // The pane token is good for nothing else, whatever the client says.
    if token == shared.pane.as_str() {
        return (client == "pane").then_some(Access::Pane);
    }
    if client == "hook" || client == "mcp" {
        let agents = shared.agents.read().ok()?;
        return agents.get(token).map(|a| Access::Agent(a.clone()));
    }
    None
}

/// Agents may only report about themselves; anything else is refused.
fn scope(msg: ClientMsg, access: &Access) -> Result<ClientMsg, String> {
    match (access, msg) {
        (Access::Full, msg) => Ok(msg),
        (Access::Agent(id), ClientMsg::Hook { event, payload, .. }) => Ok(ClientMsg::Hook {
            agent_id: id.clone(),
            event,
            payload,
        }),
        (Access::Agent(id), ClientMsg::Tool { name, args, .. }) => Ok(ClientMsg::Tool {
            agent_id: id.clone(),
            name,
            args,
        }),
        (Access::Agent(_), ClientMsg::Ping) => Ok(ClientMsg::Ping),
        (Access::Agent(_), _) => Err("agents may only send hook events and tool calls".into()),
        (Access::Pane, msg @ ClientMsg::PaneTool { .. }) => Ok(msg),
        (Access::Pane, ClientMsg::Ping) => Ok(ClientMsg::Ping),
        (Access::Pane, _) => Err("a pane agent may only call the delegation tools".into()),
    }
}

async fn serve<R, W>(r: R, mut w: W, shared: Shared) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut lines = BufReader::new(r).lines();
    let first = lines.next_line().await?.context("closed before hello")?;
    let (client, access) = match decode_line::<ClientMsg>(&first)? {
        ClientMsg::Hello {
            version,
            token,
            client,
        } => {
            if version != PROTOCOL_VERSION {
                send(&mut w, &ServerMsg::Error {
                    message: format!("protocol {version} unsupported, towerd speaks {PROTOCOL_VERSION}; restart towerd"),
                })
                .await?;
                return Ok(());
            }
            match authorize(&token, &client, &shared) {
                Some(a) => (client, a),
                None => {
                    send(
                        &mut w,
                        &ServerMsg::Error {
                            message: "bad hello".into(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        _ => {
            send(
                &mut w,
                &ServerMsg::Error {
                    message: "bad hello".into(),
                },
            )
            .await?;
            return Ok(());
        }
    };
    send(
        &mut w,
        &ServerMsg::Hello {
            version: PROTOCOL_VERSION,
            daemon_pid: std::process::id(),
        },
    )
    .await?;

    let full = access == Access::Full;
    let is_gui = full && client == "gui";
    let wants_snapshots = full && (client == "gui" || client == "cli");
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let tx = shared.tx.clone();
    let mut snaps = shared.snaps.clone();
    let (gui_tx, mut gui_rx) = mpsc::unbounded_channel::<ServerMsg>();
    if is_gui {
        let _ = tx.send(Input::GuiUp { conn, tx: gui_tx });
    } else {
        drop(gui_tx);
    }
    if wants_snapshots {
        let snap = snaps.borrow_and_update().clone();
        send(
            &mut w,
            &ServerMsg::Snapshot {
                snapshot: (*snap).clone(),
            },
        )
        .await?;
    }

    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { return Ok(()) };
                    let reply = match decode_line::<ClientMsg>(&line).map_err(|e| format!("bad message: {e}")).and_then(|m| scope(m, &access)) {
                        Ok(msg) => {
                            let (rtx, rrx) = oneshot::channel();
                            if tx.send(Input::Client { msg, reply: rtx }).is_err() {
                                return Ok(());
                            }
                            rrx.await.ok().flatten()
                        }
                        Err(message) => Some(ServerMsg::Error { message }),
                    };
                    // GUI clients learn about results from the next snapshot;
                    // only errors are worth a reply line.
                    match reply {
                        Some(ServerMsg::Ack) if is_gui => {}
                        Some(reply) => send(&mut w, &reply).await?,
                        None => {}
                    }
                }
                changed = snaps.changed(), if wants_snapshots => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let snap = snaps.borrow_and_update().clone();
                    send(&mut w, &ServerMsg::Snapshot { snapshot: (*snap).clone() }).await?;
                }
                cmd = gui_rx.recv(), if is_gui => {
                    match cmd {
                        Some(cmd) => send(&mut w, &cmd).await?,
                        None => return Ok(()),
                    }
                }
            }
        }
    }
    .await;
    if is_gui {
        let _ = tx.send(Input::GuiDown { conn });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shared() -> Shared {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_s, snaps) = watch::channel(Arc::new(tower_proto::demo_snapshot(0)));
        let agents = AgentTokens::default();
        agents
            .write()
            .unwrap()
            .insert("agent-tok".into(), "ground-2".into());
        Shared {
            tx,
            snaps,
            master: Arc::new("master".into()),
            pane: Arc::new("pane-tok".into()),
            agents,
        }
    }

    #[test]
    fn tokens_grant_full_or_scoped_access() {
        let s = shared();
        assert_eq!(authorize("master", "gui", &s), Some(Access::Full));
        assert_eq!(
            authorize("agent-tok", "mcp", &s),
            Some(Access::Agent("ground-2".into()))
        );
        assert_eq!(
            authorize("agent-tok", "gui", &s),
            None,
            "an agent token is not a GUI token"
        );
        assert_eq!(authorize("nope", "hook", &s), None);
    }

    #[test]
    fn agents_only_speak_for_themselves() {
        let a = Access::Agent("ground-2".into());
        let forged = ClientMsg::Tool {
            agent_id: "approach-1".into(),
            name: "tower_complete".into(),
            args: json!({}),
        };
        match scope(forged, &a).unwrap() {
            ClientMsg::Tool { agent_id, .. } => assert_eq!(agent_id, "ground-2"),
            other => panic!("{other:?}"),
        }
        let approve = ClientMsg::ResolveInbox {
            item_id: "i-1".into(),
            approve: true,
            option_id: None,
            text: None,
        };
        assert!(
            scope(approve.clone(), &a).is_err(),
            "no approving your own clearances"
        );
        assert!(scope(approve, &Access::Full).is_ok());
    }

    #[test]
    fn pane_agents_get_the_delegation_tools_and_nothing_else() {
        let s = shared();
        assert_eq!(authorize("master", "pane", &s), Some(Access::Pane));
        assert_eq!(authorize("pane-tok", "pane", &s), Some(Access::Pane));
        assert_eq!(
            authorize("pane-tok", "cli", &s),
            None,
            "never more than a pane"
        );
        assert_eq!(authorize("pane-tok", "gui", &s), None);
        assert_eq!(authorize("nope", "pane", &s), None);
        let call = ClientMsg::PaneTool {
            agent: tower_proto::PaneAgentInfo::default(),
            name: "tower_delegate".into(),
            args: json!({}),
        };
        assert!(scope(call.clone(), &Access::Pane).is_ok());
        assert!(
            scope(call, &Access::Agent("ground-2".into())).is_err(),
            "a Tower agent cannot pose as a pane"
        );
        for msg in [
            ClientMsg::ConnectAgents { on: true },
            ClientMsg::SetArmed { on: true },
            ClientMsg::Tool {
                agent_id: "approach-1".into(),
                name: "tower_complete".into(),
                args: json!({}),
            },
        ] {
            assert!(scope(msg, &Access::Pane).is_err());
        }
    }
}
