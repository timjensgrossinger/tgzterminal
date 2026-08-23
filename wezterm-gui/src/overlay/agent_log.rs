use crate::agent_herd::HerdAgent;
use mux::termwiztermtab::TermWizTerminal;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use termwiz::cell::{AttributeChange, CellAttributes, Intensity};
use termwiz::color::AnsiColor;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::surface::Change;
use termwiz::terminal::Terminal;

/// How many events the overlay keeps appended within a session.
const MAX_RENDERED_EVENTS: usize = 400;

/// A scroll-back log of one agent's activity, shown in a full overlay pane
/// (same hosting as the debug overlay) so it never fights the sidebar for room.
///
/// The header is painted once; thereafter only genuinely new events are appended
/// to the terminal, so refreshing never scrolls the prior frames into the
/// scrollback (which is what made the old version repeat the whole log).
pub fn show_agent_log_overlay(mut term: TermWizTerminal, agent: HerdAgent) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();
    term.render(&[Change::Title(format!("Agent log — {}", agent.name))])?;

    let provider = agent.provider.clone();
    let session_id = agent.session_id.clone();
    let cwd = agent.cwd.clone();

    // State carried across refreshes: the rendered header, and the signature of
    // the last event we appended so we only write what is new.
    let mut header_lines: Option<usize> = None;
    let mut last_event_key: Option<(String, String)> = None;

    let mut last_render = Instant::now() - Duration::from_secs(2);
    let refresh_interval = Duration::from_millis(750);

    loop {
        let now = Instant::now();
        if now.duration_since(last_render) >= refresh_interval {
            let live = refresh_activity(&provider, session_id.as_deref(), cwd.as_deref());
            render_log(
                &mut term,
                &agent,
                live.as_ref(),
                &mut header_lines,
                &mut last_event_key,
            )?;
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
                render_log(
                    &mut term,
                    &agent,
                    live.as_ref(),
                    &mut header_lines,
                    &mut last_event_key,
                )?;
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
    if provider != "claude" && provider != "opencode" {
        return None;
    }
    let (session_id, cwd) = match (session_id, cwd) {
        (Some(s), Some(c)) => (s.to_string(), c.to_path_buf()),
        _ => return None,
    };
    match provider {
        "claude" => {
            let home = dirs_home()?;
            let path =
                crate::agent_herd::claude::session_transcript_path(&home, &cwd, &session_id)?;
            Some(crate::agent_herd::transcript::read_activity(&path, 500))
        }
        "opencode" => crate::agent_herd::opencode::read_session_activity(&session_id, 500),
        _ => None,
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// A human `HH:MM:SS` timestamp, or empty when the source gave none.
fn fmt_time(at: Option<SystemTime>) -> String {
    at.map(|at| {
        let secs = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
        format!("{h:02}:{m:02}:{s:02} ")
    })
    .unwrap_or_default()
}

/// Strip injected system context from user-side text so it does not pollute the
/// log. The `<memory_context>` / `<system-reminder>` blocks wezterm passes to
/// agents are not the user's words.
fn clean_event_text(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("<memory_context>")
            || trimmed.starts_with("<system-reminder>")
            || trimmed.starts_with("<project_knowledge>")
        {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    let out = out.trim();
    // One screen line each; keep it short and readable.
    if out.chars().count() > 160 {
        format!("{}…", out.chars().take(157).collect::<String>())
    } else {
        out.to_string()
    }
}

/// A stable key for dedupe: kind + cleaned text.
fn event_key(event: &crate::agent_herd::HerdEvent) -> (String, String) {
    (
        event.kind.label().to_string(),
        clean_event_text(&event.display_text()),
    )
}

fn render_log(
    term: &mut TermWizTerminal,
    agent: &HerdAgent,
    live: Option<&crate::agent_herd::HerdActivity>,
    header_lines: &mut Option<usize>,
    last_event_key: &mut Option<(String, String)>,
) -> termwiz::Result<()> {
    let mut changes: Vec<Change> = Vec::new();

    // Paint the header only on the first frame.
    if header_lines.is_none() {
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
        let header_line_count = header.matches('\n').count();
        changes.push(Change::Text(header));
        *header_lines = Some(header_line_count);
        *last_event_key = None;
    }

    let activity = live.or_else(|| agent.activity.as_ref());
    match activity {
        None => {
            if header_lines.is_some() && last_event_key.is_none() {
                changes.push(Change::Text("(no activity recorded)\r\n".to_string()));
                *last_event_key = Some(("none".to_string(), String::new()));
            }
        }
        Some(activity) => {
            // Newest first reads better for "what is it doing right now".
            let mut shown: Vec<&crate::agent_herd::HerdEvent> = Vec::new();
            if let Some(current) = activity.current.as_ref() {
                shown.push(current);
            }
            for event in activity.recent.iter().rev() {
                shown.push(event);
            }
            let mut appended = 0;
            for event in shown {
                let key = event_key(event);
                if Some(&key) == last_event_key.as_ref() {
                    continue;
                }
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
                    fmt_time(event.at),
                    event.kind.label(),
                    clean_event_text(&event.display_text())
                );
                changes.push(Change::Text(text.replace('\n', "\r\n")));
                *last_event_key = Some(key);
                appended += 1;
                if appended >= MAX_RENDERED_EVENTS {
                    break;
                }
            }
            changes.push(Change::AllAttributes(CellAttributes::default()));
        }
    }

    // Nothing new to draw this refresh: skip the render entirely so the cursor
    // does not jump and the prior text stays put.
    if changes.is_empty() {
        return Ok(());
    }
    term.render(&changes)
}
