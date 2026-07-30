//! The agent herd overview: a full-pane list of every agent this terminal can
//! see, its subagents, and a Stop control.
//!
//! Runs as a `TermWizTerminal` overlay, which means it occupies a pane, gets all
//! input, and — importantly — runs on its own thread. Filesystem polling for
//! Claude sessions therefore never touches the render path.
//!
//! The one hard constraint: **this thread must not touch the mux.** Pane data is
//! requested from the GUI thread via [`TermWindowNotif::Apply`] and arrives over
//! a channel; see [`request_pane_rows`].

use crate::agent_herd::{
    claude, group_by_project, join_sessions_with_panes, HerdAgent, HerdGroup, HerdStatus, HerdView,
    PaneAgentRow,
};
use crate::termwindow::TermWindowNotif;
use mux::termwiztermtab::TermWizTerminal;
use mux::Mux;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};
use termwiz::cell::{unicode_column_width, AttributeChange, CellAttributes, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::input::{InputEvent, KeyCode, KeyEvent, Modifiers, MouseButtons, MouseEvent};
use termwiz::surface::{Change, Position};
use termwiz::terminal::Terminal;
use window::WindowOps;

/// Left margin, in cells. Keeps the list off the pane edge.
const MARGIN: usize = 2;
/// Indent for subagent and continuation rows.
const SUB_INDENT: usize = 6;
/// Rendered width of the Stop control, including its brackets.
const STOP_LABEL: &str = " Stop ";

/// Colors resolved on the GUI thread and handed to the overlay, so the overlay
/// thread never reads config.
#[derive(Clone, Debug)]
pub struct HerdTheme {
    /// Adapter id → dot color.
    pub adapter_colors: HashMap<String, (u8, u8, u8)>,
    /// Color for agents waiting on the human.
    pub attention: (u8, u8, u8),
    pub dim: (u8, u8, u8),
    pub accent: (u8, u8, u8),
}

impl HerdTheme {
    fn provider_color(&self, provider: &str) -> ColorAttribute {
        self.adapter_colors
            .get(provider)
            .copied()
            .map(rgb)
            .unwrap_or_else(|| rgb(self.dim))
    }

    fn status_color(&self, status: HerdStatus, provider: &str) -> ColorAttribute {
        match status {
            HerdStatus::Blocked => rgb(self.attention),
            HerdStatus::Working => self.provider_color(provider),
            HerdStatus::Idle | HerdStatus::Done | HerdStatus::Unknown => rgb(self.dim),
        }
    }
}

fn rgb(color: (u8, u8, u8)) -> ColorAttribute {
    ColorAttribute::TrueColorWithDefaultFallback(termwiz::color::SrgbaTuple::from(color))
}

/// Everything the overlay needs at spawn time, all gathered on the GUI thread.
pub struct HerdArgs {
    pub theme: HerdTheme,
    pub view: HerdView,
    pub current_project: Option<PathBuf>,
    pub refresh: Duration,
    pub include_subagents: bool,
    pub read_claude_sessions: bool,
    /// Pane rows for the first frame, so the overview opens populated.
    pub initial_panes: Vec<PaneAgentRow>,
}

/// Entry point: run the overview until the user closes it.
pub fn agent_herd_overview(
    window: ::window::Window,
    mut term: TermWizTerminal,
    args: HerdArgs,
) -> anyhow::Result<()> {
    term.set_raw_mode()?;
    let mut state = HerdState::new(args);
    let (tx, rx) = channel();

    state.rebuild();
    state.render(&mut term)?;

    let mut next_refresh = Instant::now() + state.refresh;
    loop {
        // Wake on input or on the refresh tick, whichever comes first, so the
        // list stays live without spinning.
        let wait = next_refresh.saturating_duration_since(Instant::now());
        match term.poll_input(Some(wait.max(Duration::from_millis(16))))? {
            Some(event) => {
                if state.handle_event(event, &window) {
                    return Ok(());
                }
            }
            None => {}
        }

        if Instant::now() >= next_refresh {
            request_pane_rows(&window, tx.clone());
            next_refresh = Instant::now() + state.refresh;
        }
        if state.drain_pane_rows(&rx) {
            state.rebuild();
        }

        state.render(&mut term)?;
    }
}

/// Ask the GUI thread for a fresh pane snapshot.
///
/// Fire-and-forget: the reply lands on `tx` and is picked up by a later loop
/// iteration, so a busy GUI thread never stalls input handling here.
fn request_pane_rows(window: &::window::Window, tx: Sender<Vec<PaneAgentRow>>) {
    window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
        let rows = term_window.agent_herd_pane_rows();
        // Receiver gone means the overlay closed; nothing to do.
        let _ = tx.send(rows);
    })));
}

/// Which rendered line is what, so a mouse click can be resolved back to a row.
#[derive(Clone, Debug, PartialEq)]
enum Row {
    /// The `project ▸ all` view toggle.
    ViewToggle,
    GroupHeader,
    /// Index into `agents`.
    Agent(usize),
    /// Continuation line under an agent (block reason).
    Detail(usize),
    /// (agent index, subagent index)
    Subagent(usize, usize),
    Blank,
}

struct HerdState {
    theme: HerdTheme,
    view: HerdView,
    current_project: Option<PathBuf>,
    refresh: Duration,
    include_subagents: bool,
    read_claude_sessions: bool,

    pane_rows: Vec<PaneAgentRow>,
    /// Display order, flattened across groups.
    agents: Vec<HerdAgent>,
    groups: Vec<HerdGroup>,
    selected: usize,
    scroll: usize,
    footer: Option<String>,

    /// Previous status per agent, for deriving `Done`.
    prev_status: HashMap<String, HerdStatus>,
    /// Agents that finished while unattended and haven't been looked at.
    unseen_done: HashSet<String>,
    /// Row map for the last rendered frame, indexed by screen line.
    rows: Vec<Row>,
    /// Where the Stop control was drawn: (screen line, column range).
    stop_cell: Option<(usize, std::ops::Range<usize>)>,
}

impl HerdState {
    fn new(args: HerdArgs) -> Self {
        Self {
            theme: args.theme,
            view: args.view,
            current_project: args.current_project,
            refresh: args.refresh,
            include_subagents: args.include_subagents,
            read_claude_sessions: args.read_claude_sessions,
            pane_rows: args.initial_panes,
            agents: Vec::new(),
            groups: Vec::new(),
            selected: 0,
            scroll: 0,
            footer: None,
            prev_status: HashMap::new(),
            unseen_done: HashSet::new(),
            rows: Vec::new(),
            stop_cell: None,
        }
    }

    /// Stable-enough identity for tracking an agent across refreshes.
    fn key(agent: &HerdAgent) -> String {
        if let Some(session) = &agent.session_id {
            return format!("session:{session}");
        }
        if let Some(pane_id) = agent.pane_id {
            return format!("pane:{pane_id}");
        }
        format!("name:{}:{}", agent.provider, agent.name)
    }

    fn drain_pane_rows(&mut self, rx: &Receiver<Vec<PaneAgentRow>>) -> bool {
        let mut updated = false;
        // Keep only the newest snapshot if several queued up.
        while let Ok(rows) = rx.try_recv() {
            self.pane_rows = rows;
            updated = true;
        }
        updated
    }

    /// Re-read the filesystem sources, join with the latest pane snapshot, and
    /// regroup for display.
    fn rebuild(&mut self) {
        let sessions = if self.read_claude_sessions {
            match dirs_next::home_dir() {
                Some(home) => claude::collect_sessions(&home, self.include_subagents),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let mut agents = join_sessions_with_panes(sessions, self.pane_rows.clone());
        self.apply_done_derivation(&mut agents);

        self.groups = group_by_project(agents, self.view, self.current_project.as_deref());
        self.agents = self
            .groups
            .iter()
            .flat_map(|group| group.agents.iter().cloned())
            .collect();
        if self.selected >= self.agents.len() {
            self.selected = self.agents.len().saturating_sub(1);
        }
    }

    /// `Done` is not reported by any source: it means "finished while you
    /// weren't watching". An agent that drops from active to idle enters that
    /// state and stays there until the user actually looks at it.
    fn apply_done_derivation(&mut self, agents: &mut [HerdAgent]) {
        for agent in agents.iter_mut() {
            let key = Self::key(agent);
            let was_active = matches!(
                self.prev_status.get(&key),
                Some(HerdStatus::Working) | Some(HerdStatus::Blocked)
            );
            if was_active && agent.status == HerdStatus::Idle {
                self.unseen_done.insert(key.clone());
            }
            // Anything active again is no longer a finished result.
            if agent.status.is_interruptible() {
                self.unseen_done.remove(&key);
            }
            self.prev_status.insert(key.clone(), agent.status);
            if self.unseen_done.contains(&key) {
                agent.status = HerdStatus::Done;
            }
        }
    }

    fn acknowledge_selected(&mut self) {
        if let Some(agent) = self.agents.get(self.selected) {
            let key = Self::key(agent);
            self.unseen_done.remove(&key);
        }
    }

    /// Returns true when the overlay should close.
    fn handle_event(&mut self, event: InputEvent, window: &::window::Window) -> bool {
        match event {
            InputEvent::Key(KeyEvent { key, modifiers }) => self.handle_key(key, modifiers, window),
            InputEvent::Mouse(mouse) => {
                self.handle_mouse(mouse, window);
                false
            }
            _ => false,
        }
    }

    fn handle_key(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        window: &::window::Window,
    ) -> bool {
        match (key, modifiers) {
            (KeyCode::Escape, _)
            | (KeyCode::Char('q'), Modifiers::NONE)
            | (KeyCode::Char('c'), Modifiers::CTRL) => return true,

            (KeyCode::UpArrow, _) | (KeyCode::Char('k'), Modifiers::NONE) => {
                self.selected = self.selected.saturating_sub(1);
                self.acknowledge_selected();
            }
            (KeyCode::DownArrow, _) | (KeyCode::Char('j'), Modifiers::NONE) => {
                if self.selected + 1 < self.agents.len() {
                    self.selected += 1;
                }
                self.acknowledge_selected();
            }
            (KeyCode::Tab, _) => {
                self.view = self.view.toggled();
                self.selected = 0;
                self.scroll = 0;
                self.rebuild();
            }
            (KeyCode::Char('r'), Modifiers::NONE) => {
                self.rebuild();
                self.footer = Some("refreshed".to_string());
            }
            (KeyCode::Char('s'), Modifiers::NONE) => self.stop_selected(window),
            (KeyCode::Enter, _) => {
                if self.focus_selected(window) {
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, window: &::window::Window) {
        let y = mouse.y as usize;
        let x = mouse.x as usize;
        let left = mouse.mouse_buttons == MouseButtons::LEFT;

        // Stop control wins over the row it sits on.
        if let Some((stop_y, range)) = self.stop_cell.clone() {
            if y == stop_y && range.contains(&x) {
                if left {
                    self.stop_selected(window);
                }
                return;
            }
        }

        match self.rows.get(y).cloned() {
            Some(Row::Agent(idx)) | Some(Row::Detail(idx)) | Some(Row::Subagent(idx, _)) => {
                self.selected = idx;
                self.acknowledge_selected();
            }
            Some(Row::ViewToggle) if left => {
                self.view = self.view.toggled();
                self.selected = 0;
                self.scroll = 0;
                self.rebuild();
            }
            _ => {}
        }
    }

    fn stop_selected(&mut self, window: &::window::Window) {
        let Some(agent) = self.agents.get(self.selected) else {
            return;
        };
        if !agent.can_stop() {
            self.footer = Some(match agent.pane_id {
                None => "no pane owns this agent — cannot stop it from here".to_string(),
                Some(_) => format!("{} is not running", agent.name),
            });
            return;
        }
        let Some(pane_id) = agent.pane_id else {
            return;
        };
        let name = agent.name.clone();

        // Same path as the toolbelt's Stop: a synthetic Ctrl-C to the owning
        // pane. Never a signal to a pid — this terminal does not own processes
        // it merely discovered on disk.
        window.notify(TermWindowNotif::Apply(Box::new(move |_term_window| {
            if let Some(pane) = Mux::get().get_pane(pane_id) {
                if let Err(err) = pane.key_down(
                    ::termwiz::input::KeyCode::Char('c'),
                    ::termwiz::input::Modifiers::CTRL,
                ) {
                    log::warn!("failed to send agent interrupt: {err:#}");
                }
            }
        })));
        self.footer = Some(format!("sent Ctrl-C to {name}"));
    }

    /// Focus the selected agent's pane. Returns true when the overlay should
    /// close because focus moved away.
    fn focus_selected(&mut self, window: &::window::Window) -> bool {
        let Some(agent) = self.agents.get(self.selected) else {
            return false;
        };
        let Some(pane_id) = agent.pane_id else {
            self.footer = Some("no pane owns this agent".to_string());
            return false;
        };

        window.notify(TermWindowNotif::Apply(Box::new(move |_term_window| {
            let mux = Mux::get();
            let Some(pane) = mux.get_pane(pane_id) else {
                return;
            };
            let Some((_domain, window_id, tab_id)) = mux.resolve_pane_id(pane_id) else {
                return;
            };
            if let Some(tab) = mux.get_tab(tab_id) {
                tab.set_active_pane(&pane);
            }
            // Bind the write guard as a local rather than in an `if let`
            // condition: the guard borrows `mux`, and only a real local is
            // dropped before it in scope order.
            let Some(mut window) = mux.get_window_mut(window_id) else {
                return;
            };
            if let Some(idx) = window.idx_by_id(tab_id) {
                window.save_and_then_set_active(idx);
            }
        })));
        true
    }

    fn render(&mut self, term: &mut TermWizTerminal) -> anyhow::Result<()> {
        let size = term.get_screen_size()?;
        let cols = size.cols.max(20);
        let rows_avail = size.rows.max(6);

        let mut changes = vec![
            Change::ClearScreen(ColorAttribute::Default),
            Change::AllAttributes(CellAttributes::default()),
        ];
        self.rows = vec![Row::Blank; rows_avail];
        self.stop_cell = None;

        let mut y = 1usize;
        self.render_header(&mut changes, cols, y);
        self.rows[y] = Row::ViewToggle;
        y += 2;

        // Body height, leaving the footer its line.
        let body_end = rows_avail.saturating_sub(2);
        if self.agents.is_empty() {
            self.render_empty(&mut changes, y);
        } else {
            y = self.render_groups(&mut changes, cols, y, body_end);
        }
        let _ = y;

        self.render_footer(&mut changes, rows_avail.saturating_sub(1));
        term.render(&changes)?;
        term.flush()?;
        Ok(())
    }

    fn render_header(&self, changes: &mut Vec<Change>, cols: usize, y: usize) {
        let project = self
            .groups
            .first()
            .map(|group| group.label.clone())
            .unwrap_or_else(|| "—".to_string());
        let title = match self.view {
            HerdView::CurrentProject => format!("agents · {project}"),
            HerdView::AllGrouped => "agents · all projects".to_string(),
        };
        let toggle = match self.view {
            HerdView::CurrentProject => "project ▸ all",
            HerdView::AllGrouped => "all ▸ project",
        };

        move_to(changes, MARGIN, y);
        changes.push(Change::Attribute(AttributeChange::Intensity(
            Intensity::Bold,
        )));
        changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
            .theme
            .accent))));
        changes.push(Change::Text(title.clone()));
        changes.push(Change::Attribute(AttributeChange::Intensity(
            Intensity::Normal,
        )));

        let toggle_w = unicode_column_width(toggle, None);
        if cols > MARGIN + toggle_w + unicode_column_width(&title, None) + 3 {
            move_to(changes, cols - MARGIN - toggle_w, y);
            changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                .theme
                .dim))));
            changes.push(Change::Text(toggle.to_string()));
        }
    }

    fn render_empty(&self, changes: &mut Vec<Change>, y: usize) {
        move_to(changes, MARGIN, y + 1);
        changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
            .theme
            .dim))));
        let hint = match self.view {
            HerdView::CurrentProject => "no agents in this project · tab for all",
            HerdView::AllGrouped => "no agents running",
        };
        changes.push(Change::Text(hint.to_string()));
    }

    fn render_groups(
        &mut self,
        changes: &mut Vec<Change>,
        cols: usize,
        mut y: usize,
        body_end: usize,
    ) -> usize {
        // Keep the selection on screen.
        let selected_line = self.selected;
        if selected_line < self.scroll {
            self.scroll = selected_line;
        }

        let mut agent_idx = 0usize;
        let mut skipped = 0usize;
        let groups = std::mem::take(&mut self.groups);

        for group in &groups {
            if group.show_header && y < body_end {
                if skipped >= self.scroll {
                    move_to(changes, MARGIN, y);
                    changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                        .theme
                        .dim))));
                    changes.push(Change::Text(group.label.clone()));
                    self.rows[y] = Row::GroupHeader;
                    y += 1;
                }
            }
            for agent in &group.agents {
                if skipped < self.scroll {
                    skipped += 1;
                    agent_idx += 1;
                    continue;
                }
                if y >= body_end {
                    break;
                }
                y = self.render_agent(changes, cols, y, body_end, agent, agent_idx);
                agent_idx += 1;
            }
        }

        self.groups = groups;
        y
    }

    fn render_agent(
        &mut self,
        changes: &mut Vec<Change>,
        cols: usize,
        mut y: usize,
        body_end: usize,
        agent: &HerdAgent,
        agent_idx: usize,
    ) -> usize {
        let selected = agent_idx == self.selected;

        // Status dot.
        move_to(changes, MARGIN, y);
        if selected {
            changes.push(Change::Attribute(AttributeChange::Reverse(true)));
        }
        changes.push(Change::Attribute(AttributeChange::Foreground(
            self.theme.status_color(agent.status, &agent.provider),
        )));
        changes.push(Change::Text(format!("{} ", agent.status.glyph())));

        // Name.
        changes.push(Change::Attribute(AttributeChange::Foreground(
            ColorAttribute::Default,
        )));
        let meta = self.agent_meta(agent);
        let meta_w = unicode_column_width(&meta, None);
        let stop_w = if selected && agent.can_stop() {
            unicode_column_width(STOP_LABEL, None)
        } else {
            0
        };
        let name_budget = cols
            .saturating_sub(MARGIN * 2 + 2 + meta_w + stop_w + 2)
            .max(8);
        changes.push(Change::Text(truncate(&agent.name, name_budget)));

        // Right-aligned metadata, then the Stop control on the selected row.
        let meta_x = cols.saturating_sub(MARGIN + meta_w + stop_w);
        if meta_x > MARGIN + 2 {
            move_to(changes, meta_x, y);
            changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                .theme
                .dim))));
            changes.push(Change::Text(meta));
        }
        if stop_w > 0 {
            let stop_x = cols.saturating_sub(MARGIN + stop_w);
            move_to(changes, stop_x, y);
            changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                .theme
                .attention))));
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Bold,
            )));
            changes.push(Change::Text(STOP_LABEL.to_string()));
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Normal,
            )));
            self.stop_cell = Some((y, stop_x..stop_x + stop_w));
        }
        changes.push(Change::Attribute(AttributeChange::Reverse(false)));
        self.rows[y] = Row::Agent(agent_idx);
        y += 1;

        // Block reason, with how long it has been waiting.
        if let Some(reason) = &agent.blocked_reason {
            if y < body_end {
                move_to(changes, SUB_INDENT, y);
                changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                    .theme
                    .attention))));
                changes.push(Change::Text(truncate(
                    reason,
                    cols.saturating_sub(SUB_INDENT + MARGIN + 10),
                )));
                if let Some(elapsed) = elapsed_label(agent.status_changed_at) {
                    let w = unicode_column_width(&elapsed, None);
                    let x = cols.saturating_sub(MARGIN + w);
                    if x > SUB_INDENT {
                        move_to(changes, x, y);
                        changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                            .theme
                            .dim))));
                        changes.push(Change::Text(elapsed));
                    }
                }
                self.rows[y] = Row::Detail(agent_idx);
                y += 1;
            }
        }

        // Subagents.
        let last = agent.subagents.len().saturating_sub(1);
        for (sub_idx, sub) in agent.subagents.iter().enumerate() {
            if y >= body_end {
                break;
            }
            let branch = if sub_idx == last { "└" } else { "├" };
            move_to(changes, SUB_INDENT, y);
            changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                .theme
                .dim))));
            changes.push(Change::Text(format!("{branch} ")));
            changes.push(Change::Attribute(AttributeChange::Foreground(
                ColorAttribute::Default,
            )));

            let status_w = unicode_column_width(sub.status.label(), None);
            let type_label = format!("{:<12} ", truncate(&sub.agent_type, 12));
            changes.push(Change::Text(type_label.clone()));
            let used = SUB_INDENT + 2 + unicode_column_width(&type_label, None);
            let desc_budget = cols.saturating_sub(used + MARGIN + status_w + 2).max(6);
            changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                .theme
                .dim))));
            changes.push(Change::Text(truncate(&sub.description, desc_budget)));

            let status_x = cols.saturating_sub(MARGIN + status_w);
            if status_x > used {
                move_to(changes, status_x, y);
                changes.push(Change::Attribute(AttributeChange::Foreground(
                    self.theme.status_color(sub.status, &agent.provider),
                )));
                changes.push(Change::Text(sub.status.label().to_string()));
            }
            self.rows[y] = Row::Subagent(agent_idx, sub_idx);
            y += 1;
        }

        y
    }

    /// `working · claude · opus 5`, trimmed to what is actually known.
    fn agent_meta(&self, agent: &HerdAgent) -> String {
        let mut parts = vec![agent.status.label().to_string()];
        if !agent.provider.is_empty() {
            parts.push(agent.provider.clone());
        }
        if let Some(model) = &agent.model {
            parts.push(model.clone());
        }
        if agent.pane_id.is_none() {
            parts.push("no pane".to_string());
        }
        parts.join(" · ")
    }

    fn render_footer(&self, changes: &mut Vec<Change>, y: usize) {
        move_to(changes, MARGIN, y);
        changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
            .theme
            .dim))));
        let keys = "↑↓ select   s stop   ⏎ focus   tab view   r refresh   q close";
        match &self.footer {
            Some(message) => {
                changes.push(Change::Attribute(AttributeChange::Foreground(rgb(self
                    .theme
                    .accent))));
                changes.push(Change::Text(message.clone()));
            }
            None => changes.push(Change::Text(keys.to_string())),
        }
    }
}

fn move_to(changes: &mut Vec<Change>, x: usize, y: usize) {
    changes.push(Change::CursorPosition {
        x: Position::Absolute(x),
        y: Position::Absolute(y),
    });
}

/// Truncate to a column budget, with an ellipsis when something was cut.
fn truncate(text: &str, budget: usize) -> String {
    if budget == 0 {
        return String::new();
    }
    if unicode_column_width(text, None) <= budget {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_column_width(&ch.to_string(), None);
        if width + w + 1 > budget {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('…');
    out
}

/// `2m14s` / `45s` / `3h07m`, or nothing if the timestamp is unusable.
fn elapsed_label(since: Option<SystemTime>) -> Option<String> {
    let since = since?;
    let elapsed = SystemTime::now().duration_since(since).ok()?;
    let secs = elapsed.as_secs();
    Some(if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mux::pane::PaneId;

    #[test]
    fn truncate_adds_an_ellipsis_only_when_it_cuts() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("", 10), "");
        assert_eq!(truncate("anything", 0), "");
        let cut = truncate("a-very-long-agent-name", 8);
        assert!(cut.ends_with('…'));
        assert!(unicode_column_width(&cut, None) <= 8);
    }

    #[test]
    fn elapsed_label_scales_by_magnitude() {
        let now = SystemTime::now();
        assert_eq!(
            elapsed_label(Some(now - Duration::from_secs(45))).as_deref(),
            Some("45s")
        );
        assert_eq!(
            elapsed_label(Some(now - Duration::from_secs(134))).as_deref(),
            Some("2m14s")
        );
        assert_eq!(
            elapsed_label(Some(now - Duration::from_secs(11_220))).as_deref(),
            Some("3h07m")
        );
        assert_eq!(elapsed_label(None), None);
        // A timestamp from the future must not panic.
        assert_eq!(elapsed_label(Some(now + Duration::from_secs(60))), None);
    }

    fn theme() -> HerdTheme {
        HerdTheme {
            adapter_colors: HashMap::new(),
            attention: (240, 180, 60),
            dim: (120, 120, 120),
            accent: (200, 200, 255),
        }
    }

    fn state() -> HerdState {
        HerdState::new(HerdArgs {
            theme: theme(),
            view: HerdView::CurrentProject,
            current_project: None,
            refresh: Duration::from_millis(500),
            include_subagents: true,
            read_claude_sessions: false,
            initial_panes: Vec::new(),
        })
    }

    fn agent(name: &str, status: HerdStatus, pane_id: Option<PaneId>) -> HerdAgent {
        HerdAgent {
            name: name.to_string(),
            provider: "claude".to_string(),
            status,
            blocked_reason: None,
            model: None,
            cwd: None,
            project_root: None,
            git_branch: None,
            pid: None,
            pane_id,
            session_id: Some(format!("s-{name}")),
            started_at: None,
            status_changed_at: None,
            subagents: Vec::new(),
        }
    }

    #[test]
    fn an_agent_that_finishes_unattended_reads_done_until_looked_at() {
        let mut state = state();
        let mut agents = vec![agent("alpha", HerdStatus::Working, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Working);

        // It goes quiet while the user is elsewhere.
        let mut agents = vec![agent("alpha", HerdStatus::Idle, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Done);

        // Still Done on the next poll — nobody has looked yet.
        let mut agents = vec![agent("alpha", HerdStatus::Idle, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Done);

        // Selecting it acknowledges the result.
        state.agents = agents.clone();
        state.selected = 0;
        state.acknowledge_selected();
        let mut agents = vec![agent("alpha", HerdStatus::Idle, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Idle);
    }

    #[test]
    fn an_agent_that_starts_working_again_is_no_longer_done() {
        let mut state = state();
        state.apply_done_derivation(&mut [agent("alpha", HerdStatus::Working, Some(1))]);
        state.apply_done_derivation(&mut [agent("alpha", HerdStatus::Idle, Some(1))]);
        assert!(state.unseen_done.contains("session:s-alpha"));

        let mut agents = vec![agent("alpha", HerdStatus::Working, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Working);
        assert!(!state.unseen_done.contains("session:s-alpha"));
    }

    #[test]
    fn an_agent_idle_from_the_start_is_not_done() {
        let mut state = state();
        let mut agents = vec![agent("alpha", HerdStatus::Idle, Some(1))];
        state.apply_done_derivation(&mut agents);
        assert_eq!(agents[0].status, HerdStatus::Idle);
    }

    #[test]
    fn identity_prefers_session_then_pane_then_name() {
        let mut a = agent("alpha", HerdStatus::Idle, Some(7));
        assert_eq!(HerdState::key(&a), "session:s-alpha");
        a.session_id = None;
        assert_eq!(HerdState::key(&a), "pane:7");
        a.pane_id = None;
        assert_eq!(HerdState::key(&a), "name:claude:alpha");
    }

    #[test]
    fn selection_is_clamped_when_the_list_shrinks() {
        let mut state = state();
        state.pane_rows = Vec::new();
        state.selected = 5;
        state.rebuild();
        assert_eq!(state.selected, 0);
    }
}
