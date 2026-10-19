//! View model and reducers. Pure: inputs and server messages in, effects out.

use super::clip::Rect;
use super::forms::{Field, FieldId, Place, Sheet, AUTO_ROLE};
use super::proto::{ClientMsg, HandoffPacket, ServerMsg, Snapshot, SnapshotOrigin, Stage};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Strips,
    Roster,
    Inbox,
    Frequencies,
    Models,
    Intake,
}

impl View {
    pub const ALL: [View; 6] = [
        View::Strips,
        View::Roster,
        View::Inbox,
        View::Frequencies,
        View::Models,
        View::Intake,
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::Strips => "Strips",
            View::Roster => "Roster",
            View::Inbox => "Inbox",
            View::Frequencies => "Frequencies",
            View::Models => "Models",
            View::Intake => "Intake",
        }
    }

    fn index(self) -> usize {
        View::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// The side panel over the main area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drawer {
    Flight(String),
    Agent(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScrollId {
    BoardX,
    Column(Stage),
    Drawer,
    Roster,
    Inbox,
    Frequencies,
    Models,
    Intake,
    Sheet,
    Field(FieldId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    NewFlight,
    OpenLaunch {
        role_id: String,
    },
    LaunchFlight {
        flight_id: String,
        role_id: String,
    },
    OpenPacket {
        flight_id: String,
    },
    Answer {
        item_id: String,
    },
    Resolve {
        item_id: String,
        approve: bool,
        option_id: Option<String>,
    },
    Release {
        flight_id: String,
        handoff_id: String,
    },
    Escalate {
        flight_id: String,
    },
    DeleteFlight {
        flight_id: String,
    },
    StopAgent {
        agent_id: String,
    },
    FocusPane {
        pane_id: u64,
    },
    OpenAgent {
        agent_id: String,
    },
    OpenFlight {
        flight_id: String,
    },
    /// Open a file in `$EDITOR`, in a split next to Tower.
    EditFile {
        path: String,
    },
    /// A plain terminal split next to Tower.
    OpenTerminal {
        cwd: Option<String>,
    },
    Probe,
    PollSources,
    Refresh,
    SheetRole(Option<String>),
    /// New flight / Launch: where the work happens.
    SheetPlace(Place),
    /// New flight: how hard it is (`None` = the router decides).
    SheetTier(Option<String>),
    /// Open a deliverable with the system's default app.
    OpenPath {
        path: String,
    },
    Submit {
        launch: bool,
    },
    CloseSheet,
    SendCompose,
    /// Frequencies: the team (`team`) or a flight's thread (its id).
    SelectFrequency(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Nav(View),
    Card(String),
    /// The dimmed area around a drawer: closes it.
    Scrim,
    CloseDrawer,
    /// The dimmed area around a sheet: swallows clicks.
    SheetScrim,
    Button(Action),
    /// An inbox row: opens its flight.
    InboxRow(String),
    Field(FieldId),
    /// Swallows clicks on a surface without doing anything.
    Surface,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Conn {
    Connecting,
    Live { pid: u32 },
    Offline(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollRegion {
    pub id: ScrollId,
    pub viewport: Rect,
    /// Content extent along the scroll axis.
    pub content: f32,
    pub horizontal: bool,
    /// The stored offset counts from the end (chat-style, sticks to the bottom).
    pub from_bottom: bool,
}

impl ScrollRegion {
    pub fn max_offset(&self) -> f32 {
        let view = if self.horizontal {
            self.viewport.w
        } else {
            self.viewport.h
        };
        (self.content - view).max(0.0)
    }
}

/// What the last painted frame looked like, for mapping input back to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub hits: Vec<(Rect, Hit)>,
    pub scrolls: Vec<ScrollRegion>,
    /// Window-pixel origin of the pane's content area.
    pub origin: (f32, f32),
    pub cell: (f32, f32),
    /// Pixels per wheel notch.
    pub wheel_step: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Escape,
    Tab,
    BackTab,
    Enter { ctrl: bool },
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Char(char),
}

/// Input in window pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Move {
        x: f32,
        y: f32,
    },
    Down {
        x: f32,
        y: f32,
    },
    Up {
        x: f32,
        y: f32,
    },
    /// Notches; positive `dy` scrolls content up (towards the top).
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
    },
    Key(Key),
    /// Composed (IME) text.
    Text(String),
    Paste(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Send(ClientMsg),
    Split {
        argv: Option<Vec<String>>,
        cwd: Option<String>,
    },
    FocusPane(u64),
    /// Open a file with the system's default app.
    Open(String),
    Repaint,
}

pub const TOAST_MS: u64 = 3600;

/// Open `path` in the user's editor: `$EDITOR` through `sh` on Unix (so
/// values like "code -w" work), `$EDITOR` or Notepad on Windows.
pub fn edit_argv(path: &str) -> Vec<String> {
    if cfg!(windows) {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad.exe".into());
        vec![editor, path.to_string()]
    } else {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "exec ${EDITOR:-vi} \"$1\"".into(),
            "tower-edit".into(),
            path.to_string(),
        ]
    }
}

pub fn empty_snapshot() -> Snapshot {
    Snapshot {
        origin: SnapshotOrigin::Live,
        now_unix: 0,
        roles: vec![],
        agents: vec![],
        flights: vec![],
        inbox: vec![],
        endpoints: vec![],
        frequencies: vec![],
        config_dir: String::new(),
        sources: vec![],
        flights_root: String::new(),
        triage_role: String::new(),
        engines: vec![],
        bandit_mode: String::new(),
    }
}

pub struct Model {
    pub snap: Snapshot,
    pub conn: Conn,
    pub view: View,
    pub drawer: Option<Drawer>,
    pub sheet: Option<Sheet>,
    pub focus: Option<FieldId>,
    /// The message field of the agent drawer and the frequencies view.
    pub compose: Field,
    pub freq: String,
    /// Where new flights work by default: the folder Tower was opened from.
    pub workdir: Option<String>,
    pub hover: Option<Hit>,
    pressed: Option<Hit>,
    pub scroll: HashMap<ScrollId, f32>,
    /// Message and the `now_ms` it expires at.
    pub toast: Option<(String, u64)>,
    pub frame: Frame,
    /// Uncommitted IME composition, drawn at the caret.
    pub preedit: Option<String>,
    /// The caret's cell in the pane, for placing the IME candidate window.
    pub caret_cell: Option<(usize, isize)>,
}

impl Model {
    pub fn new(snap: Snapshot) -> Self {
        Self {
            snap,
            conn: Conn::Connecting,
            view: View::Strips,
            drawer: None,
            sheet: None,
            focus: None,
            compose: Field::new(false, "Message…"),
            freq: "team".into(),
            workdir: None,
            hover: None,
            pressed: None,
            scroll: HashMap::new(),
            toast: None,
            frame: Frame::default(),
            preedit: None,
            caret_cell: None,
        }
    }

    /// New work gets its own folder unless Tower was opened from a real
    /// project: opened from the home folder (or nowhere), "here" would mean
    /// dropping files into your home directory.
    pub fn default_place(&self) -> Place {
        let home = std::env::var("HOME")
            .ok()
            .map(|h| h.trim_end_matches('/').to_string());
        match self.workdir.as_deref().map(|w| w.trim_end_matches('/')) {
            None => Place::NewFolder,
            Some(w) if Some(w.to_string()) == home => Place::NewFolder,
            Some(_) => Place::Here,
        }
    }

    pub fn scroll_of(&self, id: ScrollId) -> f32 {
        self.scroll.get(&id).copied().unwrap_or(0.0)
    }

    pub fn live(&self) -> bool {
        matches!(self.conn, Conn::Live { .. })
    }

    /// The field with keyboard focus, wherever it lives.
    pub fn focused_field(&self) -> Option<&Field> {
        match self.focus? {
            FieldId::Compose => Some(&self.compose),
            id => self.sheet.as_ref()?.field(id),
        }
    }

    fn focused_field_mut(&mut self) -> Option<&mut Field> {
        match self.focus? {
            FieldId::Compose => Some(&mut self.compose),
            id => self.sheet.as_mut()?.field_mut(id),
        }
    }

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.frame
            .hits
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h.clone())
    }

    pub fn toast(&mut self, text: impl Into<String>, now_ms: u64) {
        self.toast = Some((text.into(), now_ms + TOAST_MS));
    }

    /// Drop an expired toast. Returns true if something changed.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        match &self.toast {
            Some((_, until)) if *until <= now_ms => {
                self.toast = None;
                true
            }
            _ => false,
        }
    }

    pub fn set_conn(&mut self, conn: Conn) {
        self.conn = conn;
    }

    pub fn apply_server(&mut self, msg: ServerMsg, now_ms: u64) {
        match msg {
            ServerMsg::Hello { daemon_pid, .. } => self.conn = Conn::Live { pid: daemon_pid },
            ServerMsg::Snapshot { snapshot } => {
                let gone = match &self.drawer {
                    Some(Drawer::Flight(id)) => snapshot.flight(id).is_none(),
                    Some(Drawer::Agent(id)) => snapshot.agent(id).is_none(),
                    None => false,
                };
                if gone {
                    self.drawer = None;
                    if self.focus == Some(FieldId::Compose) && self.view != View::Frequencies {
                        self.focus = None;
                    }
                }
                self.snap = snapshot;
            }
            ServerMsg::Error { message } => self.toast(message, now_ms),
            ServerMsg::ToolResult { ok: false, text } => self.toast(text, now_ms),
            _ => {}
        }
    }

    pub fn apply(&mut self, input: Input, now_ms: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        match input {
            Input::Move { x, y } => {
                let hit = self.hit_at(x, y);
                if hit != self.hover {
                    self.hover = hit;
                    fx.push(Effect::Repaint);
                }
            }
            Input::Down { x, y } => {
                self.pressed = self.hit_at(x, y);
                // Clicking a field focuses it at once, like a text box should.
                if let Some(Hit::Field(id)) = &self.pressed {
                    self.focus = Some(*id);
                    fx.push(Effect::Repaint);
                }
            }
            Input::Up { x, y } => {
                let hit = self.hit_at(x, y);
                let pressed = self.pressed.take();
                if let (Some(h), Some(p)) = (hit, pressed) {
                    if h == p {
                        self.activate(h, now_ms, &mut fx);
                        fx.push(Effect::Repaint);
                    }
                }
            }
            Input::Wheel { x, y, dx, dy } => {
                if self.wheel(x, y, dx, dy) {
                    // Content moved under the pointer: hover follows it.
                    self.hover = None;
                    fx.push(Effect::Repaint);
                }
            }
            Input::Key(key) => {
                if self.key(key, now_ms, &mut fx) {
                    fx.push(Effect::Repaint);
                }
            }
            Input::Text(text) => {
                if let Some(f) = self.focused_field_mut() {
                    f.insert(&text);
                    fx.push(Effect::Repaint);
                } else {
                    for c in text.chars() {
                        if self.key(Key::Char(c), now_ms, &mut fx) {
                            fx.push(Effect::Repaint);
                        }
                    }
                }
            }
            Input::Paste(text) => {
                if let Some(f) = self.focused_field_mut() {
                    f.insert(&text);
                    fx.push(Effect::Repaint);
                }
            }
        }
        fx
    }

    fn wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let step = if self.frame.wheel_step > 0.0 {
            self.frame.wheel_step
        } else {
            40.0
        };
        let mut moved = false;
        // Innermost region first: regions are pushed outside-in.
        let vertical = self
            .frame
            .scrolls
            .iter()
            .rev()
            .find(|s| !s.horizontal && s.viewport.contains(x, y) && s.max_offset() > 0.0)
            .cloned();
        if dy != 0.0 {
            if let Some(region) = vertical {
                moved |= self.nudge(&region, -dy * step);
            } else if dx == 0.0 {
                // Nothing vertical to scroll: a plain wheel pans the board.
                if let Some(region) = self.horizontal_at(x, y) {
                    moved |= self.nudge(&region, -dy * step);
                }
            }
        }
        if dx != 0.0 {
            if let Some(region) = self.horizontal_at(x, y) {
                moved |= self.nudge(&region, -dx * step);
            }
        }
        moved
    }

    fn horizontal_at(&self, x: f32, y: f32) -> Option<ScrollRegion> {
        self.frame
            .scrolls
            .iter()
            .rev()
            .find(|s| s.horizontal && s.viewport.contains(x, y) && s.max_offset() > 0.0)
            .cloned()
    }

    /// Move a region by `delta` pixels towards its end (down / right).
    fn nudge(&mut self, region: &ScrollRegion, delta: f32) -> bool {
        let delta = if region.from_bottom { -delta } else { delta };
        let cur = self.scroll_of(region.id).max(0.0).min(region.max_offset());
        let next = (cur + delta).max(0.0).min(region.max_offset());
        if (next - cur).abs() > f32::EPSILON {
            self.scroll.insert(region.id, next);
            true
        } else {
            false
        }
    }

    fn select_view(&mut self, view: View) {
        self.view = view;
        if self.focus == Some(FieldId::Compose) {
            self.focus = None;
        }
    }

    fn key(&mut self, key: Key, now_ms: u64, fx: &mut Vec<Effect>) -> bool {
        if self.focus.is_some() {
            return self.edit_key(key, now_ms, fx);
        }
        if self.sheet.is_some() {
            return match key {
                Key::Escape => {
                    self.sheet = None;
                    true
                }
                Key::Tab => {
                    self.focus = self
                        .sheet
                        .as_ref()
                        .and_then(|s| s.field_ids().first().copied());
                    true
                }
                _ => false,
            };
        }
        match key {
            Key::Escape => self.drawer.take().is_some(),
            Key::Tab => {
                let i = (self.view.index() + 1) % View::ALL.len();
                self.select_view(View::ALL[i]);
                true
            }
            Key::BackTab => {
                let i = (self.view.index() + View::ALL.len() - 1) % View::ALL.len();
                self.select_view(View::ALL[i]);
                true
            }
            Key::Char(c) if ('1'..='6').contains(&c) => {
                self.select_view(View::ALL[c as usize - '1' as usize]);
                true
            }
            Key::Char('n') => {
                self.activate(Hit::Button(Action::NewFlight), now_ms, fx);
                true
            }
            Key::Char('r') => {
                self.activate(Hit::Button(Action::Refresh), now_ms, fx);
                true
            }
            Key::Enter { .. }
                if matches!(self.drawer, Some(Drawer::Agent(_)))
                    || self.view == View::Frequencies =>
            {
                self.focus = Some(FieldId::Compose);
                true
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let id = if self.drawer.is_some() {
                    Some(ScrollId::Drawer)
                } else {
                    match self.view {
                        View::Roster => Some(ScrollId::Roster),
                        View::Inbox => Some(ScrollId::Inbox),
                        View::Frequencies => Some(ScrollId::Frequencies),
                        View::Models => Some(ScrollId::Models),
                        View::Intake => Some(ScrollId::Intake),
                        View::Strips => None,
                    }
                };
                let region =
                    id.and_then(|id| self.frame.scrolls.iter().find(|s| s.id == id).cloned());
                if let Some(region) = region {
                    let step = self.frame.wheel_step.max(1.0);
                    let page = region.viewport.h * 0.9;
                    let delta = match key {
                        Key::Up => -step,
                        Key::Down => step,
                        Key::PageUp => -page,
                        _ => page,
                    };
                    return self.nudge(&region, delta);
                }
                false
            }
            _ => false,
        }
    }

    /// Keys while a text field has focus.
    fn edit_key(&mut self, key: Key, now_ms: u64, fx: &mut Vec<Effect>) -> bool {
        let focus = match self.focus {
            Some(f) => f,
            None => return false,
        };
        match key {
            Key::Escape => {
                if self.sheet.is_some() && focus != FieldId::Compose {
                    self.sheet = None;
                }
                self.focus = None;
            }
            Key::Tab | Key::BackTab => {
                if let Some(sheet) = &self.sheet {
                    let ids = sheet.field_ids();
                    if let Some(i) = ids.iter().position(|f| *f == focus) {
                        let n = ids.len();
                        let next = if key == Key::Tab {
                            (i + 1) % n
                        } else {
                            (i + n - 1) % n
                        };
                        self.focus = Some(ids[next]);
                    }
                }
            }
            Key::Enter { ctrl } => {
                let multiline = self.focused_field().map(|f| f.multiline).unwrap_or(false);
                if multiline && !ctrl {
                    if let Some(f) = self.focused_field_mut() {
                        f.buf.newline();
                    }
                } else if focus == FieldId::Compose {
                    self.action(Action::SendCompose, now_ms, fx);
                } else {
                    self.action(Action::Submit { launch: true }, now_ms, fx);
                }
            }
            other => {
                let f = match self.focused_field_mut() {
                    Some(f) => f,
                    None => return false,
                };
                match other {
                    Key::Backspace => f.buf.backspace(),
                    Key::Delete => f.buf.delete_forward(),
                    Key::Left => f.buf.move_left(),
                    Key::Right => f.buf.move_right(),
                    Key::Up => f.buf.move_up(),
                    Key::Down => f.buf.move_down(),
                    Key::Home => f.buf.move_home(),
                    Key::End => f.buf.move_end(),
                    Key::Char(c) if !c.is_control() => f.buf.insert_char(c),
                    _ => return false,
                }
            }
        }
        true
    }

    fn activate(&mut self, hit: Hit, now_ms: u64, fx: &mut Vec<Effect>) {
        // Anything but a field (or a button inside the form) drops focus.
        match &hit {
            Hit::Field(_) => {}
            Hit::Button(Action::SendCompose) | Hit::Button(Action::Submit { .. }) => {}
            Hit::Button(Action::SheetRole(_))
            | Hit::Button(Action::SheetPlace(_))
            | Hit::Button(Action::SheetTier(_)) => {}
            _ => self.focus = None,
        }
        match hit {
            Hit::Nav(view) => self.select_view(view),
            Hit::Card(id) => {
                if self.drawer == Some(Drawer::Flight(id.clone())) {
                    self.drawer = None;
                } else {
                    self.drawer = Some(Drawer::Flight(id));
                    self.scroll.remove(&ScrollId::Drawer);
                }
            }
            Hit::InboxRow(flight_id) => {
                self.drawer = Some(Drawer::Flight(flight_id));
                self.scroll.remove(&ScrollId::Drawer);
            }
            Hit::Scrim | Hit::CloseDrawer => self.drawer = None,
            Hit::SheetScrim | Hit::Surface => {}
            Hit::Field(id) => self.focus = Some(id),
            Hit::Button(action) => self.action(action, now_ms, fx),
        }
    }

    fn send(&mut self, msg: ClientMsg, now_ms: u64, fx: &mut Vec<Effect>) -> bool {
        if self.live() {
            fx.push(Effect::Send(msg));
            true
        } else {
            self.toast("towerd is not connected", now_ms);
            false
        }
    }

    fn open_sheet(&mut self, sheet: Sheet) {
        self.focus = sheet.field_ids().first().copied();
        self.sheet = Some(sheet);
        self.scroll.remove(&ScrollId::Sheet);
    }

    fn action(&mut self, action: Action, now_ms: u64, fx: &mut Vec<Effect>) {
        match action {
            Action::NewFlight => {
                let place = self.default_place();
                // With a triage role around, the default is "let Tower decide".
                let role =
                    Some(AUTO_ROLE.to_string()).filter(|_| !self.snap.triage_role.is_empty());
                self.open_sheet(Sheet::new_flight(role, place))
            }
            Action::OpenLaunch { role_id } => {
                let name = self
                    .snap
                    .role(&role_id)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| role_id.clone());
                let place = self.default_place();
                self.open_sheet(Sheet::launch(&role_id, &name, place));
            }
            Action::LaunchFlight { flight_id, role_id } => {
                let workdir = self.workdir.clone();
                self.send(
                    ClientMsg::LaunchAgent {
                        role_id,
                        flight_id: Some(flight_id),
                        task: None,
                        workdir,
                        new_folder: false,
                    },
                    now_ms,
                    fx,
                );
            }
            Action::OpenPacket { flight_id } => {
                if let Some(f) = self.snap.flight(&flight_id) {
                    let sheet = Sheet::packet(&f.id, &f.key, &f.title);
                    self.open_sheet(sheet);
                }
            }
            Action::Answer { item_id } => {
                if let Some(i) = self.snap.inbox.iter().find(|i| i.id == item_id) {
                    let sheet = Sheet::answer(&i.id, &i.title, &i.detail);
                    self.open_sheet(sheet);
                }
            }
            Action::Resolve {
                item_id,
                approve,
                option_id,
            } => {
                self.send(
                    ClientMsg::ResolveInbox {
                        item_id,
                        approve,
                        option_id,
                        text: None,
                    },
                    now_ms,
                    fx,
                );
            }
            Action::Release {
                flight_id,
                handoff_id,
            } => {
                self.send(
                    ClientMsg::ReleaseHandoff {
                        flight_id,
                        handoff_id,
                    },
                    now_ms,
                    fx,
                );
            }
            Action::Escalate { flight_id } => {
                if self.send(ClientMsg::Escalate { flight_id }, now_ms, fx) {
                    self.toast("Escalating to the next model…", now_ms);
                }
            }
            Action::DeleteFlight { flight_id } => {
                if self.send(ClientMsg::DeleteFlight { flight_id }, now_ms, fx) {
                    self.drawer = None;
                }
            }
            Action::StopAgent { agent_id } => {
                self.send(ClientMsg::StopAgent { agent_id }, now_ms, fx);
            }
            Action::FocusPane { pane_id } => fx.push(Effect::FocusPane(pane_id)),
            Action::OpenAgent { agent_id } => {
                self.drawer = Some(Drawer::Agent(agent_id));
                self.scroll.remove(&ScrollId::Drawer);
            }
            Action::OpenFlight { flight_id } => {
                self.drawer = Some(Drawer::Flight(flight_id));
                self.scroll.remove(&ScrollId::Drawer);
            }
            Action::EditFile { path } => {
                let dir = std::path::Path::new(&path)
                    .parent()
                    .map(|p| p.display().to_string());
                fx.push(Effect::Split {
                    argv: Some(edit_argv(&path)),
                    cwd: dir,
                });
            }
            Action::OpenTerminal { cwd } => fx.push(Effect::Split {
                argv: None,
                cwd: cwd.or_else(|| self.workdir.clone()),
            }),
            Action::PollSources => {
                if self.send(ClientMsg::PollSources, now_ms, fx) {
                    self.toast("Polling intake…", now_ms);
                }
            }
            Action::Probe => {
                if self.send(ClientMsg::ProbeEndpoints, now_ms, fx) {
                    self.toast("Probing model servers…", now_ms);
                }
            }
            Action::Refresh => {
                self.send(ClientMsg::GetSnapshot, now_ms, fx);
            }
            Action::SheetRole(role) => match &mut self.sheet {
                Some(Sheet::NewFlight(f)) => f.role = role,
                Some(Sheet::Packet(f)) => f.to_role = role,
                _ => {}
            },
            Action::SheetPlace(place) => match &mut self.sheet {
                Some(Sheet::NewFlight(f)) => f.place = place,
                Some(Sheet::Launch(f)) if place != Place::Worktree => f.place = place,
                _ => {}
            },
            Action::OpenPath { path } => fx.push(Effect::Open(path)),
            Action::SheetTier(tier) => {
                if let Some(Sheet::NewFlight(f)) = &mut self.sheet {
                    f.tier = tier;
                }
            }
            Action::CloseSheet => {
                self.sheet = None;
                self.focus = None;
            }
            Action::Submit { launch } => self.submit(launch, now_ms, fx),
            Action::SelectFrequency(id) => {
                self.freq = id;
                self.scroll.remove(&ScrollId::Frequencies);
            }
            Action::SendCompose => self.send_compose(now_ms, fx),
        }
    }

    fn submit(&mut self, launch: bool, now_ms: u64, fx: &mut Vec<Effect>) {
        let sheet = match &self.sheet {
            Some(s) => s,
            None => return,
        };
        if let Some(why) = sheet.blocker() {
            self.toast(why, now_ms);
            return;
        }
        let workdir = self.workdir.clone();
        let (msg, note) = match sheet {
            Sheet::NewFlight(f) => {
                let auto = f.role.as_deref() == Some(AUTO_ROLE);
                let launch = launch && f.role.is_some();
                (
                    ClientMsg::CreateFlight {
                        title: f.title.text().trim().to_string(),
                        brief: f.brief.text().trim().to_string(),
                        role_id: f.role.clone().filter(|_| !auto),
                        workdir,
                        worktree: f.place == Place::Worktree,
                        new_folder: f.place == Place::NewFolder,
                        auto_assign: auto,
                        launch,
                        tier: f.tier.clone(),
                    },
                    match (auto, launch) {
                        (true, true) => "Flight created; triage is picking who takes it",
                        (true, false) => "Flight created; triage is picking a role",
                        (false, true) => "Flight created, agent taking off",
                        (false, false) => "Flight created",
                    },
                )
            }
            Sheet::Launch(f) => (
                ClientMsg::LaunchAgent {
                    role_id: f.role_id.clone(),
                    flight_id: None,
                    task: Some(f.task.text().trim().to_string()),
                    workdir,
                    new_folder: f.place == Place::NewFolder,
                },
                "Agent taking off",
            ),
            Sheet::Packet(f) => (
                ClientMsg::HandDown {
                    flight_id: f.flight_id.clone(),
                    packet: HandoffPacket {
                        to_role: f.to_role.clone().unwrap_or_default(),
                        goal: f.goal.text().trim().to_string(),
                        context: f.context.lines(),
                        constraints: f.constraints.lines(),
                        acceptance: f.acceptance.lines(),
                        ..Default::default()
                    },
                },
                "Handoff released",
            ),
            Sheet::Answer(f) => (
                ClientMsg::ResolveInbox {
                    item_id: f.item_id.clone(),
                    approve: true,
                    option_id: None,
                    text: Some(f.text.text().trim().to_string()),
                },
                "Answer sent",
            ),
        };
        if self.send(msg, now_ms, fx) {
            self.sheet = None;
            self.focus = None;
            self.toast(note, now_ms);
        }
    }

    fn send_compose(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let text = self.compose.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let msg = match (&self.drawer, self.view) {
            (Some(Drawer::Agent(id)), _) => ClientMsg::SendMessage {
                agent_id: id.clone(),
                text,
            },
            (_, View::Frequencies) => ClientMsg::Post {
                frequency_id: self.freq.clone(),
                text,
            },
            _ => return,
        };
        if self.send(msg, now_ms, fx) {
            self.compose.buf.clear();
            self.scroll.remove(&ScrollId::Drawer);
            self.scroll.remove(&ScrollId::Frequencies);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::proto::demo_snapshot;
    use super::*;

    fn model_with_hits(hits: Vec<(Rect, Hit)>) -> Model {
        let mut m = Model::new(demo_snapshot(1_000_000));
        m.frame.hits = hits;
        m
    }

    fn live(m: &mut Model) {
        m.apply_server(
            ServerMsg::Hello {
                version: 2,
                daemon_pid: 42,
            },
            0,
        );
    }

    fn click(m: &mut Model, x: f32, y: f32) -> Vec<Effect> {
        m.apply(Input::Down { x, y }, 0);
        m.apply(Input::Up { x, y }, 0)
    }

    fn type_text(m: &mut Model, s: &str) {
        for c in s.chars() {
            m.apply(Input::Key(Key::Char(c)), 0);
        }
    }

    fn sent(fx: &[Effect]) -> Vec<ClientMsg> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Send(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn click_needs_press_and_release_on_the_same_target() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 0.0, 10.0, 10.0);
        let mut m = model_with_hits(vec![
            (a, Hit::Nav(View::Roster)),
            (b, Hit::Nav(View::Inbox)),
        ]);
        m.apply(Input::Down { x: 5.0, y: 5.0 }, 0);
        m.apply(Input::Up { x: 25.0, y: 5.0 }, 0);
        assert_eq!(m.view, View::Strips, "drag off a target cancels");
        click(&mut m, 25.0, 5.0);
        assert_eq!(m.view, View::Inbox);
    }

    #[test]
    fn cards_toggle_the_flight_drawer_and_escape_closes_it() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut m = model_with_hits(vec![(r, Hit::Surface), (r, Hit::Card("f-12".into()))]);
        click(&mut m, 1.0, 1.0);
        assert_eq!(m.drawer, Some(Drawer::Flight("f-12".into())));
        click(&mut m, 1.0, 1.0);
        assert_eq!(m.drawer, None);
        click(&mut m, 1.0, 1.0);
        m.apply(Input::Key(Key::Escape), 0);
        assert_eq!(m.drawer, None);
    }

    #[test]
    fn hover_changes_request_repaint_once() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut m = model_with_hits(vec![(r, Hit::Surface)]);
        assert_eq!(
            m.apply(Input::Move { x: 1.0, y: 1.0 }, 0),
            vec![Effect::Repaint]
        );
        assert!(m.apply(Input::Move { x: 2.0, y: 2.0 }, 0).is_empty());
        assert_eq!(
            m.apply(Input::Move { x: 50.0, y: 2.0 }, 0),
            vec![Effect::Repaint]
        );
    }

    #[test]
    fn wheel_scrolls_innermost_region_and_clamps() {
        let mut m = model_with_hits(vec![]);
        m.frame.wheel_step = 10.0;
        m.frame.scrolls = vec![
            ScrollRegion {
                id: ScrollId::BoardX,
                viewport: Rect::new(0.0, 0.0, 100.0, 100.0),
                content: 300.0,
                horizontal: true,
                from_bottom: false,
            },
            ScrollRegion {
                id: ScrollId::Column(Stage::Holding),
                viewport: Rect::new(0.0, 20.0, 50.0, 80.0),
                content: 100.0,
                horizontal: false,
                from_bottom: false,
            },
        ];
        m.apply(
            Input::Wheel {
                x: 10.0,
                y: 30.0,
                dx: 0.0,
                dy: -1.0,
            },
            0,
        );
        assert_eq!(m.scroll_of(ScrollId::Column(Stage::Holding)), 10.0);
        for _ in 0..10 {
            m.apply(
                Input::Wheel {
                    x: 10.0,
                    y: 30.0,
                    dx: 0.0,
                    dy: -1.0,
                },
                0,
            );
        }
        assert_eq!(m.scroll_of(ScrollId::Column(Stage::Holding)), 20.0);
        m.apply(
            Input::Wheel {
                x: 80.0,
                y: 30.0,
                dx: 0.0,
                dy: -1.0,
            },
            0,
        );
        assert_eq!(
            m.scroll_of(ScrollId::BoardX),
            10.0,
            "outside the column the wheel pans the board"
        );
    }

    #[test]
    fn chat_regions_scroll_from_the_bottom() {
        let mut m = model_with_hits(vec![]);
        m.frame.wheel_step = 10.0;
        m.frame.scrolls = vec![ScrollRegion {
            id: ScrollId::Drawer,
            viewport: Rect::new(0.0, 0.0, 100.0, 100.0),
            content: 300.0,
            horizontal: false,
            from_bottom: true,
        }];
        // Wheel up (towards older messages) grows the distance from the bottom.
        m.apply(
            Input::Wheel {
                x: 1.0,
                y: 1.0,
                dx: 0.0,
                dy: 2.0,
            },
            0,
        );
        assert_eq!(m.scroll_of(ScrollId::Drawer), 20.0);
    }

    #[test]
    fn new_flight_form_types_and_submits() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.workdir = Some("/repo".into());
        m.apply(Input::Key(Key::Char('n')), 0);
        assert!(matches!(m.sheet, Some(Sheet::NewFlight(_))));
        assert_eq!(m.focus, Some(FieldId::Title));
        // Digits are text now, not view switches.
        type_text(&mut m, "Add v2 flag");
        assert_eq!(m.view, View::Strips);
        m.apply(Input::Key(Key::Tab), 0);
        assert_eq!(m.focus, Some(FieldId::Brief));
        m.apply(Input::Paste("line one\nline two".into()), 0);
        m.apply(Input::Key(Key::Enter { ctrl: false }), 0);
        assert!(
            m.sheet.is_some(),
            "Enter in a multi-line field is a newline"
        );
        if let Some(Sheet::NewFlight(f)) = &mut m.sheet {
            f.role = Some("approach".into());
        }
        let fx = m.apply(Input::Key(Key::Enter { ctrl: true }), 0);
        match sent(&fx).as_slice() {
            [ClientMsg::CreateFlight {
                title,
                brief,
                role_id,
                workdir,
                launch,
                worktree,
                new_folder,
                auto_assign,
                tier,
            }] => {
                assert_eq!(tier, &None, "Auto by default");
                assert_eq!(title, "Add v2 flag");
                assert_eq!(brief, "line one\nline two");
                assert_eq!(role_id.as_deref(), Some("approach"));
                assert_eq!(workdir.as_deref(), Some("/repo"));
                assert!(*launch);
                assert!(!*worktree);
                assert!(!*new_folder, "opened in /repo: work happens here");
                assert!(!*auto_assign, "a role was picked by hand");
            }
            other => panic!("{other:?}"),
        }
        assert!(m.sheet.is_none());
        assert_eq!(m.focus, None);
    }

    #[test]
    fn auto_is_the_default_and_lets_triage_pick() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.apply(Input::Key(Key::Char('n')), 0);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert_eq!(f.role.as_deref(), Some(AUTO_ROLE)),
            _ => panic!("no sheet"),
        }
        type_text(&mut m, "Fix the flaky test");
        let fx = m.apply(Input::Key(Key::Enter { ctrl: false }), 0);
        match sent(&fx).as_slice() {
            [ClientMsg::CreateFlight {
                role_id,
                auto_assign,
                launch,
                ..
            }] => {
                assert_eq!(role_id, &None, "the sentinel never leaves the GUI");
                assert!(*auto_assign);
                assert!(*launch);
            }
            other => panic!("{other:?}"),
        }
        // Without a triage role there is no Auto default.
        m.snap.triage_role.clear();
        m.apply(Input::Key(Key::Char('n')), 0);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert_eq!(f.role, None),
            _ => panic!("no sheet"),
        }
    }

    #[test]
    fn empty_forms_explain_instead_of_sending() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.apply(Input::Key(Key::Char('n')), 0);
        let fx = m.apply(Input::Key(Key::Enter { ctrl: false }), 0);
        assert!(sent(&fx).is_empty());
        assert!(m.toast.as_ref().unwrap().0.contains("title"));
        m.apply(Input::Key(Key::Escape), 0);
        assert!(m.sheet.is_none());
    }

    #[test]
    fn launch_form_sends_the_task() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        let mut fx = Vec::new();
        m.action(
            Action::OpenLaunch {
                role_id: "approach".into(),
            },
            0,
            &mut fx,
        );
        type_text(&mut m, "Fix the login loop");
        let fx = m.apply(Input::Key(Key::Enter { ctrl: true }), 0);
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::LaunchAgent {
                role_id: "approach".into(),
                flight_id: None,
                task: Some("Fix the login loop".into()),
                workdir: None,
                new_folder: true
            }]
        );
    }

    #[test]
    fn compose_sends_to_the_open_agent_and_clears() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let mut m = model_with_hits(vec![(r, Hit::Field(FieldId::Compose))]);
        live(&mut m);
        m.drawer = Some(Drawer::Agent("ground-1".into()));
        click(&mut m, 1.0, 1.0);
        assert_eq!(m.focus, Some(FieldId::Compose));
        type_text(&mut m, "use the fixtures");
        let fx = m.apply(Input::Key(Key::Enter { ctrl: false }), 0);
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::SendMessage {
                agent_id: "ground-1".into(),
                text: "use the fixtures".into()
            }]
        );
        assert!(m.compose.is_empty());
        assert_eq!(
            m.focus,
            Some(FieldId::Compose),
            "keeps focus for the next message"
        );
    }

    #[test]
    fn actions_need_a_live_daemon() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let resolve = Hit::Button(Action::Resolve {
            item_id: "i-5".into(),
            approve: true,
            option_id: None,
        });
        let mut m = model_with_hits(vec![(r, resolve)]);
        assert!(sent(&click(&mut m, 1.0, 1.0)).is_empty());
        assert!(m.toast.is_some());
        live(&mut m);
        assert_eq!(sent(&click(&mut m, 1.0, 1.0)).len(), 1);
    }

    #[test]
    fn edit_and_terminal_open_splits() {
        let mut m = model_with_hits(vec![]);
        m.workdir = Some("/repo".into());
        let mut fx = Vec::new();
        m.action(
            Action::EditFile {
                path: "/cfg/roles/ground.toml".into(),
            },
            0,
            &mut fx,
        );
        m.action(Action::OpenTerminal { cwd: None }, 0, &mut fx);
        match &fx[0] {
            Effect::Split {
                argv: Some(argv),
                cwd,
            } => {
                assert_eq!(argv.last().unwrap(), "/cfg/roles/ground.toml");
                assert_eq!(cwd.as_deref(), Some("/cfg/roles"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            fx[1],
            Effect::Split {
                argv: None,
                cwd: Some("/repo".into())
            }
        );
    }

    #[test]
    fn snapshot_closes_drawers_of_vanished_things_and_toasts_expire() {
        let mut m = model_with_hits(vec![]);
        m.drawer = Some(Drawer::Flight("f-12".into()));
        let mut snap = demo_snapshot(1);
        snap.flights.retain(|f| f.id != "f-12");
        m.apply_server(ServerMsg::Snapshot { snapshot: snap }, 0);
        assert_eq!(m.drawer, None);
        m.apply_server(
            ServerMsg::Error {
                message: "x".into(),
            },
            100,
        );
        assert!(!m.tick(100 + TOAST_MS - 1));
        assert!(m.tick(100 + TOAST_MS));
    }
}
