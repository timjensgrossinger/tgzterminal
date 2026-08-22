use crate::agent_herd::HerdAgent;
use mux::termwiztermtab::TermWizTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use termwiz::cell::{AttributeChange, CellAttributes, Intensity};
use termwiz::color::AnsiColor;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::Change;
use termwiz::terminal::Terminal;

/// A scroll-back log of one agent's activity, shown in a full overlay pane
/// (same hosting as the debug overlay) so it never fights the sidebar for room.
///
/// The agent snapshot is captured at open time. For Claude sessions we re-read
/// the transcript on a timer and on `r`, so the view stays live while the agent
/// runs; other vendors show the captured snapshot.
pub fn show_agent_log_overlay(mut term: TermWizTerminal, agent: HerdAgent) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    term.render(&[Change::Title(format!("Agent log — {}", agent.name))])?;

    let provider = agent.provider.clone();
    let session_id = agent.session_id.clone();
    let cwd = agent.cwd.clone();

    let mut last_render = Instant::now() - Duration::from_secs(2);
    let refresh_interval = Duration::from_millis(750);

    loop {
        let now = Instant::now();
        if now.duration_since(last_render) >= refresh_interval {
            let live = refresh_activity(&provider, session_id.as_deref(), cwd.as_deref());
            render_log(&mut term, &agent, live.as_ref())?;
            last_render = now;
        }

        match term.poll_input(Some(Duration::from_millis(300)))? {
            Some(InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            }))
            | Some(InputEvent::Key(KeyEvent {
                key: KeyCode::Char('q'),
                ..
            })) => return Ok(()),
            Some(InputEvent::Key(KeyEvent {
                key: KeyCode::Char('r'),
                ..
            })) => {
                let live = refresh_activity(&provider, session_id.as_deref(), cwd.as_deref());
                render_log(&mut term, &agent, live.as_ref())?;
                last_render = Instant::now();
            }
            _ => {}
        }
    }
}

/// Re-read the most recent activity for a live agent, where the vendor lets us.
fn refresh_activity(
    provider: &str,
    session_id: Option<&str>,
    cwd: Option<&std::path::Path>,
) -> Option<crate::agent_herd::HerdActivity> {
    if provider != "claude" {
        return None;
    }
    let (session_id, cwd) = match (session_id, cwd) {
        (Some(s), Some(c)) => (s.to_string(), c.to_path_buf()),
        _ => return None,
    };
    let home = dirs_home()?;
    let path = crate::agent_herd::claude::session_transcript_path(&home, &cwd, &session_id)?;
    Some(crate::agent_herd::transcript::read_activity(&path, 500))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn render_log(
    term: &mut TermWizTerminal,
    agent: &HerdAgent,
    live: Option<&crate::agent_herd::HerdActivity>,
) -> termwiz::Result<()> {
    let activity = live.or_else(|| agent.activity.as_ref());
    let mut changes: Vec<Change> =
        vec![Change::ClearScreen(termwiz::color::ColorAttribute::Default)];

    let status = agent.display_status(std::time::SystemTime::now()).label();
    let mut header = format!(
        "Agent: {}  [{status}]\r\nprovider: {}",
        agent.name,
        agent.vendor.label()
    );
    if let Some(model) = agent.model.as_deref() {
        header.push_str(&format!("  model: {model}"));
    }
    if let Some(root) = agent.project_root.as_ref() {
        header.push_str(&format!("\r\nproject: {}", root.display()));
    }
    if !agent.subagents.is_empty() {
        header.push_str(&format!("\r\nsubagents: {}", agent.subagents.len()));
    }
    header.push_str("\r\npress r to refresh . q or ESC to close\r\n\r\n");
    changes.push(Change::Text(header));

    match activity {
        None => changes.push(Change::Text("(no activity recorded)\r\n".to_string())),
        Some(activity) => {
            // Newest first reads better for "what is it doing right now".
            let mut shown = vec![];
            if let Some(current) = activity.current.as_ref() {
                shown.push((true, current));
            }
            for event in activity.recent.iter().rev() {
                shown.push((false, event));
            }
            for (is_current, event) in shown {
                let when = event.at.map(|at| format!("{:?} ", at)).unwrap_or_default();
                changes.push(
                    AttributeChange::Intensity(if is_current {
                        Intensity::Bold
                    } else {
                        Intensity::Normal
                    })
                    .into(),
                );
                let color = match event.kind {
                    crate::agent_herd::HerdEventKind::Tool => AnsiColor::Blue,
                    crate::agent_herd::HerdEventKind::Assistant => AnsiColor::Fuchsia,
                    crate::agent_herd::HerdEventKind::Notice => AnsiColor::White,
                    crate::agent_herd::HerdEventKind::Thinking => AnsiColor::Maroon,
                    crate::agent_herd::HerdEventKind::ToolResult => AnsiColor::Green,
                    crate::agent_herd::HerdEventKind::SubagentSpawn => AnsiColor::Yellow,
                };
                changes.push(AttributeChange::Foreground(color.into()).into());
                let text = format!(
                    "{}{} {}\r\n",
                    when,
                    event.kind.label(),
                    event.display_text()
                );
                changes.push(Change::Text(text.replace('\n', "\r\n")));
            }
            changes.push(Change::AllAttributes(CellAttributes::default()));
        }
    }

    term.render(&changes)
}
