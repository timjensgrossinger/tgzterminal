//! View model and reducers. Pure: inputs and server messages in, effects out.

use super::clip::Rect;
use super::forms::{Field, FieldId, Place, Sheet, SheetFlag, AUTO_ROLE};
use super::proto::{ClientMsg, HandoffPacket, ServerMsg, Snapshot, SnapshotOrigin, Stage};
use std::collections::HashMap;

/// The three views, plus Team while the frequencies extra is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    /// Flights by stage, with what is waiting on you at the top.
    Board,
    /// Roles, models, routing, policy, sources, schedules, extras.
    Setup,
    /// What ran by itself; activity charts when that extra is on.
    History,
    /// The team frequency and flight threads (extras.frequencies).
    Team,
}

impl View {
    pub const ALL: [View; 3] = [View::Board, View::Setup, View::History];

    pub fn label(self) -> &'static str {
        match self {
            View::Board => "Board",
            View::Setup => "Setup",
            View::History => "History",
            View::Team => "Team",
        }
    }
}

/// A side feature behind `[extras]` in tower.toml, toggled in Setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Extra {
    Scouts,
    MachineRoutines,
    Activity,
    Frequencies,
}

impl Extra {
    pub const ALL: [Extra; 4] = [
        Extra::Scouts,
        Extra::MachineRoutines,
        Extra::Activity,
        Extra::Frequencies,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Extra::Scouts => "Scouts",
            Extra::MachineRoutines => "This machine's jobs",
            Extra::Activity => "Activity charts",
            Extra::Frequencies => "Team frequency",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Extra::Scouts => {
                "Agents read mail or Teams with their own M365 tools and report what needs work ([[scout]] in sources.toml)."
            }
            Extra::MachineRoutines => {
                "Your launchd agents and crontab, read-only, listed under Schedules."
            }
            Extra::Activity => {
                "Tiles and charts in History: what ran by itself, how runs ended, what it cost. Counting never stops."
            }
            Extra::Frequencies => {
                "A Team view: a chat channel plus one thread per flight; agents post with tower_post."
            }
        }
    }

    pub fn get(self, e: &super::proto::Extras) -> bool {
        match self {
            Extra::Scouts => e.scouts,
            Extra::MachineRoutines => e.machine_routines,
            Extra::Activity => e.activity,
            Extra::Frequencies => e.frequencies,
        }
    }

    pub fn set(self, e: &mut super::proto::Extras, on: bool) {
        match self {
            Extra::Scouts => e.scouts = on,
            Extra::MachineRoutines => e.machine_routines = on,
            Extra::Activity => e.activity = on,
            Extra::Frequencies => e.frequencies = on,
        }
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
    /// The "waiting on you" strip on the Board.
    Inbox,
    Frequencies,
    Setup,
    History,
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
    /// Save state and start towerd afresh (new build, current PATH).
    RestartDaemon,
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
    /// Arm or disarm automatic actions.
    SetArmed(bool),
    /// Start a run of a scout now.
    RunScout(String),
    NewSchedule,
    EditSchedule(String),
    /// Fire a schedule now.
    RunSchedule(String),
    SetScheduleEnabled {
        id: String,
        enabled: bool,
    },
    /// Delete takes two clicks.
    DeleteSchedule(String),
    /// Schedule form: fill in one of `forms::TEMPLATES`.
    ScheduleTemplate(usize),
    NewEndpoint,
    EditEndpoint(String),
    /// Delete takes two clicks.
    DeleteEndpoint(String),
    /// Add a model; `Some(<endpoint>/<name>)` starts from a served one.
    NewModel(Option<String>),
    EditModel(String),
    /// Delete takes two clicks.
    DeleteModel(String),
    /// Server form: its kind.
    SheetKind(String),
    /// Server form: local, cloud, or `None` = guess from the address.
    SheetLocal(Option<bool>),
    /// Model form: the server it runs on.
    SheetEndpoint(String),
    /// Ask before connecting (`true`) or disconnecting the CLIs.
    OpenConnect(bool),
    NewRole,
    EditRole(String),
    /// Delete takes two clicks.
    DeleteRole(String),
    /// Role form: how it runs.
    SheetDriver(String),
    /// Role form: its colour.
    SheetColor(String),
    /// Role form: append a model to the ladder.
    RoleAddModel(String),
    EditRouting,
    OpenLocalOnly,
    /// Policy view: set an action's level.
    SetPolicyLevel {
        action: String,
        level: String,
    },
    SheetFlag(SheetFlag),
    /// Show or hide vendor launchd jobs (updaters and the like).
    ToggleOtherRoutines,
    /// Activity: how many days the charts show.
    ActivityDays(usize),
    /// Setup: accept a vendor-terms note (its matrix id).
    AcceptTerms(String),
    /// Setup: fold a terms note away for this session ("Not now"), or back.
    DeferTerms {
        id: String,
        defer: bool,
    },
    /// Setup: switch a side feature on or off (writes `[extras]`).
    ToggleExtra(Extra),
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
        fanouts: vec![],
        models: vec![],
        connected_clis: vec![],
        engine_ids: vec![],
        local_only_paths: vec![],
        bandit_mode: String::new(),
        armed_until: None,
        policy: vec![],
        schedules: vec![],
        routines: vec![],
        routines_note: String::new(),
        activity: Default::default(),
        extras: Default::default(),
        notices: vec![],
        setup: Default::default(),
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
    /// Activity: days shown (7, 30 or 90).
    pub activity_days: usize,
    /// Routines: vendor launchd jobs are listed too.
    pub show_other_routines: bool,
    /// A schedule whose Delete was clicked once.
    pub confirm_delete: Option<String>,
    /// Terms notes folded away with "Not now" (this session only).
    pub terms_deferred: Vec<String>,
}

impl Model {
    pub fn new(snap: Snapshot) -> Self {
        Self {
            snap,
            conn: Conn::Connecting,
            view: View::Board,
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
            activity_days: 7,
            show_other_routines: false,
            confirm_delete: None,
            terms_deferred: Vec::new(),
        }
    }

    /// The views in the rail: the three, plus Team while frequencies are on.
    pub fn views(&self) -> Vec<View> {
        let mut v = View::ALL.to_vec();
        if self.snap.extras.frequencies {
            v.push(View::Team);
        }
        v
    }

    /// Terms notes the user has not accepted and not folded away.
    pub fn terms_due(&self) -> Vec<&super::proto::TermsNote> {
        self.snap
            .setup
            .terms_pending
            .iter()
            .filter(|t| !self.terms_deferred.contains(&t.id))
            .collect()
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

    /// "Auto" is worth offering: a triage role exists and can run.
    /// "Auto" is worth offering when triage can run, or when heuristics
    /// have a role to fall back to.
    pub fn auto_available(&self) -> bool {
        (!self.snap.triage_role.is_empty() && self.snap.setup.triage_usable)
            || !self.snap.setup.auto_heuristic_role.is_empty()
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
                    if self.focus == Some(FieldId::Compose) && self.view != View::Team {
                        self.focus = None;
                    }
                }
                self.snap = snapshot;
                if self.view == View::Team && !self.snap.extras.frequencies {
                    self.select_view(View::Board);
                }
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
        let views = self.views();
        let index = views.iter().position(|v| *v == self.view).unwrap_or(0);
        match key {
            Key::Escape => self.drawer.take().is_some(),
            Key::Tab => {
                self.select_view(views[(index + 1) % views.len()]);
                true
            }
            Key::BackTab => {
                self.select_view(views[(index + views.len() - 1) % views.len()]);
                true
            }
            // `1`..: a view; a digit past the last view does nothing.
            Key::Char(c) if c.is_ascii_digit() && c != '0' => {
                match views.get(c as usize - '1' as usize) {
                    Some(v) => {
                        self.select_view(*v);
                        true
                    }
                    None => false,
                }
            }
            Key::Char('n') => {
                self.activate(Hit::Button(Action::NewFlight), now_ms, fx);
                true
            }
            Key::Char('r') => {
                self.activate(Hit::Button(Action::Refresh), now_ms, fx);
                true
            }
            Key::Char('s') => {
                self.activate(Hit::Button(Action::NewSchedule), now_ms, fx);
                true
            }
            Key::Enter { .. }
                if matches!(self.drawer, Some(Drawer::Agent(_))) || self.view == View::Team =>
            {
                self.focus = Some(FieldId::Compose);
                true
            }
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let id = if self.drawer.is_some() {
                    Some(ScrollId::Drawer)
                } else {
                    match self.view {
                        // The strip; with nothing waiting there is no region.
                        View::Board => Some(ScrollId::Inbox),
                        View::Setup => Some(ScrollId::Setup),
                        View::History => Some(ScrollId::History),
                        View::Team => Some(ScrollId::Frequencies),
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
                    Key::Char(c) if !c.is_control() => f.insert_char(c),
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
            | Hit::Button(Action::SheetTier(_))
            | Hit::Button(Action::SheetKind(_))
            | Hit::Button(Action::SheetLocal(_))
            | Hit::Button(Action::SheetEndpoint(_))
            | Hit::Button(Action::SheetDriver(_))
            | Hit::Button(Action::SheetColor(_))
            | Hit::Button(Action::RoleAddModel(_)) => {}
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
                // With a triage role that can run, the default is "let Tower decide".
                let role = Some(AUTO_ROLE.to_string()).filter(|_| self.auto_available());
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
            Action::RestartDaemon => {
                if self.confirm_delete.as_deref() == Some("restart-daemon") {
                    self.confirm_delete = None;
                    if self.send(ClientMsg::RestartDaemon, now_ms, fx) {
                        self.toast("Restarting towerd…", now_ms);
                    }
                } else {
                    self.confirm_delete = Some("restart-daemon".into());
                    let acp = self
                        .snap
                        .agents
                        .iter()
                        .filter(|a| a.status.is_live() && a.driver == super::proto::DriverKind::Acp)
                        .count();
                    self.toast(
                        format!(
                            "Click again to restart towerd (stops {acp} ACP agent(s); pane agents keep running)"
                        ),
                        now_ms,
                    );
                }
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
            Action::RunScout(name) => {
                if self.send(ClientMsg::RunScout { name: name.clone() }, now_ms, fx) {
                    self.toast(&format!("Starting scout {name}…"), now_ms);
                }
            }
            Action::SetArmed(on) => {
                if self.send(ClientMsg::SetArmed { on }, now_ms, fx) {
                    self.toast(
                        if on {
                            "Armed: auto actions run on their own until the timer ends"
                        } else {
                            "Disarmed: Tower asks before everything"
                        },
                        now_ms,
                    );
                }
            }
            Action::Probe => {
                if self.send(ClientMsg::ProbeEndpoints, now_ms, fx) {
                    self.toast("Probing model servers…", now_ms);
                }
            }
            Action::Refresh => {
                if self.view == View::Setup && self.snap.extras.machine_routines {
                    self.send(ClientMsg::RefreshRoutines, now_ms, fx);
                }
                self.send(ClientMsg::GetSnapshot, now_ms, fx);
            }
            Action::NewSchedule => {
                let mut sheet = Sheet::schedule(None, self.workdir.as_deref());
                if let Sheet::Schedule(f) = &mut sheet {
                    f.place = self.default_place();
                    f.role = Some(AUTO_ROLE.to_string()).filter(|_| self.auto_available());
                }
                self.open_sheet(sheet);
            }
            Action::AcceptTerms(note_id) => {
                if self.send(ClientMsg::AcceptTerms { note_id }, now_ms, fx) {
                    self.toast("Accepted", now_ms);
                }
            }
            Action::DeferTerms { id, defer } => {
                self.terms_deferred.retain(|t| *t != id);
                if defer {
                    self.terms_deferred.push(id);
                }
            }
            Action::ToggleExtra(extra) => {
                let mut extras = self.snap.extras;
                let on = !extra.get(&extras);
                extra.set(&mut extras, on);
                if self.send(ClientMsg::SetExtras { extras }, now_ms, fx) {
                    // Show the change at once; the daemon's snapshot confirms it.
                    self.snap.extras = extras;
                    if self.view == View::Team && !extras.frequencies {
                        self.select_view(View::Board);
                    }
                    self.toast(
                        format!(
                            "{} {}",
                            extra.label(),
                            if extra.get(&extras) { "on" } else { "off" }
                        ),
                        now_ms,
                    );
                }
            }
            Action::EditSchedule(id) => {
                let spec = self
                    .snap
                    .schedules
                    .iter()
                    .find(|s| s.spec.id == id)
                    .map(|s| s.spec.clone());
                if let Some(spec) = spec {
                    self.open_sheet(Sheet::schedule(Some(&spec), self.workdir.as_deref()));
                }
            }
            Action::RunSchedule(id) => {
                if self.send(ClientMsg::RunSchedule { id }, now_ms, fx) {
                    self.toast("Starting a run now…", now_ms);
                }
            }
            Action::SetScheduleEnabled { id, enabled } => {
                if self.send(ClientMsg::SetScheduleEnabled { id, enabled }, now_ms, fx) {
                    self.toast(if enabled { "Resumed" } else { "Paused" }, now_ms);
                }
            }
            Action::DeleteSchedule(id) => {
                if self.confirm_delete.as_deref() == Some(id.as_str()) {
                    self.confirm_delete = None;
                    if self.send(ClientMsg::DeleteSchedule { id }, now_ms, fx) {
                        self.toast("Schedule deleted", now_ms);
                    }
                } else {
                    self.confirm_delete = Some(id);
                    self.toast("Click Delete again to remove the schedule", now_ms);
                }
            }
            Action::ScheduleTemplate(i) => {
                if let Some(Sheet::Schedule(f)) = &mut self.sheet {
                    f.apply_template(i);
                }
            }
            Action::SheetFlag(flag) => match &mut self.sheet {
                Some(Sheet::Schedule(f)) => match flag {
                    SheetFlag::Overlap => f.overlap_run = !f.overlap_run,
                    SheetFlag::CatchUp => f.catch_up = !f.catch_up,
                    SheetFlag::LocalOnly => f.local_only = !f.local_only,
                    SheetFlag::HandsFree => f.hands_free = !f.hands_free,
                    SheetFlag::Heuristic => {}
                },
                Some(Sheet::NewFlight(f)) if flag == SheetFlag::LocalOnly => {
                    f.local_only = !f.local_only
                }
                Some(Sheet::NewFlight(f)) if flag == SheetFlag::Heuristic => {
                    f.heuristic = !f.heuristic
                }
                Some(Sheet::NewFlight(f)) if flag == SheetFlag::HandsFree => {
                    f.hands_free = !f.hands_free
                }
                _ => {}
            },
            Action::NewEndpoint => self.open_sheet(Sheet::endpoint(None)),
            Action::EditEndpoint(name) => {
                let ep = self.snap.endpoints.iter().find(|e| e.name == name).cloned();
                if let Some(ep) = ep {
                    self.open_sheet(Sheet::endpoint(Some(&ep)));
                }
            }
            Action::DeleteEndpoint(name) => {
                let key = format!("endpoint:{}", name);
                if self.confirm_delete.as_deref() == Some(key.as_str()) {
                    self.confirm_delete = None;
                    if self.send(ClientMsg::DeleteEndpoint { name }, now_ms, fx) {
                        self.sheet = None;
                        self.focus = None;
                        self.toast("Model server removed", now_ms);
                    }
                } else {
                    self.confirm_delete = Some(key);
                    self.toast(
                        "Click Delete again to remove the server and its models",
                        now_ms,
                    );
                }
            }
            Action::NewModel(prefill) => {
                let sheet = Sheet::model(None, prefill.as_deref());
                self.open_sheet(sheet);
            }
            Action::EditModel(id) => {
                let m = self.snap.models.iter().find(|m| m.id == id).cloned();
                if let Some(m) = m {
                    self.open_sheet(Sheet::model(Some(&m), None));
                }
            }
            Action::DeleteModel(id) => {
                let key = format!("model:{}", id);
                if self.confirm_delete.as_deref() == Some(key.as_str()) {
                    self.confirm_delete = None;
                    if self.send(ClientMsg::DeleteModel { id }, now_ms, fx) {
                        self.sheet = None;
                        self.focus = None;
                        self.toast("Model removed", now_ms);
                    }
                } else {
                    self.confirm_delete = Some(key);
                    self.toast("Click Delete again to remove the model", now_ms);
                }
            }
            Action::SheetKind(kind) => {
                if let Some(Sheet::Endpoint(f)) = &mut self.sheet {
                    // A blank address follows the kind's usual one.
                    let usual = |k: &str| {
                        super::forms::ENDPOINT_KINDS
                            .iter()
                            .find(|(n, _)| *n == k)
                            .map(|(_, u)| *u)
                    };
                    let was = f.url.text();
                    if was.trim().is_empty() || usual(&f.kind) == Some(was.trim()) {
                        if let Some(u) = usual(&kind) {
                            f.url.buf.set_text(u);
                        }
                    }
                    f.kind = kind;
                }
            }
            Action::OpenConnect(on) => self.open_sheet(Sheet::Connect(on)),
            Action::NewRole => self.open_sheet(Sheet::role(None)),
            Action::EditRole(id) => {
                let r = self.snap.role(&id).cloned();
                if let Some(r) = r {
                    self.open_sheet(Sheet::role(Some(&r)));
                }
            }
            Action::DeleteRole(id) => {
                let key = format!("role:{}", id);
                if self.confirm_delete.as_deref() == Some(key.as_str()) {
                    self.confirm_delete = None;
                    if self.send(ClientMsg::DeleteRole { id }, now_ms, fx) {
                        self.sheet = None;
                        self.focus = None;
                        self.toast("Role removed (its file is kept as .bak)", now_ms);
                    }
                } else {
                    self.confirm_delete = Some(key);
                    self.toast("Click Delete again to remove the role", now_ms);
                }
            }
            Action::SheetDriver(d) => {
                if let Some(Sheet::Role(f)) = &mut self.sheet {
                    f.driver = d;
                }
            }
            Action::SheetColor(c) => {
                if let Some(Sheet::Role(f)) = &mut self.sheet {
                    f.color = c;
                }
            }
            Action::RoleAddModel(m) => {
                if let Some(Sheet::Role(f)) = &mut self.sheet {
                    f.add_model(&m);
                }
            }
            Action::EditRouting => {
                let tier = |t: &str| -> Vec<String> {
                    self.snap
                        .engines
                        .iter()
                        .filter(|e| e.tier == t)
                        .map(|e| e.id.clone())
                        .collect()
                };
                let sheet = Sheet::routing(&tier("low"), &tier("medium"), &tier("high"));
                self.open_sheet(sheet);
            }
            Action::OpenLocalOnly => {
                let sheet = Sheet::local_only(&self.snap.local_only_paths);
                self.open_sheet(sheet);
            }
            Action::SetPolicyLevel { action, level } => {
                let note = format!("{} set to {}", action, level);
                if self.send(
                    ClientMsg::SetPolicyLevel {
                        action,
                        level: Some(level),
                    },
                    now_ms,
                    fx,
                ) {
                    self.toast(note, now_ms);
                }
            }
            Action::SheetLocal(local) => {
                if let Some(Sheet::Endpoint(f)) = &mut self.sheet {
                    f.local = local;
                }
            }
            Action::SheetEndpoint(name) => {
                if let Some(Sheet::Model(f)) = &mut self.sheet {
                    f.endpoint = Some(name);
                }
            }
            Action::ToggleOtherRoutines => self.show_other_routines = !self.show_other_routines,
            Action::ActivityDays(n) => self.activity_days = n,
            Action::SheetRole(role) => match &mut self.sheet {
                Some(Sheet::NewFlight(f)) => f.role = role,
                Some(Sheet::Schedule(f)) => f.role = role,
                Some(Sheet::Packet(f)) => f.to_role = role,
                _ => {}
            },
            Action::SheetPlace(place) => match &mut self.sheet {
                Some(Sheet::NewFlight(f)) => f.place = place,
                Some(Sheet::Schedule(f)) => f.place = place,
                Some(Sheet::Launch(f)) if place != Place::Worktree => f.place = place,
                _ => {}
            },
            Action::OpenPath { path } => fx.push(Effect::Open(path)),
            Action::SheetTier(tier) => match &mut self.sheet {
                Some(Sheet::NewFlight(f)) => f.tier = tier,
                Some(Sheet::Schedule(f)) => f.tier = tier,
                _ => {}
            },
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
        // Routing saves one message per tier.
        if let Sheet::Routing(f) = sheet {
            let msgs: Vec<ClientMsg> = f
                .tiers()
                .into_iter()
                .map(|(tier, engines)| ClientMsg::SetRouting { tier, engines })
                .collect();
            if self.live() {
                fx.extend(msgs.into_iter().map(Effect::Send));
                self.sheet = None;
                self.focus = None;
                self.toast("Routing saved", now_ms);
            } else {
                self.toast("towerd is not connected", now_ms);
            }
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
                        auto_mode: if auto && f.heuristic {
                            super::proto::AutoMode::Heuristic
                        } else {
                            super::proto::AutoMode::Off
                        },
                        launch,
                        tier: f.tier.clone(),
                        local_only: f.local_only,
                        hands_free: f.hands_free,
                    },
                    match (auto, auto && f.heuristic, launch) {
                        (true, true, true) => "Flight created; heuristics picked who takes it",
                        (true, true, false) => "Flight created; heuristics picked a role",
                        (true, false, true) => "Flight created; triage is picking who takes it",
                        (true, false, false) => "Flight created; triage is picking a role",
                        (false, _, true) => "Flight created, agent taking off",
                        (false, _, false) => "Flight created",
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
            Sheet::Schedule(f) => (
                ClientMsg::SaveSchedule { spec: f.spec() },
                if f.id.is_empty() {
                    "Schedule created"
                } else {
                    "Schedule saved"
                },
            ),
            Sheet::Endpoint(f) => (
                ClientMsg::SaveEndpoint {
                    spec: f.spec(),
                    old_name: f.old_name.clone(),
                },
                if f.old_name.is_none() {
                    "Model server added; probing it…"
                } else {
                    "Model server saved"
                },
            ),
            Sheet::Routing(_) => return,
            Sheet::Role(f) => (
                ClientMsg::SaveRole {
                    spec: f.spec(),
                    old_id: f.old_id.clone(),
                },
                if f.old_id.is_none() {
                    "Role saved"
                } else {
                    "Role updated"
                },
            ),
            Sheet::LocalOnly(f) => (
                ClientMsg::SetLocalOnlyPaths { paths: f.lines() },
                "Local-only folders saved",
            ),
            Sheet::Connect(on) => (
                ClientMsg::ConnectAgents { on: *on },
                if *on {
                    "Connecting your CLIs…"
                } else {
                    "Disconnecting your CLIs…"
                },
            ),
            Sheet::Model(f) => (
                ClientMsg::SaveModel {
                    spec: f.spec(),
                    old_id: f.old_id.clone(),
                },
                if f.old_id.is_none() {
                    "Model added"
                } else {
                    "Model saved"
                },
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
            (_, View::Team) => ClientMsg::Post {
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

    /// A button press, without needing its rectangle.
    fn act(m: &mut Model, action: Action) -> Vec<Effect> {
        let mut fx = Vec::new();
        m.activate(Hit::Button(action), 0, &mut fx);
        fx
    }

    #[test]
    fn click_needs_press_and_release_on_the_same_target() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 0.0, 10.0, 10.0);
        let mut m = model_with_hits(vec![
            (a, Hit::Nav(View::Setup)),
            (b, Hit::Nav(View::History)),
        ]);
        m.apply(Input::Down { x: 5.0, y: 5.0 }, 0);
        m.apply(Input::Up { x: 25.0, y: 5.0 }, 0);
        assert_eq!(m.view, View::Board, "drag off a target cancels");
        click(&mut m, 25.0, 5.0);
        assert_eq!(m.view, View::History);
    }

    #[test]
    fn three_views_digits_beyond_are_ignored() {
        let mut m = model_with_hits(vec![]);
        for (key, view) in [('1', View::Board), ('2', View::Setup), ('3', View::History)].iter() {
            m.apply(Input::Key(Key::Char(*key)), 0);
            assert_eq!(m.view, *view);
        }
        for key in ['4', '7', '9', '0'].iter() {
            let fx = m.apply(Input::Key(Key::Char(*key)), 0);
            assert_eq!(m.view, View::History, "{} does nothing", key);
            assert!(fx.is_empty(), "{} asks for no repaint", key);
        }
        m.apply(Input::Key(Key::Tab), 0);
        assert_eq!(m.view, View::Board, "Tab wraps after the last view");
        m.apply(Input::Key(Key::BackTab), 0);
        assert_eq!(m.view, View::History);
        // With the frequencies extra on, 4 is Team and Tab reaches it.
        m.snap.extras.frequencies = true;
        m.apply(Input::Key(Key::Char('4')), 0);
        assert_eq!(m.view, View::Team);
        m.apply(Input::Key(Key::Tab), 0);
        assert_eq!(m.view, View::Board);
    }

    #[test]
    fn team_view_falls_back_when_frequencies_turn_off() {
        let mut m = model_with_hits(vec![]);
        m.snap.extras.frequencies = true;
        m.apply(Input::Key(Key::Char('4')), 0);
        assert_eq!(m.view, View::Team);
        let mut snap = m.snap.clone();
        snap.extras.frequencies = false;
        m.apply_server(ServerMsg::Snapshot { snapshot: snap }, 0);
        assert_eq!(m.view, View::Board);
    }

    #[test]
    fn accept_terms_sends_and_not_now_only_defers() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        let fx = act(
            &mut m,
            Action::DeferTerms {
                id: "claude-agent-sdk".into(),
                defer: true,
            },
        );
        assert!(sent(&fx).is_empty(), "Not now tells the daemon nothing");
        assert_eq!(m.terms_deferred, vec!["claude-agent-sdk".to_string()]);
        act(
            &mut m,
            Action::DeferTerms {
                id: "claude-agent-sdk".into(),
                defer: false,
            },
        );
        assert!(m.terms_deferred.is_empty());
        let fx = act(&mut m, Action::AcceptTerms("claude-agent-sdk".into()));
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::AcceptTerms {
                note_id: "claude-agent-sdk".into()
            }]
        );
    }

    #[test]
    fn toggle_extra_sends_the_whole_set_and_needs_a_live_daemon() {
        let mut m = model_with_hits(vec![]);
        let fx = act(&mut m, Action::ToggleExtra(Extra::Frequencies));
        assert!(sent(&fx).is_empty());
        assert!(!m.snap.extras.frequencies, "offline: nothing changes");
        live(&mut m);
        m.snap.extras.scouts = true;
        let fx = act(&mut m, Action::ToggleExtra(Extra::Frequencies));
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::SetExtras {
                extras: super::super::proto::Extras {
                    scouts: true,
                    frequencies: true,
                    ..Default::default()
                }
            }]
        );
        assert!(m.snap.extras.frequencies, "shown at once");
        m.view = View::Team;
        act(&mut m, Action::ToggleExtra(Extra::Frequencies));
        assert_eq!(m.view, View::Board, "Team goes with its extra");
    }

    #[test]
    fn refresh_rereads_machine_jobs_only_when_shown() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.view = View::Setup;
        let fx = m.apply(Input::Key(Key::Char('r')), 0);
        assert_eq!(sent(&fx), vec![ClientMsg::GetSnapshot]);
        m.snap.extras.machine_routines = true;
        let fx = m.apply(Input::Key(Key::Char('r')), 0);
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::RefreshRoutines, ClientMsg::GetSnapshot]
        );
        m.view = View::Board;
        let fx = m.apply(Input::Key(Key::Char('r')), 0);
        assert_eq!(sent(&fx), vec![ClientMsg::GetSnapshot]);
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
    fn routing_saves_every_tier_and_policy_chips_send_at_once() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        let mut fx = vec![];
        m.action(Action::EditRouting, 0, &mut fx);
        assert!(matches!(m.sheet, Some(Sheet::Routing(_))));
        m.action(Action::Submit { launch: false }, 0, &mut fx);
        let tiers: Vec<String> = sent(&fx)
            .into_iter()
            .filter_map(|msg| match msg {
                ClientMsg::SetRouting { tier, .. } => Some(tier),
                _ => None,
            })
            .collect();
        assert_eq!(tiers, vec!["low", "medium", "high"]);
        assert!(m.sheet.is_none());
        let mut fx = vec![];
        m.action(
            Action::SetPolicyLevel {
                action: "land".into(),
                level: "auto".into(),
            },
            0,
            &mut fx,
        );
        assert_eq!(
            sent(&fx),
            vec![ClientMsg::SetPolicyLevel {
                action: "land".into(),
                level: Some("auto".into())
            }]
        );
    }

    #[test]
    fn sensitive_flights_and_schedules_are_sent_as_local_only() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.apply(Input::Key(Key::Char('n')), 0);
        type_text(&mut m, "Export payroll");
        let mut fx = vec![];
        m.action(Action::SheetFlag(SheetFlag::LocalOnly), 0, &mut fx);
        m.action(Action::Submit { launch: false }, 0, &mut fx);
        match sent(&fx).as_slice() {
            [ClientMsg::CreateFlight { local_only, .. }] => assert!(*local_only),
            other => panic!("{:?}", other),
        }
        m.open_sheet(Sheet::schedule(None, None));
        m.action(Action::SheetFlag(SheetFlag::LocalOnly), 0, &mut fx);
        match &m.sheet {
            Some(Sheet::Schedule(f)) => assert!(f.spec().local_only),
            _ => panic!("schedule sheet"),
        }
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
        assert_eq!(m.view, View::Board);
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
                ..
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
        // A triage role that cannot run (not installed, terms pending) is
        // no Auto default either: the flight would only sit in Holding.
        m.snap.setup.triage_usable = false;
        m.apply(Input::Key(Key::Char('n')), 0);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert_eq!(f.role, None),
            _ => panic!("no sheet"),
        }
        m.sheet = None;
        m.focus = None;
        // Without a triage role there is no Auto default.
        m.snap.setup.triage_usable = true;
        m.snap.triage_role.clear();
        m.apply(Input::Key(Key::Char('n')), 0);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert_eq!(f.role, None),
            _ => panic!("no sheet"),
        }
        m.sheet = None;
        m.focus = None;
        // ...unless heuristics have a role to fall back to.
        m.snap.setup.auto_heuristic_role = "auto".into();
        assert!(m.auto_available());
        m.apply(Input::Key(Key::Char('n')), 0);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert_eq!(f.role.as_deref(), Some(AUTO_ROLE)),
            _ => panic!("no sheet"),
        }
    }

    #[test]
    fn the_hands_free_toggle_works_on_new_flight_and_schedule_sheets() {
        let mut m = model_with_hits(vec![]);
        let mut fx = Vec::new();
        m.sheet = Some(Sheet::new_flight(None, Place::NewFolder));
        m.action(Action::SheetFlag(SheetFlag::HandsFree), 0, &mut fx);
        assert!(matches!(&m.sheet, Some(Sheet::NewFlight(f)) if f.hands_free));
        m.sheet = Some(Sheet::schedule(None, None));
        m.action(Action::SheetFlag(SheetFlag::HandsFree), 0, &mut fx);
        assert!(matches!(&m.sheet, Some(Sheet::Schedule(f)) if f.hands_free));
        m.action(Action::SheetFlag(SheetFlag::HandsFree), 0, &mut fx);
        assert!(matches!(&m.sheet, Some(Sheet::Schedule(f)) if !f.hands_free));
    }

    #[test]
    fn heuristics_only_is_sent_as_its_own_auto_mode() {
        let mut m = model_with_hits(vec![]);
        live(&mut m);
        m.snap.setup.auto_heuristic_role = "auto".into();
        m.apply(Input::Key(Key::Char('n')), 0);
        let mut fx = Vec::new();
        m.action(Action::SheetFlag(SheetFlag::Heuristic), 0, &mut fx);
        match &m.sheet {
            Some(Sheet::NewFlight(f)) => assert!(f.heuristic),
            _ => panic!("no sheet"),
        }
        type_text(&mut m, "Fix the flaky test");
        let fx = m.apply(Input::Key(Key::Enter { ctrl: false }), 0);
        match sent(&fx).as_slice() {
            [ClientMsg::CreateFlight {
                auto_assign,
                auto_mode,
                ..
            }] => {
                assert!(*auto_assign);
                assert_eq!(*auto_mode, super::super::proto::AutoMode::Heuristic);
            }
            other => panic!("{other:?}"),
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
