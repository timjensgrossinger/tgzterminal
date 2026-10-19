//! `tower`: the Tower engine daemon and its agent-side shims.
//!
//! ```text
//! tower daemon --socket <path> [--config-dir <dir>] [--demo]
//! tower status --socket <path>
//! tower hook <event>    (run by Claude Code hooks; reads the payload on stdin)
//! tower mcp             (stdio MCP server with the team tools)
//! ```
//! `hook` and `mcp` find the daemon through TOWER_SOCK / TOWER_AGENT_ID,
//! which the daemon sets when it launches an agent.

mod claude_trust;
mod config;
mod drivers;
mod engine;
mod gates;
mod issues;
mod jira;
mod mcp_http;
mod paths;
mod projects;
mod prompt;
mod routing;
mod server;
mod shim;
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
         tower status [--socket <path>]\n       tower auth jira <site>\n       tower hook <event>\n       tower mcp"
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
        "auth" => auth(rest),
        _ => usage(),
    }
}

/// `tower status`: one line about the running daemon.
fn status(args: &[String]) -> anyhow::Result<()> {
    use tower_proto::ServerMsg;
    let socket = socket_arg(args)?;
    let paths = paths::Paths::new(
        socket.clone(),
        None,
        flag(args, "--data-dir").map(PathBuf::from),
    );
    let token = std::fs::read_to_string(paths.token())
        .with_context(|| format!("read {}", paths.token().display()))?;
    let (mut r, _w) = transport::hello(&socket, &token, "cli", std::time::Duration::from_secs(5))
        .with_context(|| format!("connect {}", socket.display()))?;
    loop {
        match transport::read_msg(&mut r)? {
            ServerMsg::Snapshot { snapshot } => {
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
                for s in &snapshot.sources {
                    if let Some(e) = &s.error {
                        println!("  intake {}: {e}", s.name);
                    }
                }
                return Ok(());
            }
            ServerMsg::Error { message } => bail!("towerd: {message}"),
            _ => {}
        }
    }
}
