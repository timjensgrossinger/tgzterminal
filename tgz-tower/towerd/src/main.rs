//! `tower`: the Tower engine daemon and its agent-side shims.
//!
//! ```text
//! tower daemon --socket <path> [--config-dir <dir>] [--demo]
//! tower status --socket <path>
//! tower hook <event>    (run by Claude Code hooks; reads the payload on stdin)
//! tower mcp             (stdio MCP server with the team tools)
//! tower notify <json>   (run by Codex's `notify` at the end of a turn)
//! ```
//! `hook` and `mcp` find the daemon through TOWER_SOCK / TOWER_AGENT_ID,
//! which the daemon sets when it launches an agent.

mod audit;
mod claude_trust;
mod cli;
mod clock;
mod compliance;
mod config;
mod config_write;
mod connect;
mod drivers;
mod engine;
mod gates;
mod history;
mod issues;
mod jira;
mod local_guard;
mod mcp_http;
mod paths;
mod policy;
mod privacy;
mod projects;
mod prompt;
mod responses;
mod routines;
mod routing;
mod schedule;
mod scout;
mod server;
mod shim;
mod sla;
mod store;
mod transport;
mod usage;

use anyhow::{bail, Context};
use std::path::PathBuf;

/// `tower auth jira <site>`: store an Atlassian API token for a site.
fn auth(args: &[String]) -> anyhow::Result<()> {
    use std::io::{BufRead, IsTerminal};
    let (Some(kind), Some(site)) = (args.first(), args.get(1)) else {
        usage()
    };
    if kind != "jira" {
        bail!("only `tower auth jira <site>` exists so far");
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("Paste the Atlassian API token for {site} and press Enter (create one at id.atlassian.com > Security > API tokens):");
    }
    let mut token = String::new();
    stdin.lock().read_line(&mut token)?;
    let token = token.trim();
    if token.is_empty() {
        bail!("no token given");
    }
    let data = flag(args, "--data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(paths::default_data_dir);
    jira::secrets::set(site, token, &data)?;
    eprintln!("Stored. Add a [[jira]] watch for {site} to sources.toml; Tower polls it within its interval.");
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage: tower daemon [--socket <path>] [--config-dir <dir>] [--data-dir <dir>] [--demo]\n       \
         tower status [--socket <path>]\n       tower flight new|list|show …\n       tower inbox list|answer …\n       tower routines [--config-dir <dir>]\n       tower auth jira <site>\n       tower hook <event>\n       tower mcp\n       tower guard prompt|tool|dir [<folder> [<cli>]]\n\n{}",
        cli::USAGE
    );
    std::process::exit(2);
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
    }
    None
}

fn socket_arg(args: &[String]) -> anyhow::Result<PathBuf> {
    if let Some(p) = flag(args, "--socket") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("TOWER_SOCK") {
        return Ok(PathBuf::from(p));
    }
    Ok(paths::default_socket())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { usage() };
    let rest = &args[1..];
    match cmd.as_str() {
        // The shims stay synchronous and start fast.
        "hook" => return shim::hook(rest.first().map(String::as_str).unwrap_or("")),
        "mcp" => return shim::mcp(),
        "notify" => return shim::notify(rest.last().map(String::as_str).unwrap_or("")),
        "guard" => return local_guard::run(rest),
        _ => {}
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    match cmd.as_str() {
        "daemon" => {
            let socket = socket_arg(rest)?;
            let paths = paths::Paths::new(
                socket,
                flag(rest, "--config-dir").map(PathBuf::from),
                flag(rest, "--data-dir").map(PathBuf::from),
            );
            rt.block_on(server::run(paths, rest.iter().any(|a| a == "--demo")))
        }
        "status" => status(rest),
        "flight" => cli::flight(rest),
        "inbox" => cli::inbox(rest),
        "routines" => routines_cmd(rest),
        "auth" => auth(rest),
        _ => usage(),
    }
}

/// `tower routines`: Tower's schedules and this machine's launchd/cron
/// jobs, read locally (no daemon needed).
fn routines_cmd(args: &[String]) -> anyhow::Result<()> {
    let paths = paths::Paths::new(
        paths::default_socket(),
        flag(args, "--config-dir").map(PathBuf::from),
        None,
    );
    let tz = clock::local_tz();
    let now = paths::now_unix();
    let when = |t: Option<u64>| match t {
        Some(t) => {
            let c = clock::civil(t, &tz);
            format!(
                "next {} {} {:02}:{:02}",
                clock::weekday_name(t, &tz),
                clock::day_key(t, &tz),
                c.hour,
                c.minute
            )
        }
        None => String::new(),
    };
    println!(
        "Tower schedules ({}):",
        paths.config.join(schedule::FILE).display()
    );
    match schedule::load(&paths.config) {
        Ok(list) if list.is_empty() => println!("  none"),
        Ok(list) => {
            for s in list {
                let (text, next) = match schedule::Timing::of(&s) {
                    Ok(t) => (t.describe(), when(t.next(now, None, &tz))),
                    Err(e) => (format!("can't read: {e}"), String::new()),
                };
                let off = if s.enabled { "" } else { " (paused)" };
                println!("  {} [{}]{off}: {text}  {next}", s.name, s.id);
            }
        }
        Err(e) => println!("  {e}"),
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let found = routines::collect(&home, now);
    println!("This machine:");
    if !found.note.is_empty() {
        println!("  {}", found.note);
    }
    for r in found.rows {
        let exit = r
            .last_exit
            .map(|x| format!(" exit {x}"))
            .unwrap_or_default();
        let stalled = if r.stalled { " STALLED" } else { "" };
        println!(
            "  [{}] {}: {}{exit}{stalled}  {}",
            r.source,
            r.label,
            r.schedule_text,
            when(r.next_fire_unix)
        );
    }
    Ok(())
}

/// `tower status`: one line about the running daemon.
fn status(args: &[String]) -> anyhow::Result<()> {
    let socket = socket_arg(args)?;
    let paths = paths::Paths::new(
        socket.clone(),
        None,
        flag(args, "--data-dir").map(PathBuf::from),
    );
    let token = std::fs::read_to_string(paths.token())
        .with_context(|| format!("read {}", paths.token().display()))?;
    let snapshot = transport::snapshot(&socket, &token, std::time::Duration::from_secs(5))
        .with_context(|| format!("connect {}", socket.display()))?;
    let live = snapshot
        .agents
        .iter()
        .filter(|a| a.status.is_live())
        .count();
    println!(
        "{} roles · {} live agents · {} flights · {} in inbox ({:?})",
        snapshot.roles.len(),
        live,
        snapshot.flights.len(),
        snapshot.inbox.len(),
        snapshot.origin
    );
    for r in &snapshot.roles {
        if let Some(w) = &r.warning {
            println!("  {}: {w}", r.name);
        }
    }
    for t in &snapshot.setup.terms_pending {
        println!("  terms: {} (accept it in Setup)", t.title);
    }
    for s in &snapshot.sources {
        if let Some(e) = &s.error {
            println!("  intake {}: {e}", s.name);
        }
    }
    Ok(())
}
