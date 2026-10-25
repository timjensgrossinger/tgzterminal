//! The connection to towerd. One per GUI process, shared by every Tower pane.
//!
//! A worker thread connects (starting the daemon if needed), forwards server
//! messages into each subscribed pane's model and asks the mux to repaint that
//! pane. Terminal work the daemon asks for (open a pane next to Tower, type
//! into an agent's pane, focus, close) runs on the GUI thread through the mux,
//! so it works even while the Tower tab is not visible.

use super::model::{Conn, Model};
use super::pane::now_ms;
use super::proto::{ClientMsg, ServerMsg};
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use mux::domain::SplitSource;
use mux::pane::{PaneId, SpawnTitlePolicy};
use mux::tab::{SplitDirection, SplitRequest, SplitSize};
use mux::{Mux, MuxNotification};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock, Weak};

struct Client {
    tx: Mutex<Option<Sender<ClientMsg>>>,
    subs: Mutex<Vec<(PaneId, Weak<Mutex<Model>>)>>,
    /// Last known connection state, so late subscribers start with it.
    conn: Mutex<Conn>,
    /// Panes opened for daemon agents; their removal is reported back.
    spawned: Mutex<HashSet<PaneId>>,
    /// The newest agent pane: the next one stacks below it.
    last_agent_pane: Mutex<Option<PaneId>>,
}

static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();

fn client() -> &'static Arc<Client> {
    CLIENT.get_or_init(|| {
        let c = Arc::new(Client {
            tx: Mutex::new(None),
            subs: Mutex::new(Vec::new()),
            conn: Mutex::new(Conn::Connecting),
            spawned: Mutex::new(HashSet::new()),
            last_agent_pane: Mutex::new(None),
        });
        if let Some(mux) = Mux::try_get() {
            mux.subscribe(|n| {
                if let MuxNotification::PaneRemoved(id) = n {
                    if let Some(c) = CLIENT.get() {
                        if c.spawned.lock().remove(&id) {
                            send(ClientMsg::PaneExited { pane_id: id as u64 });
                        }
                    }
                }
                true
            });
        }
        let worker = Arc::clone(&c);
        std::thread::Builder::new()
            .name("tgz-tower-client".into())
            .spawn(move || imp::run(worker))
            .expect("spawn tower client thread");
        c
    })
}

pub fn subscribe(pane_id: PaneId, model: &Arc<Mutex<Model>>) {
    let c = client();
    let conn = c.conn.lock().clone();
    model.lock().set_conn(conn);
    c.subs.lock().push((pane_id, Arc::downgrade(model)));
    // Fresh data for the new pane.
    send(ClientMsg::GetSnapshot);
}

/// Queue a message; dropped while disconnected (the model already said so).
pub fn send(msg: ClientMsg) {
    if let Some(tx) = client().tx.lock().as_ref() {
        let _ = tx.send(msg);
    }
}

fn each_sub(c: &Client, mut f: impl FnMut(&mut Model)) {
    let mut subs = c.subs.lock();
    subs.retain(|(_, w)| w.strong_count() > 0);
    for (pane_id, weak) in subs.iter() {
        if let Some(model) = weak.upgrade() {
            f(&mut model.lock());
            Mux::notify_from_any_thread(MuxNotification::PaneOutput(*pane_id));
        }
    }
}

/// A Tower pane to place new splits next to.
fn tower_pane(c: &Client) -> Option<PaneId> {
    let subs = c.subs.lock();
    let mux = Mux::try_get()?;
    subs.iter()
        .filter(|(_, w)| w.strong_count() > 0)
        .map(|(id, _)| *id)
        .find(|id| mux.get_pane(*id).is_some())
}

fn set_conn(c: &Client, conn: Conn) {
    *c.conn.lock() = conn.clone();
    each_sub(c, |m| m.set_conn(conn.clone()));
}

fn deliver(c: &Client, msg: ServerMsg) {
    match msg {
        ServerMsg::SpawnPane {
            request_id,
            argv,
            cwd,
            env,
            title,
        } => {
            let tower = tower_pane(c);
            // The mux split future is not Send: start it from the GUI thread.
            promise::spawn::spawn_into_main_thread(async move {
                promise::spawn::spawn(async move {
                    let result = match tower {
                        Some(t) => split_next_to(t, Some(argv), cwd, env, Some(title), true).await,
                        None => Err(anyhow::anyhow!("no Tower tab is open")),
                    };
                    let (pane_id, error) = match result {
                        Ok(p) => (Some(p as u64), None),
                        Err(e) => (None, Some(format!("{:#}", e))),
                    };
                    send(ClientMsg::PaneSpawned {
                        request_id,
                        pane_id,
                        error,
                    });
                })
                .detach();
            })
            .detach();
        }
        ServerMsg::SendToPane {
            pane_id,
            text,
            submit,
        } => {
            promise::spawn::spawn_into_main_thread(async move {
                type_into(pane_id as PaneId, &text, submit);
            })
            .detach();
        }
        ServerMsg::FocusPane { pane_id } => focus(pane_id),
        ServerMsg::ClosePane { pane_id } => {
            promise::spawn::spawn_into_main_thread(async move {
                if let Some(mux) = Mux::try_get() {
                    mux.remove_pane(pane_id as PaneId);
                }
            })
            .detach();
        }
        other => {
            if let ServerMsg::Hello { daemon_pid, .. } = &other {
                *c.conn.lock() = Conn::Live { pid: *daemon_pid };
            }
            let now = now_ms();
            each_sub(c, |m| m.apply_server(other.clone(), now));
        }
    }
}

/// Paste `text` into a pane like the composer does, then press Enter. The
/// Enter waits a moment: an Enter right behind a bracketed paste is
/// swallowed by Claude Code's paste handling and the message just sits there.
fn type_into(pane_id: PaneId, text: &str, submit: bool) {
    let mux = match Mux::try_get() {
        Some(m) => m,
        None => return,
    };
    let pane = match mux.get_pane(pane_id) {
        Some(p) => p,
        None => return,
    };
    if pane.send_paste(text).is_err() || !submit {
        return;
    }
    promise::spawn::spawn(async move {
        smol::Timer::after(std::time::Duration::from_millis(250)).await;
        let _ = pane.writer().write_all(b"\r");
    })
    .detach();
}

pub fn focus(pane_id: u64) {
    promise::spawn::spawn_into_main_thread(async move {
        if let Some(mux) = Mux::try_get() {
            let _ = mux.focus_pane_and_containing_tab(pane_id as PaneId);
        }
    })
    .detach();
}

/// A plain split next to Tower (Open terminal, Edit role).
pub fn open_split(tower: PaneId, argv: Option<Vec<String>>, cwd: Option<String>) {
    promise::spawn::spawn_into_main_thread(async move {
        promise::spawn::spawn(async move {
            if let Err(e) = split_next_to(tower, argv, cwd, vec![], None, false).await {
                log::error!("tgz_tower: split failed: {:#}", e);
            }
        })
        .detach();
    })
    .detach();
}

/// Where a new pane goes: `(split target, below it?, size %)`. The first
/// agent pane opens right of Tower; each later one stacks below the newest
/// agent pane still in Tower's tab, so the board keeps its width.
pub fn placement(tower: PaneId, prev_agent_in_tab: Option<PaneId>) -> (PaneId, bool, u8) {
    match prev_agent_in_tab {
        Some(p) => (p, true, 50),
        None => (tower, false, 42),
    }
}

/// Split next to Tower. Agent panes (`stack`) go right of Tower first, then
/// stack below the newest agent pane in the same tab.
async fn split_next_to(
    tower: PaneId,
    argv: Option<Vec<String>>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    title: Option<String>,
    stack: bool,
) -> anyhow::Result<PaneId> {
    let mux = Mux::try_get().ok_or_else(|| anyhow::anyhow!("no mux"))?;
    let (_, _, tower_tab) = mux
        .resolve_pane_id(tower)
        .ok_or_else(|| anyhow::anyhow!("the Tower pane is gone"))?;
    let prev = if stack {
        *client().last_agent_pane.lock()
    } else {
        None
    };
    let prev = prev.filter(|p| {
        mux.resolve_pane_id(*p)
            .map(|(_, _, t)| t == tower_tab)
            .unwrap_or(false)
    });
    let (target, below, percent) = placement(tower, prev);
    let direction = if below {
        SplitDirection::Vertical
    } else {
        SplitDirection::Horizontal
    };
    if let Some(tab) = mux.get_tab(tower_tab) {
        // Splitting a zoomed tab fails; flatten it first.
        tab.set_zoomed(false);
    }
    let spawn = SpawnCommand {
        args: argv,
        cwd: cwd.map(PathBuf::from),
        set_environment_variables: env.into_iter().collect(),
        domain: SpawnTabDomain::CurrentPaneDomain,
        label: title.clone(),
        ..Default::default()
    };
    let (command, command_dir) = crate::spawn::command_builder_for(&spawn)?;
    let (pane, _size) = mux
        .split_pane(
            target,
            SplitRequest {
                direction,
                target_is_second: true,
                top_level: false,
                size: SplitSize::Percent(percent),
            },
            SplitSource::Spawn {
                command,
                command_dir,
            },
            SpawnTabDomain::CurrentPaneDomain,
        )
        .await?;
    pane.set_config(Arc::new(config::TermConfig::with_config(
        config::configuration(),
    )));
    if let Some(t) = &title {
        pane.set_spawn_title(t, SpawnTitlePolicy::WhileForeground);
    }
    let id = pane.pane_id();
    if stack {
        *client().last_agent_pane.lock() = Some(id);
        client().spawned.lock().insert(id);
    }
    Ok(id)
}

pub(super) use imp::daemon_binary;

mod imp {
    //! The connection itself. Platform specifics are three small functions
    //! (`endpoint`, `open`, `spawn_daemon`): a Unix socket next to the token
    //! file, or a named pipe with the token in the data directory on Windows.

    use super::super::proto::{decode_line, encode_line, ClientMsg, ServerMsg, PROTOCOL_VERSION};
    use super::{deliver, set_conn, Client, Conn};
    use mux::Mux;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A connected stream that can be split into a reader and a writer.
    pub trait Stream: Read + Write + Send + Sized + 'static {
        fn dup(&self) -> std::io::Result<Self>;
    }

    #[cfg(unix)]
    impl Stream for std::os::unix::net::UnixStream {
        fn dup(&self) -> std::io::Result<Self> {
            self.try_clone()
        }
    }

    #[cfg(windows)]
    impl Stream for std::fs::File {
        fn dup(&self) -> std::io::Result<Self> {
            self.try_clone()
        }
    }

    fn data_dir() -> PathBuf {
        config::DATA_DIR.join("tower")
    }

    /// `(endpoint, master token file)`.
    #[cfg(unix)]
    fn endpoint() -> (PathBuf, PathBuf) {
        let sock = data_dir().join("towerd.sock");
        let mut token = sock.as_os_str().to_owned();
        token.push(".token");
        (sock, PathBuf::from(token))
    }

    #[cfg(windows)]
    fn endpoint() -> (PathBuf, PathBuf) {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let user: String = user.chars().filter(|c| c.is_alphanumeric()).collect();
        (
            PathBuf::from(format!(r"\\.\pipe\tgz-towerd-{}", user)),
            data_dir().join("towerd.token"),
        )
    }

    #[cfg(unix)]
    fn open(sock: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
        std::os::unix::net::UnixStream::connect(sock)
    }

    #[cfg(windows)]
    fn open(sock: &Path) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(sock)
    }

    /// Where the daemon binary is: `TGZ_TOWERD`, else the experiment's own
    /// target dir next to this checkout (debug first, it's what you iterate on).
    pub(in super::super) fn daemon_binary() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("TGZ_TOWERD") {
            return Some(PathBuf::from(p));
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("tgz-tower")
            .join("target");
        let exe = format!("tower{}", std::env::consts::EXE_SUFFIX);
        ["debug", "release"]
            .iter()
            .map(|p| root.join(p).join(&exe))
            .find(|p| p.exists())
    }

    fn spawn_daemon(sock: &Path) -> anyhow::Result<()> {
        let bin = daemon_binary().ok_or_else(|| {
            anyhow::anyhow!("towerd not built (cargo build --manifest-path tgz-tower/Cargo.toml)")
        })?;
        let data = data_dir();
        std::fs::create_dir_all(&data)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("towerd.log"))?;
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("daemon")
            .arg("--socket")
            .arg(sock)
            .arg("--data-dir")
            .arg(&data)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group: a Ctrl-C in the launching terminal must
            // not take the daemon (and the agents it supervises) down.
            cmd.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        }
        cmd.spawn()?;
        log::debug!("tgz_tower: started towerd");
        Ok(())
    }

    fn connect(sock: &Path) -> anyhow::Result<impl Stream> {
        if let Ok(s) = open(sock) {
            return Ok(s);
        }
        spawn_daemon(sock)?;
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(s) = open(sock) {
                return Ok(s);
            }
            if Instant::now() > deadline {
                anyhow::bail!("towerd did not start (see towerd.log)");
            }
        }
    }

    fn session(c: &Arc<Client>) -> anyhow::Result<()> {
        let (sock, token_file) = endpoint();
        let stream = connect(&sock)?;
        // The token is written right after the daemon starts listening.
        let mut token = String::new();
        for _ in 0..20 {
            if let Ok(t) = std::fs::read_to_string(&token_file) {
                token = t.trim().to_string();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut writer = stream.dup()?;
        writer.write_all(
            encode_line(&ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                token,
                client: "gui".into(),
            })?
            .as_bytes(),
        )?;
        // Tell the daemon which panes exist, so agents whose pane closed
        // while nobody was watching are marked offline.
        if let Some(mux) = Mux::try_get() {
            let ids = mux
                .iter_panes()
                .iter()
                .map(|p| p.pane_id() as u64)
                .collect();
            writer.write_all(encode_line(&ClientMsg::LivePanes { pane_ids: ids })?.as_bytes())?;
        }

        let (tx, rx) = std::sync::mpsc::channel::<ClientMsg>();
        *c.tx.lock() = Some(tx);
        std::thread::Builder::new()
            .name("tgz-tower-writer".into())
            .spawn(move || {
                for msg in rx {
                    let line = match encode_line(&msg) {
                        Ok(l) => l,
                        Err(_) => continue,
                    };
                    if writer.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                }
            })?;

        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line?;
            match decode_line::<ServerMsg>(&line) {
                Ok(msg) => deliver(c, msg),
                Err(err) => log::debug!("tgz_tower: undecodable server line: {}", err),
            }
        }
        *c.tx.lock() = None;
        Ok(())
    }

    pub fn run(c: Arc<Client>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            set_conn(&c, Conn::Connecting);
            let started = Instant::now();
            let why = match session(&c) {
                Ok(()) => "towerd closed the connection".to_string(),
                Err(err) => format!("{:#}", err),
            };
            *c.tx.lock() = None;
            set_conn(&c, Conn::Offline(why));
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_secs(1);
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::placement;

    #[test]
    fn first_agent_goes_right_of_tower_then_they_stack() {
        assert_eq!(placement(1, None), (1, false, 42));
        assert_eq!(placement(1, Some(5)), (5, true, 50));
    }
}
