//! The agent herd view: a list of every agent this terminal can see, what each
//! one is doing, its subagents, and a Stop control.
//!
//! Drawn into a `TermWizTerminal`, which gets all of the pane's input and —
//! importantly — runs on its own thread, so filesystem polling for Claude
//! sessions and transcripts never touches the render path. It lives in a real
//! split pane; see [`crate::termwindow::agent_insight`] for how that pane is
//! made and torn down.
//!
//! The one hard constraint: **this thread must not touch the mux.** Pane data is
//! requested from the GUI thread via [`TermWindowNotif::Apply`] and arrives over
//! a channel; see [`request_pane_rows`].

use crate::agent_herd::{
    claude, group_by_project, join_sessions_with_panes, transcript, HerdActivity, HerdAgent,
    HerdEventKind, HerdGroup, HerdStatus, HerdView, PaneAgentRow,
};
use crate::termwindow::TermWindowNotif;
use mux::termwiztermtab::TermWizTerminal;
use mux::Mux;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
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
    /// Read each agent's transcript for what it is currently doing.
    pub show_activity: bool,
    /// How many recent events the per-agent log keeps.
    pub activity_history: usize,
}

/// Entry point: run the overview until the user closes it.
pub fn agent_herd_overview(
    window: ::window::Window,
    mut term: TermWizTerminal,
    args: HerdArgs,
) -> anyhow::Result<()> {
    term.set_raw_mode()?;
    // Names the pane in the sidebar and the tab title. Cosmetic only — nothing
    // identifies this pane by its title.
    term.render(&[Change::Title(
        crate::termwindow::agent_insight::INSIGHT_PANE_TITLE.to_string(),
    )])?;
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
    /// The `now:` activity headline under an agent.
    Activity(usize),
    /// (agent index, event index) in the expanded activity log.
    Event(usize, usize),
    /// (agent index, subagent index)
    Subagent(usize, usize),
    Blank,
}

/// A run of text at a fixed column, with the attributes it is drawn in.
#[derive(Clone, Debug, PartialEq)]
struct Segment {
    x: usize,
    text: String,
    fg: ColorAttribute,
    bold: bool,
    reverse: bool,
}

impl Segment {
    fn dim(x: usize, text: impl Into<String>, dim: (u8, u8, u8)) -> Self {
        Self {
            x,
            text: text.into(),
            fg: rgb(dim),
            bold: false,
            reverse: false,
        }
    }
}

/// One laid-out body line, before it is clipped to the visible window.
#[derive(Clone, Debug, PartialEq)]
struct BodyLine {
    row: Row,
    segments: Vec<Segment>,
    /// Where the Stop control sits on this line, if it has one.
    stop: Option<Range<usize>>,
}

/// Scroll offset that keeps the selected agent's row on screen.
///
/// Clamps in both directions, which is the whole point: the offset must grow
/// when the selection moves below the fold, not only shrink when it moves above
/// it.
fn clamp_scroll(lines: &[BodyLine], selected: usize, scroll: usize, height: usize) -> usize {
    if height == 0 || lines.is_empty() {
        return 0;
    }
    let max_scroll = lines.len().saturating_sub(height);
    let mut scroll = scroll.min(max_scroll);
    if let Some(idx) = lines
        .iter()
        .position(|line| line.row == Row::Agent(selected))
    {
        if idx < scroll {
            scroll = idx;
        } else if idx >= scroll + height {
            scroll = idx + 1 - height;
        }
    }
    scroll
}

struct HerdState {
    theme: HerdTheme,
    view: HerdView,
    current_project: Option<PathBuf>,
    refresh: Duration,
    include_subagents: bool,
    read_claude_sessions: bool,
    show_activity: bool,
    activity_history: usize,

    pane_rows: Vec<PaneAgentRow>,
    /// Display order, flattened across groups.
    agents: Vec<HerdAgent>,
    groups: Vec<HerdGroup>,
    selected: usize,
    scroll: usize,
    footer: Option<String>,

    /// Agents whose activity log is expanded, keyed by [`HerdState::key`].
    expanded: HashSet<String>,
    /// Transcript activity per session, with the `(mtime, len)` it was read at
    /// so an unchanged file is never re-read.
    activity_cache: HashMap<String, (Option<(Option<SystemTime>, u64)>, HerdActivity)>,

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
            show_activity: args.show_activity,
            activity_history: args.activity_history,
            pane_rows: args.initial_panes,
            agents: Vec::new(),
            groups: Vec::new(),
            selected: 0,
            scroll: 0,
            footer: None,
            expanded: HashSet::new(),
            activity_cache: HashMap::new(),
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
        self.fill_activity(&mut agents);
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

    /// Attach "what is it doing" to every agent whose transcript we can find.
    ///
    /// The cache is what makes this affordable at the refresh rate: a transcript
    /// is only re-read when its `(mtime, len)` moved, so a quiet agent costs one
    /// `stat` per tick and a busy one costs a bounded tail read.
    fn fill_activity(&mut self, agents: &mut [HerdAgent]) {
        if !self.show_activity || self.activity_history == 0 {
            return;
        }
        let Some(home) = dirs_next::home_dir() else {
            return;
        };

        let mut live = HashSet::new();
        for agent in agents.iter_mut() {
            // Only Claude publishes a transcript we know how to read; other
            // vendors simply have no activity line, which is the honest result.
            let (Some(session_id), Some(cwd)) = (agent.session_id.as_deref(), agent.cwd.as_deref())
            else {
                continue;
            };
            if agent.provider != "claude" {
                continue;
            }
            let Some(path) = claude::session_transcript_path(&home, cwd, session_id) else {
                continue;
            };
            let stamp = std::fs::metadata(&path)
                .ok()
                .map(|meta| (meta.modified().ok(), meta.len()));

            live.insert(session_id.to_string());
            let cached = self.activity_cache.get(session_id);
            let activity = match cached {
                Some((cached_stamp, activity)) if *cached_stamp == stamp && stamp.is_some() => {
                    activity.clone()
                }
                _ => {
                    let activity = transcript::read_activity(&path, self.activity_history);
                    self.activity_cache
                        .insert(session_id.to_string(), (stamp, activity.clone()));
                    activity
                }
            };
            if !activity.is_empty() {
                agent.activity = Some(activity);
            }
        }

        // Sessions that ended must not keep their entry alive forever.
        self.activity_cache
            .retain(|session_id, _| live.contains(session_id));
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
            // The pane persists, so Enter is free for the thing you do most
            // often here — look closer — and focusing moves to `f`.
            (KeyCode::Enter, _)
            | (KeyCode::RightArrow, _)
            | (KeyCode::LeftArrow, _)
            | (KeyCode::Char('l'), Modifiers::NONE)
            | (KeyCode::Char('h'), Modifiers::NONE) => self.toggle_expand_selected(),
            (KeyCode::Char('f'), Modifiers::NONE) => {
                self.focus_selected(window);
            }
            _ => {}
        }
        false
    }

    /// Expand or collapse the selected agent's activity log.
    fn toggle_expand_selected(&mut self) {
        let Some(agent) = self.agents.get(self.selected) else {
            return;
        };
        let key = Self::key(agent);
        let has_log = agent
            .activity
            .as_ref()
            .is_some_and(|activity| !activity.recent.is_empty());
        if !self.expanded.remove(&key) {
            if !has_log {
                self.footer = Some(format!("no recorded activity for {}", agent.name));
                return;
            }
            self.expanded.insert(key);
        }
        self.acknowledge_selected();
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
            Some(Row::Agent(idx))
            | Some(Row::Detail(idx))
            | Some(Row::Event(idx, _))
            | Some(Row::Subagent(idx, _)) => {
                self.selected = idx;
                self.acknowledge_selected();
            }
            // The activity line is the expander, so clicking it does what
            // clicking a disclosure triangle does.
            Some(Row::Activity(idx)) => {
                self.selected = idx;
                self.acknowledge_selected();
                if left {
                    self.toggle_expand_selected();
                }
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

    /// Focus the selected agent's pane.
    ///
    /// This view lives in a pane of its own, so focusing an agent moves the
    /// cursor without tearing the list down — that is the point of it being a
    /// pane rather than an overlay.
    fn focus_selected(&mut self, window: &::window::Window) {
        let Some(agent) = self.agents.get(self.selected) else {
            return;
        };
        let Some(pane_id) = agent.pane_id else {
            self.footer = Some("no pane owns this agent".to_string());
            return;
        };
        let name = agent.name.clone();

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
        self.footer = Some(format!("focused {name}"));
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
            self.paint_body(&mut changes, cols, y, body_end);
        }

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

    /// Lay the whole body out as data, without clipping it.
    ///
    /// Layout is separated from painting so that scrolling has a real line
    /// count to work against — the previous single-pass renderer could only
    /// ever scroll *up*, because it never knew how much was below the fold —
    /// and so the layout can be tested without a terminal.
    fn body_lines(&self, cols: usize) -> Vec<BodyLine> {
        let mut lines = Vec::new();
        let mut agent_idx = 0usize;

        for group in &self.groups {
            if group.show_header {
                lines.push(BodyLine {
                    row: Row::GroupHeader,
                    segments: vec![Segment::dim(MARGIN, group.label.clone(), self.theme.dim)],
                    stop: None,
                });
            }
            for agent in &group.agents {
                self.agent_lines(&mut lines, cols, agent, agent_idx);
                agent_idx += 1;
            }
        }
        lines
    }

    fn agent_lines(
        &self,
        lines: &mut Vec<BodyLine>,
        cols: usize,
        agent: &HerdAgent,
        agent_idx: usize,
    ) {
        let selected = agent_idx == self.selected;
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

        let mut segments = vec![
            Segment {
                x: MARGIN,
                text: format!("{} ", agent.status.glyph()),
                fg: self.theme.status_color(agent.status, &agent.provider),
                bold: false,
                reverse: selected,
            },
            Segment {
                x: MARGIN + 2,
                text: truncate(&agent.name, name_budget),
                fg: ColorAttribute::Default,
                bold: false,
                reverse: selected,
            },
        ];

        let meta_x = cols.saturating_sub(MARGIN + meta_w + stop_w);
        if meta_x > MARGIN + 2 {
            segments.push(Segment {
                x: meta_x,
                text: meta,
                fg: rgb(self.theme.dim),
                bold: false,
                reverse: selected,
            });
        }
        let mut stop = None;
        if stop_w > 0 {
            let stop_x = cols.saturating_sub(MARGIN + stop_w);
            segments.push(Segment {
                x: stop_x,
                text: STOP_LABEL.to_string(),
                fg: rgb(self.theme.attention),
                bold: true,
                reverse: selected,
            });
            stop = Some(stop_x..stop_x + stop_w);
        }
        lines.push(BodyLine {
            row: Row::Agent(agent_idx),
            segments,
            stop,
        });

        // Block reason, with how long it has been waiting.
        if let Some(reason) = &agent.blocked_reason {
            let mut segments = vec![Segment {
                x: SUB_INDENT,
                text: truncate(reason, cols.saturating_sub(SUB_INDENT + MARGIN + 10)),
                fg: rgb(self.theme.attention),
                bold: false,
                reverse: false,
            }];
            if let Some(elapsed) = elapsed_label(agent.status_changed_at) {
                let w = unicode_column_width(&elapsed, None);
                let x = cols.saturating_sub(MARGIN + w);
                if x > SUB_INDENT {
                    segments.push(Segment::dim(x, elapsed, self.theme.dim));
                }
            }
            lines.push(BodyLine {
                row: Row::Detail(agent_idx),
                segments,
                stop: None,
            });
        }

        self.activity_lines(lines, cols, agent, agent_idx);

        // Subagents.
        let last = agent.subagents.len().saturating_sub(1);
        for (sub_idx, sub) in agent.subagents.iter().enumerate() {
            let branch = if sub_idx == last { "└" } else { "├" };
            let status_w = unicode_column_width(sub.status.label(), None);
            let type_label = format!("{:<12} ", truncate(&sub.agent_type, 12));
            let type_w = unicode_column_width(&type_label, None);
            let used = SUB_INDENT + 2 + type_w;
            let desc_budget = cols.saturating_sub(used + MARGIN + status_w + 2).max(6);

            let mut segments = vec![
                Segment::dim(SUB_INDENT, format!("{branch} "), self.theme.dim),
                Segment {
                    x: SUB_INDENT + 2,
                    text: type_label,
                    fg: ColorAttribute::Default,
                    bold: false,
                    reverse: false,
                },
                Segment::dim(
                    used,
                    truncate(&sub.description, desc_budget),
                    self.theme.dim,
                ),
            ];
            let status_x = cols.saturating_sub(MARGIN + status_w);
            if status_x > used {
                segments.push(Segment {
                    x: status_x,
                    text: sub.status.label().to_string(),
                    fg: self.theme.status_color(sub.status, &agent.provider),
                    bold: false,
                    reverse: false,
                });
            }
            lines.push(BodyLine {
                row: Row::Subagent(agent_idx, sub_idx),
                segments,
                stop: None,
            });
        }
    }

    /// The `now:` headline and, when expanded, the recent-activity log.
    fn activity_lines(
        &self,
        lines: &mut Vec<BodyLine>,
        cols: usize,
        agent: &HerdAgent,
        agent_idx: usize,
    ) {
        if !self.show_activity {
            return;
        }
        let Some(activity) = &agent.activity else {
            return;
        };
        let expanded = self.expanded.contains(&Self::key(agent));
        let now = SystemTime::now();

        if let Some((label, text)) = activity.headline(agent.status, now) {
            // The marker only claims to expand something when there is
            // something behind it.
            let marker = if activity.recent.is_empty() {
                "↳"
            } else if expanded {
                "▾"
            } else {
                "▸"
            };
            let prefix = format!("{marker} {label}: ");
            let prefix_w = unicode_column_width(&prefix, None);
            let budget = cols.saturating_sub(SUB_INDENT + prefix_w + MARGIN).max(8);
            lines.push(BodyLine {
                row: Row::Activity(agent_idx),
                segments: vec![
                    Segment::dim(SUB_INDENT, prefix, self.theme.dim),
                    Segment {
                        x: SUB_INDENT + prefix_w,
                        text: truncate(text, budget),
                        fg: ColorAttribute::Default,
                        bold: false,
                        reverse: false,
                    },
                ],
                stop: None,
            });
        }

        if !expanded {
            return;
        }
        for (event_idx, event) in activity.recent.iter().enumerate() {
            let age = elapsed_label(event.at).unwrap_or_else(|| "—".to_string());
            let age = format!("{age:>6}  ");
            let age_w = unicode_column_width(&age, None);
            let x = SUB_INDENT + 2;
            let budget = cols.saturating_sub(x + age_w + MARGIN).max(8);
            lines.push(BodyLine {
                row: Row::Event(agent_idx, event_idx),
                segments: vec![
                    Segment::dim(x, age, self.theme.dim),
                    Segment {
                        x: x + age_w,
                        text: truncate(&event.text, budget),
                        fg: if event.kind == HerdEventKind::Tool {
                            ColorAttribute::Default
                        } else {
                            rgb(self.theme.dim)
                        },
                        bold: false,
                        reverse: false,
                    },
                ],
                stop: None,
            });
        }
    }

    /// Paint the visible slice of the body, scrolling so the selected agent
    /// stays on screen in **both** directions.
    fn paint_body(&mut self, changes: &mut Vec<Change>, cols: usize, y: usize, body_end: usize) {
        let lines = self.body_lines(cols);
        let height = body_end.saturating_sub(y);
        self.scroll = clamp_scroll(&lines, self.selected, self.scroll, height);

        for (offset, line) in lines.iter().skip(self.scroll).take(height).enumerate() {
            let screen_y = y + offset;
            for segment in &line.segments {
                move_to(changes, segment.x, screen_y);
                changes.push(Change::Attribute(AttributeChange::Reverse(segment.reverse)));
                changes.push(Change::Attribute(AttributeChange::Foreground(segment.fg)));
                changes.push(Change::Attribute(AttributeChange::Intensity(
                    if segment.bold {
                        Intensity::Bold
                    } else {
                        Intensity::Normal
                    },
                )));
                changes.push(Change::Text(segment.text.clone()));
            }
            changes.push(Change::Attribute(AttributeChange::Reverse(false)));
            changes.push(Change::Attribute(AttributeChange::Intensity(
                Intensity::Normal,
            )));
            if let Some(range) = &line.stop {
                self.stop_cell = Some((screen_y, range.clone()));
            }
            self.rows[screen_y] = line.row.clone();
        }
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
        let keys = "↑↓ select   ⏎ details   f focus   s stop   tab view   r refresh   q close";
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
    use crate::agent_herd::HerdEvent;
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
            show_activity: true,
            activity_history: 30,
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
            activity: None,
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

    fn activity(current: Option<&str>, recent: &[&str]) -> HerdActivity {
        let now = SystemTime::now();
        let event = |text: &str| HerdEvent {
            at: Some(now),
            kind: HerdEventKind::Tool,
            text: text.to_string(),
        };
        HerdActivity {
            current: current.map(event),
            recent: recent.iter().map(|text| event(text)).collect(),
        }
    }

    /// Load `state` with `agents` as a single group, the way `rebuild` would.
    fn seed(state: &mut HerdState, agents: Vec<HerdAgent>) {
        state.agents = agents.clone();
        state.groups = vec![HerdGroup {
            label: "repo".to_string(),
            show_header: false,
            agents,
        }];
    }

    #[test]
    fn the_activity_headline_is_a_line_of_its_own_under_the_agent() {
        let mut state = state();
        let mut working = agent("alpha", HerdStatus::Working, Some(1));
        working.activity = Some(activity(Some("Bash cargo check"), &["Read config.rs"]));
        seed(&mut state, vec![working]);

        let rows: Vec<Row> = state
            .body_lines(80)
            .into_iter()
            .map(|line| line.row)
            .collect();
        assert_eq!(rows, vec![Row::Agent(0), Row::Activity(0)]);

        // Turning the feature off removes the line entirely.
        state.show_activity = false;
        assert_eq!(state.body_lines(80).len(), 1);
    }

    #[test]
    fn an_agent_with_no_activity_gets_no_activity_line() {
        let mut state = state();
        seed(
            &mut state,
            vec![agent("alpha", HerdStatus::Working, Some(1))],
        );
        assert_eq!(state.body_lines(80).len(), 1);
    }

    #[test]
    fn expanding_an_agent_adds_exactly_its_event_lines() {
        let mut state = state();
        let mut working = agent("alpha", HerdStatus::Working, Some(1));
        working.activity = Some(activity(
            Some("Bash cargo check"),
            &["Read config.rs", "Edit sidebar.rs", "Bash cargo check"],
        ));
        seed(&mut state, vec![working]);
        assert_eq!(state.body_lines(80).len(), 2);

        state.selected = 0;
        state.toggle_expand_selected();
        let rows: Vec<Row> = state
            .body_lines(80)
            .into_iter()
            .map(|line| line.row)
            .collect();
        assert_eq!(
            rows,
            vec![
                Row::Agent(0),
                Row::Activity(0),
                Row::Event(0, 0),
                Row::Event(0, 1),
                Row::Event(0, 2),
            ]
        );

        // And collapses back.
        state.toggle_expand_selected();
        assert_eq!(state.body_lines(80).len(), 2);
    }

    #[test]
    fn expanding_an_agent_with_no_log_says_so_instead_of_expanding_nothing() {
        let mut state = state();
        seed(
            &mut state,
            vec![agent("alpha", HerdStatus::Working, Some(1))],
        );
        state.selected = 0;
        state.toggle_expand_selected();

        assert!(state.expanded.is_empty());
        assert_eq!(
            state.footer.as_deref(),
            Some("no recorded activity for alpha")
        );
    }

    #[test]
    fn scrolling_follows_the_selection_below_the_fold() {
        let agents: Vec<HerdAgent> = (0..10)
            .map(|idx| agent(&format!("a{idx}"), HerdStatus::Idle, Some(idx as PaneId)))
            .collect();
        let mut state = state();
        seed(&mut state, agents);

        // One line per agent here, and room for three of them.
        state.selected = 7;
        let lines = state.body_lines(80);
        // The bug this replaced: scroll only ever shrank, so a selection below
        // the fold stayed off screen forever.
        assert_eq!(clamp_scroll(&lines, 7, 0, 3), 5);
        // Moving back up pulls it the other way.
        assert_eq!(clamp_scroll(&lines, 1, 5, 3), 1);
        // Already visible: left alone.
        assert_eq!(clamp_scroll(&lines, 6, 5, 3), 5);
        // Never past the end of the list.
        assert_eq!(clamp_scroll(&lines, 9, 99, 3), 7);
    }

    #[test]
    fn scrolling_accounts_for_lines_the_agents_bring_with_them() {
        let mut state = state();
        let mut agents = Vec::new();
        for idx in 0..4 {
            let mut a = agent(&format!("a{idx}"), HerdStatus::Working, Some(idx as PaneId));
            a.activity = Some(activity(Some("Bash cargo check"), &[]));
            agents.push(a);
        }
        seed(&mut state, agents);

        let lines = state.body_lines(80);
        // Two lines per agent, so the third agent starts on line 4.
        assert_eq!(lines.len(), 8);
        assert_eq!(clamp_scroll(&lines, 3, 0, 4), 3);
    }
}
