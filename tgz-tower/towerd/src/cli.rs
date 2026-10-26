//! `tower flight …` and `tower inbox …`: the Tower view's actions from a
//! shell, for scripts, Routines and machines without the terminal. They
//! send the same messages the view sends. ACP and llm roles start with no
//! Tower tab open; a pane role still needs one.

use crate::{flag, paths, socket_arg, transport};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tower_proto::{ClientMsg, Flight, InboxItem, ServerMsg, Snapshot, Stage, TimelineBody};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Exit codes: 1 the daemon said no, 2 usage, 3 no daemon reachable.
const EXIT_DAEMON: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_CONNECT: i32 = 3;

pub const USAGE: &str = "\
tower flight new --title <t> [--brief <b> | --brief-file <f> | --brief -] [--role <id>]
                 [--tier low|medium|high] [--here | --new-folder | --worktree] [--workdir <dir>]
                 [--local-only] [--no-launch] [--json]
tower flight list [--all] [--json]
tower flight show <key|id> [--json]
tower inbox list [--json]
tower inbox answer <id> <approve|deny|option-id> [--text <answer>]
  all: [--socket <path>] [--data-dir <dir>]";

fn usage(why: &str) -> ! {
    if !why.is_empty() {
        eprintln!("tower: {why}");
    }
    eprintln!("usage:\n{USAGE}");
    std::process::exit(EXIT_USAGE);
}

pub fn flight(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first() else { usage("") };
    let rest = &args[1..];
    match sub.as_str() {
        "new" => flight_new(rest),
        "list" => flight_list(rest),
        "show" => flight_show(rest),
        other => usage(&format!("unknown flight command {other:?}")),
    }
}

pub fn inbox(args: &[String]) -> anyhow::Result<()> {
    let Some(sub) = args.first() else { usage("") };
    let rest = &args[1..];
    match sub.as_str() {
        "list" => inbox_list(rest),
        "answer" => inbox_answer(rest),
        other => usage(&format!("unknown inbox command {other:?}")),
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

struct Conn {
    socket: PathBuf,
    token: String,
}

impl Conn {
    fn open(args: &[String]) -> Conn {
        let socket = match socket_arg(args) {
            Ok(s) => s,
            Err(e) => usage(&e.to_string()),
        };
        let paths = paths::Paths::new(
            socket.clone(),
            None,
            flag(args, "--data-dir").map(PathBuf::from),
        );
        let token = match std::fs::read_to_string(paths.token()) {
            Ok(t) => t,
            Err(_) => Self::unreachable(&socket),
        };
        Conn { socket, token }
    }

    fn unreachable(socket: &Path) -> ! {
        eprintln!(
            "tower: no Tower daemon at {}: open the Tower tab in TGZTerminal, or run `tower daemon`",
            socket.display()
        );
        std::process::exit(EXIT_CONNECT);
    }

    /// Send one message; a daemon error ends the command with exit 1.
    fn call(&self, msg: &ClientMsg) -> ServerMsg {
        match transport::call(&self.socket, &self.token, msg, TIMEOUT) {
            Ok(ServerMsg::Error { message }) => {
                eprintln!("tower: {message}");
                std::process::exit(EXIT_DAEMON);
            }
            Ok(reply) => reply,
            Err(_) => Self::unreachable(&self.socket),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match transport::snapshot(&self.socket, &self.token, TIMEOUT) {
            Ok(s) => s,
            Err(_) => Self::unreachable(&self.socket),
        }
    }
}

fn json<T: serde::Serialize>(v: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

// ---------------------------------------------------------------------------
// flight new
// ---------------------------------------------------------------------------

/// Flags that take a value, so the parser knows what to skip.
const VALUE_FLAGS: [&str; 8] = [
    "--title",
    "--brief",
    "--brief-file",
    "--role",
    "--tier",
    "--workdir",
    "--socket",
    "--data-dir",
];
const BOOL_FLAGS: [&str; 6] = [
    "--here",
    "--new-folder",
    "--worktree",
    "--local-only",
    "--no-launch",
    "--json",
];

/// `tower flight new …` as a `CreateFlight`. `cwd` is "Here"; `read` gets
/// the brief from a file, or from stdin for `-`.
pub fn parse_new(
    args: &[String],
    cwd: &Path,
    read: &dyn Fn(&str) -> Result<String, String>,
) -> Result<ClientMsg, String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if VALUE_FLAGS.contains(&a) {
            if i + 1 >= args.len() {
                return Err(format!("{a} needs a value"));
            }
            i += 2;
        } else if BOOL_FLAGS.contains(&a) {
            i += 1;
        } else {
            return Err(format!("unknown argument {a:?}"));
        }
    }
    let title = flag(args, "--title")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or("--title is required")?;
    let brief = match (flag(args, "--brief"), flag(args, "--brief-file")) {
        (Some(_), Some(_)) => return Err("give --brief or --brief-file, not both".into()),
        (Some(b), None) if b == "-" => read("-")?,
        (Some(b), None) => b,
        (None, Some(f)) => read(&f)?,
        (None, None) => String::new(),
    };
    let tier = flag(args, "--tier");
    if let Some(t) = &tier {
        if !["low", "medium", "high"].contains(&t.as_str()) {
            return Err(format!("--tier must be low, medium or high, not {t:?}"));
        }
    }
    let wheres = ["--here", "--new-folder", "--worktree"]
        .iter()
        .filter(|w| has(args, w))
        .count();
    if wheres > 1 {
        return Err("give one of --here, --new-folder, --worktree".into());
    }
    let new_folder = has(args, "--new-folder");
    let worktree = has(args, "--worktree");
    let workdir = if new_folder {
        None
    } else {
        Some(
            flag(args, "--workdir")
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf())
                .display()
                .to_string(),
        )
    };
    let role_id = flag(args, "--role").filter(|r| !r.trim().is_empty());
    Ok(ClientMsg::CreateFlight {
        title,
        brief,
        auto_assign: role_id.is_none(),
        role_id,
        workdir,
        worktree,
        new_folder,
        launch: !has(args, "--no-launch"),
        tier,
        local_only: has(args, "--local-only"),
    })
}

fn read_brief(from: &str) -> Result<String, String> {
    use std::io::Read;
    let mut s = String::new();
    if from == "-" {
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| format!("stdin: {e}"))?;
    } else {
        s = std::fs::read_to_string(from).map_err(|e| format!("{from}: {e}"))?;
    }
    Ok(s)
}

fn flight_new(args: &[String]) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let msg = match parse_new(args, &cwd, &read_brief) {
        Ok(m) => m,
        Err(e) => usage(&e),
    };
    let conn = Conn::open(args);
    match conn.call(&msg) {
        ServerMsg::Created { id, key } => {
            if has(args, "--json") {
                let snap = conn.snapshot();
                match snap.flights.iter().find(|f| f.id == id) {
                    Some(f) => json(f)?,
                    None => json(&serde_json::json!({ "id": id, "key": key }))?,
                }
            } else {
                println!("{key}  {id}");
                // Say what happened to it right away (held on a terms note,
                // waiting for a role, …) instead of leaving that to `show`.
                let snap = conn.snapshot();
                if let Some(f) = snap.flights.iter().find(|f| f.id == id) {
                    if let Some(note) = last_note(f) {
                        println!("{}: {note}", f.stage.label());
                    }
                }
            }
            Ok(())
        }
        other => anyhow::bail!("unexpected reply {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// flight list / show
// ---------------------------------------------------------------------------

fn flight_list(args: &[String]) -> anyhow::Result<()> {
    let conn = Conn::open(args);
    let snap = conn.snapshot();
    let all = has(args, "--all");
    let flights: Vec<&Flight> = snap
        .flights
        .iter()
        .filter(|f| all || f.stage != Stage::Landed)
        .collect();
    if has(args, "--json") {
        return json(&flights);
    }
    if flights.is_empty() {
        println!(
            "no open flights{}",
            if all {
                ""
            } else {
                " (--all shows landed ones)"
            }
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = flights
        .iter()
        .map(|f| {
            let waiting = snap
                .inbox
                .iter()
                .filter(|i| i.flight_id.as_deref() == Some(f.id.as_str()))
                .count();
            vec![
                f.key.clone(),
                f.stage.label().to_string(),
                f.role_id.clone().unwrap_or_default(),
                if waiting > 0 {
                    waiting.to_string()
                } else {
                    String::new()
                },
                f.title.clone(),
            ]
        })
        .collect();
    print!(
        "{}",
        table(&["KEY", "STAGE", "ROLE", "WAITING", "TITLE"], &rows)
    );
    Ok(())
}

fn find_flight<'a>(snap: &'a Snapshot, what: &str) -> Option<&'a Flight> {
    snap.flights
        .iter()
        .find(|f| f.id == what || f.key.eq_ignore_ascii_case(what))
}

fn flight_show(args: &[String]) -> anyhow::Result<()> {
    let Some(what) = args.first().filter(|a| !a.starts_with("--")) else {
        usage("flight show needs a key or id")
    };
    let conn = Conn::open(args);
    let snap = conn.snapshot();
    let Some(f) = find_flight(&snap, what) else {
        eprintln!("tower: no flight {what:?}");
        std::process::exit(EXIT_DAEMON);
    };
    if has(args, "--json") {
        return json(f);
    }
    println!("{}  {}  {}", f.key, f.stage.label(), f.title);
    println!("  id       {}", f.id);
    if let Some(r) = &f.role_id {
        println!("  role     {r}");
    }
    if let Some(a) = &f.agent_id {
        println!("  agent    {a}");
    }
    if let Some(r) = &f.route {
        let engine = r.engine.as_deref().unwrap_or("-");
        println!(
            "  route    {} tier · {engine} · effort {}",
            r.tier, r.effort
        );
    }
    if let Some(w) = &f.workdir {
        println!("  workdir  {w}");
    }
    if f.local_only {
        println!("  local-only: cloud models never see this work");
    }
    println!(
        "  usage    {} in · {} out{}",
        f.usage.input_tokens,
        f.usage.output_tokens,
        f.usage
            .cost_usd
            .map(|c| format!(" · ${c:.2}"))
            .unwrap_or_default()
    );
    let notes: Vec<String> = f.timeline.iter().filter_map(note_text).collect();
    if !notes.is_empty() {
        println!("timeline (last {}):", notes.len().min(10));
        for n in notes.iter().rev().take(10).rev() {
            println!("  {n}");
        }
    }
    let waiting: Vec<&InboxItem> = snap
        .inbox
        .iter()
        .filter(|i| i.flight_id.as_deref() == Some(f.id.as_str()))
        .collect();
    if !waiting.is_empty() {
        println!("waiting on you:");
        for i in waiting {
            println!("  {}  {}  [{}]", i.id, i.title, options_text(i));
        }
    }
    Ok(())
}

fn note_text(t: &tower_proto::TimelineEntry) -> Option<String> {
    Some(match &t.body {
        TimelineBody::System { text } => text.clone(),
        TimelineBody::Message { from, text } => format!("{from}: {}", shorten(text, 160)),
        TimelineBody::ToolCall { agent, summary, .. } => format!("{agent}: {summary}"),
        TimelineBody::Gate { gate } => format!("gate {}: {:?}", gate.name, gate.state),
        TimelineBody::Handoff { packet } => format!("handoff {} → {}", packet.id, packet.to_role),
    })
}

fn last_note(f: &Flight) -> Option<String> {
    f.timeline.iter().rev().find_map(note_text)
}

fn shorten(s: &str, n: usize) -> String {
    tower_core::shorten(s, n)
}

// ---------------------------------------------------------------------------
// inbox list / answer
// ---------------------------------------------------------------------------

/// What the user can answer: option ids, or approve/deny when there are none.
fn options_text(i: &InboxItem) -> String {
    if i.options.is_empty() {
        return "approve/deny".into();
    }
    i.options
        .iter()
        .map(|o| o.id.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn inbox_list(args: &[String]) -> anyhow::Result<()> {
    let conn = Conn::open(args);
    let snap = conn.snapshot();
    if has(args, "--json") {
        return json(&snap.inbox);
    }
    if snap.inbox.is_empty() {
        println!("nothing waiting on you");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = snap
        .inbox
        .iter()
        .map(|i| {
            let flight = i
                .flight_id
                .as_deref()
                .and_then(|id| snap.flights.iter().find(|f| f.id == id))
                .map(|f| f.key.clone())
                .unwrap_or_default();
            vec![
                i.id.clone(),
                i.kind.label().to_string(),
                flight,
                options_text(i),
                i.title.clone(),
            ]
        })
        .collect();
    print!(
        "{}",
        table(&["ID", "KIND", "FLIGHT", "OPTIONS", "TITLE"], &rows)
    );
    Ok(())
}

/// `tower inbox answer <id> <choice> [--text …]` as a `ResolveInbox`:
/// `approve`/`deny` are the buttons an item without options has, anything
/// else is one of its option ids.
pub fn parse_answer(args: &[String]) -> Result<ClientMsg, String> {
    // Positional words: not a flag, and not the value of one.
    let mut words: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if VALUE_FLAGS.contains(&a) || a == "--text" {
            i += 2;
        } else if a.starts_with("--") {
            i += 1;
        } else {
            words.push(&args[i]);
            i += 1;
        }
    }
    let item_id = words
        .first()
        .ok_or("inbox answer needs an item id")?
        .to_string();
    let choice = words
        .get(1)
        .ok_or("inbox answer needs a choice: approve, deny or an option id")?
        .to_string();
    let text = flag(args, "--text");
    let (approve, option_id) = match choice.as_str() {
        "approve" | "yes" | "accept" | "run" => (true, None),
        "deny" | "no" | "reject" | "not_now" => (false, None),
        other => (true, Some(other.to_string())),
    };
    Ok(ClientMsg::ResolveInbox {
        item_id,
        approve,
        option_id,
        text,
    })
}

fn inbox_answer(args: &[String]) -> anyhow::Result<()> {
    let msg = match parse_answer(args) {
        Ok(m) => m,
        Err(e) => usage(&e),
    };
    let conn = Conn::open(args);
    match conn.call(&msg) {
        ServerMsg::Ack => {
            println!("ok");
            Ok(())
        }
        ServerMsg::ToolResult { text, .. } => {
            println!("{text}");
            Ok(())
        }
        other => anyhow::bail!("unexpected reply {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Aligned columns; the last column is not padded.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let cols = header.len();
    let mut width = vec![0usize; cols];
    for (i, h) in header.iter().enumerate() {
        width[i] = h.chars().count();
    }
    for r in rows {
        for (i, c) in r.iter().enumerate().take(cols) {
            width[i] = width[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cols {
                out.push_str(c);
            } else {
                out.push_str(&format!("{c:<w$}  ", w = width[i]));
            }
        }
        out.trim_end().to_string() + "\n"
    };
    let mut out = line(header.to_vec());
    for r in rows {
        out.push_str(&line(r.iter().map(String::as_str).collect()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn flight_new_args_map_to_create_flight() {
        let cwd = Path::new("/work/here");
        let read = |_: &str| Ok("from file".to_string());
        let m = parse_new(
            &args("--title Fix --brief b --role crew --tier high --local-only --no-launch"),
            cwd,
            &read,
        )
        .unwrap();
        assert_eq!(
            m,
            ClientMsg::CreateFlight {
                title: "Fix".into(),
                brief: "b".into(),
                role_id: Some("crew".into()),
                workdir: Some("/work/here".into()),
                worktree: false,
                new_folder: false,
                auto_assign: false,
                launch: false,
                tier: Some("high".into()),
                local_only: true,
            }
        );
        // No role: Auto. New folder: no workdir. Brief from a file or stdin.
        let m = parse_new(
            &args("--title T --brief-file notes.md --new-folder"),
            cwd,
            &read,
        )
        .unwrap();
        assert!(matches!(
            &m,
            ClientMsg::CreateFlight { brief, auto_assign: true, role_id: None, workdir: None, new_folder: true, launch: true, .. }
            if brief == "from file"
        ));
        let m = parse_new(
            &args("--title T --brief - --worktree --workdir /repo"),
            cwd,
            &read,
        )
        .unwrap();
        assert!(matches!(
            &m,
            ClientMsg::CreateFlight { brief, worktree: true, workdir: Some(w), .. }
            if brief == "from file" && w == "/repo"
        ));
        for bad in [
            "--brief b",
            "--title T --tier huge",
            "--title T --here --new-folder",
            "--title T --brief a --brief-file f",
            "--title T --bogus",
            "--title",
        ] {
            assert!(parse_new(&args(bad), cwd, &read).is_err(), "{bad}");
        }
    }

    #[test]
    fn inbox_answer_maps_choice_to_option_or_bool() {
        let m = parse_answer(&args("i-3 approve")).unwrap();
        assert_eq!(
            m,
            ClientMsg::ResolveInbox {
                item_id: "i-3".into(),
                approve: true,
                option_id: None,
                text: None
            }
        );
        let m = parse_answer(&args("i-3 deny --socket /tmp/s")).unwrap();
        assert!(matches!(
            m,
            ClientMsg::ResolveInbox {
                approve: false,
                option_id: None,
                ..
            }
        ));
        let m = parse_answer(&args("--socket /tmp/s i-4 accept --text looks_good")).unwrap();
        assert!(matches!(
            &m,
            ClientMsg::ResolveInbox { item_id, approve: true, option_id: None, text: Some(t) }
            if item_id == "i-4" && t == "looks_good"
        ));
        let m = parse_answer(&args("i-5 allow_always")).unwrap();
        assert!(matches!(
            &m,
            ClientMsg::ResolveInbox { approve: true, option_id: Some(o), .. } if o == "allow_always"
        ));
        assert!(parse_answer(&args("i-5")).is_err());
        assert!(parse_answer(&args("")).is_err());
    }

    #[test]
    fn tables_align_every_column_but_the_last() {
        let t = table(
            &["KEY", "TITLE"],
            &[
                vec!["TWR-1".into(), "short".into()],
                vec!["TWR-12".into(), "a longer title that is not padded".into()],
            ],
        );
        assert_eq!(
            t,
            "KEY     TITLE\nTWR-1   short\nTWR-12  a longer title that is not padded\n"
        );
    }
}
