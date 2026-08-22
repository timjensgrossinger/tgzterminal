#![allow(clippy::range_plus_one)]
use super::renderstate::*;
use super::utilsprites::RenderMetrics;
use crate::agent_herd::sessions::AgentSession;
use crate::agent_herd::AgentKey;
use crate::colorease::ColorEase;
use crate::frontend::{front_end, try_front_end};
use crate::inputmap::InputMap;
use crate::overlay::{
    confirm_close_pane, confirm_close_tab, confirm_close_window, confirm_quit_program, launcher,
    start_overlay, start_overlay_pane, CopyModeParams, CopyOverlay, LauncherArgs, LauncherFlags,
    QuickSelectOverlay,
};
use crate::resize_increment_calculator::ResizeIncrementCalculator;
use crate::scripting::guiwin::GuiWin;
use crate::scrollbar::*;
use crate::selection::Selection;
use crate::shapecache::*;
use crate::tabbar::{TabBarItem, TabBarState};
use crate::termwindow::background::{
    load_background_image, reload_background_image, LoadedBackgroundLayer,
};
use crate::termwindow::keyevent::{KeyTableArgs, KeyTableState};
use crate::termwindow::modal::Modal;
use crate::termwindow::render::paint::AllowImage;
use crate::termwindow::render::sidebar::AgentDetectionCacheEntry;
use crate::termwindow::render::{
    CachedLineState, LineQuadCacheKey, LineQuadCacheValue, LineToEleShapeCacheKey,
    LineToElementShapeItem,
};
use crate::termwindow::webgpu::WebGpuState;
use ::wezterm_term::input::{ClickPosition, MouseButton as TMB};
use ::window::*;
use anyhow::{anyhow, ensure, Context};
use config::keyassignment::{
    Confirmation, KeyAssignment, LauncherActionArgs, PaneDirection, Pattern, PromptInputLine,
    QuickSelectArguments, RotationDirection, SpawnCommand, SplitSize,
};
use config::window::WindowLevel;
use config::{
    configuration, AgentAdapterConfig, AgentLaunchTarget, AudibleBell, ConfigHandle, Dimension,
    DimensionContext, FrontEndSelection, GeometryOrigin, GuiPosition, TermConfig,
    WindowCloseConfirmation,
};
use lfucache::*;
use mlua::{FromLua, LuaSerdeExt, UserData, UserDataFields};
use mux::pane::{
    CachePolicy, CloseReason, Pane, PaneId, Pattern as MuxPattern, PerformAssignmentResult,
};
use mux::renderable::RenderableDimensions;
use mux::tab::{
    PositionedPane, PositionedSplit, SplitDirection, SplitRequest, SplitSize as MuxSplitSize, Tab,
    TabId,
};
use mux::window::WindowId as MuxWindowId;
use mux::{Mux, MuxNotification};
use mux_lua::MuxPane;
use percent_encoding::percent_decode_str;
use smol::channel::Sender;
use smol::Timer;
use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet, LinkedList};
use std::ops::Add;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::SequenceNo;
use wezterm_dynamic::Value;
use wezterm_font::FontConfiguration;
use wezterm_term::color::ColorPalette;
use wezterm_term::input::LastMouseClick;

#[derive(Clone, Debug)]
struct RemoteFileBrowserContext {
    destination: String,
    port: Option<u16>,
    path: String,
}

/// One pass of "is this pane on another machine?" evidence.
///
/// See `TermWindow::pane_remote_signals`, which is the only constructor. The
/// individual flags are kept alongside the verdict because
/// `file_browser_remote_context` needs them to decide *which* remote it is,
/// and re-deriving them would repeat an expensive process-info fetch.
struct PaneRemoteSignals {
    working_dir_url: Option<url::Url>,
    working_dir_path: Option<String>,
    /// Foreground process argv, when it could be read. Only the argv is kept:
    /// it is the sole part of the process info any caller needs, and holding it
    /// avoids taking a `procinfo` dependency here.
    fg_argv: Option<Vec<String>>,
    name_is_ssh: bool,
    argv_is_ssh: bool,
    ssh_scheme: bool,
    remote_ssh_domain: bool,
    osc7_remote_host: bool,
}

impl PaneRemoteSignals {
    fn looks_remote(&self) -> bool {
        self.ssh_scheme
            || self.name_is_ssh
            || self.argv_is_ssh
            || self.remote_ssh_domain
            || self.osc7_remote_host
    }
}
use wezterm_term::{Alert, Progress, StableRowIndex, TerminalConfiguration, TerminalSize};

mod agent_launch;
pub mod background;
pub mod box_model;
pub mod charselect;
pub mod clipboard;
pub mod composer;
pub mod keyevent;
pub mod modal;
mod mouseevent;
pub mod palette;
pub mod paneselect;
mod prevcursor;
pub mod render;
pub mod resize;
mod selection;
pub mod spawn;
pub mod tgz_last_session;
pub mod tgz_ui_state;
pub mod webgpu;
pub mod wsl_paths;
use crate::spawn::SpawnWhere;
use prevcursor::PrevCursorPos;

const ATLAS_SIZE: usize = 128;

lazy_static::lazy_static! {
    static ref WINDOW_CLASS: Mutex<String> = Mutex::new(wezterm_gui_subcommands::DEFAULT_WINDOW_CLASS.to_owned());
    static ref POSITION: Mutex<Option<GuiPosition>> = Mutex::new(None);
}

pub const ICON_DATA: &'static [u8] = include_bytes!("../../../assets/icon/terminal.png");

pub fn set_window_position(pos: GuiPosition) {
    POSITION.lock().unwrap().replace(pos);
}

pub fn set_window_class(cls: &str) {
    *WINDOW_CLASS.lock().unwrap() = cls.to_owned();
}

/// Path handed to the worktree script as `$TGZTERMINAL_BIN`.
///
/// The CLI binary is `tgzterminal[.exe]` (see `wezterm/Cargo.toml`'s `[[bin]]
/// name`). The previous code probed for an extension-less name, so on Windows it
/// never matched and silently handed the script `wezterm-gui.exe` instead — a
/// GUI binary where a CLI was expected. Falling back to the bare name is the
/// honest alternative: it works when the executable's directory is on PATH and
/// fails visibly when it is not.
fn cli_bin_for_script(exe: Option<&Path>, windows: bool, exists: &dyn Fn(&Path) -> bool) -> String {
    const CLI: &str = "tgzterminal";
    let name = if windows { "tgzterminal.exe" } else { CLI };
    match exe
        .and_then(|exe| exe.parent())
        .map(|dir| dir.join(name))
        .filter(|candidate| exists(candidate))
    {
        Some(path) => shell_path(&path, windows),
        None => CLI.to_string(),
    }
}

/// Render a path for the POSIX shell script the worktree picker runs.
///
/// That script goes through msys/git-bash on Windows, where a backslash is an
/// escape character, so a native path has to be emitted with forward slashes.
fn shell_path(path: &Path, windows: bool) -> String {
    let path = path.to_string_lossy().to_string();
    if windows {
        path.replace('\\', "/")
    } else {
        path
    }
}

pub fn get_window_class() -> String {
    WINDOW_CLASS.lock().unwrap().clone()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseCapture {
    UI,
    TerminalPane(PaneId),
}

/// Type used together with Window::notify to do something in the
/// context of the window-specific event loop
pub enum TermWindowNotif {
    InvalidateShapeCache,
    PerformAssignment {
        pane_id: PaneId,
        assignment: KeyAssignment,
        tx: Option<Sender<anyhow::Result<()>>>,
    },
    SetLeftStatus(String),
    SetRightStatus(String),
    GetDimensions(Sender<(Dimensions, WindowState)>),
    GetSelectionForPane {
        pane_id: PaneId,
        tx: Sender<String>,
    },
    GetEffectiveConfig(Sender<ConfigHandle>),
    FinishWindowEvent {
        name: String,
        again: bool,
    },
    GetConfigOverrides(Sender<wezterm_dynamic::Value>),
    SetConfigOverrides(wezterm_dynamic::Value),
    CancelOverlayForPane(PaneId),
    CancelOverlayForTab {
        tab_id: TabId,
        pane_id: Option<PaneId>,
    },
    MuxNotification(MuxNotification),
    EmitStatusUpdate,
    Apply(Box<dyn FnOnce(&mut TermWindow) + Send + Sync>),
    SwitchToMuxWindow(MuxWindowId),
    SetInnerSize {
        width: usize,
        height: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentToolbeltAction {
    Interrupt,
    CopyMenu,
    Compose,
    /// Toggle the persistent docked input strip for this (agent) pane.
    DockInput,
    Attach,
    Resume,
    OpenLogs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCopyAction {
    Conversation,
    Markdown,
    LastAgentMessage,
    Summary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UIItemType {
    TabBar(TabBarItem),
    CloseTab(usize),
    /// Sidebar close button — separate from `CloseTab` so the dispatcher can
    /// tell the vertical sidebar apart from the horizontal top tab bar and
    /// emit surface-appropriate menu labels (Above/Below vs Left/Right).
    SidebarCloseTab(usize),
    /// Footer chip in the collapsed sidebar rail showing the waiting-agent
    /// count; clicking it jumps to the oldest waiting pane.
    SidebarWaitingCounter,
    /// A row in the right-click close-tab submenu.
    CloseTabMenuItem {
        source: CloseTabSource,
        action: CloseTabMenuAction,
    },
    SidebarTab {
        tab_idx: usize,
        active: bool,
    },
    /// Chevron on a split tab's row that shows or hides its pane children.
    SidebarTabExpand {
        tab_idx: usize,
    },
    /// An indented pane row beneath an expanded tab.
    SidebarPaneRow {
        pane_id: PaneId,
    },
    /// Close button on a pane row.
    SidebarPaneClose {
        pane_id: PaneId,
    },
    SidebarTabList,
    SidebarScrollTrack,
    SidebarScrollThumb,
    SidebarResize {
        start_width: usize,
    },
    SidebarSearch,
    SidebarAutoHideToggle,
    SidebarWorktreeButton,
    /// Sidebar button that starts a fresh agent session.
    SidebarAgentLaunchButton,
    /// A single agent row in the launch dropdown.
    SidebarAgentMenuItem {
        adapter_id: String,
    },
    /// The sticky project-root toggle row in the launch dropdown.
    SidebarAgentMenuProjectRootToggle,
    /// The "Agent overview" row in the launch dropdown: opens the herd
    /// overview in the active pane.
    SidebarAgentMenuHerd,
    /// An indented target row under an expanded agent in the launch
    /// dropdown: split / fullscreen (zoomed) / new tab, for that one launch.
    SidebarAgentMenuTarget {
        adapter_id: String,
        target: AgentLaunchTarget,
    },
    /// The "Resume session" row in the launch dropdown: expands into the
    /// recently-used sessions found on disk.
    SidebarAgentMenuResume,
    /// One past session under an expanded "Resume session" row. Carries an
    /// index into the scanned session list rather than the session itself, so
    /// hit-test items stay small.
    SidebarAgentMenuResumeSession {
        index: usize,
    },
    /// The "Reopen last window" button: restores the previous run's agent
    /// sessions into new tabs of this window.
    SidebarAgentMenuRestoreLastWindow,
    /// Chevron beside the sidebar new-tab button.
    SidebarNewTabMenuButton,
    /// A shell/domain row in the new-tab dropdown.
    SidebarNewTabMenuItem {
        index: usize,
    },
    /// Sidebar button that opens the SSH quick-launch dropdown.
    SidebarSshLaunchButton,
    /// A row in the SSH quick-launch dropdown.
    SidebarSshMenuItem {
        domain_name: String,
    },
    AgentToolbeltButton {
        pane_id: PaneId,
        action: AgentToolbeltAction,
    },
    AgentCopyMenuItem {
        pane_id: PaneId,
        action: AgentCopyAction,
    },
    AboveScrollThumb,
    ScrollThumb,
    BelowScrollThumb,
    Split(PositionedSplit),
    /// Agent section header in the sidebar: toggle expand/collapse.
    SidebarAgentSectionHeader,
    /// A single agent row in the sidebar agent section: focuses the agent.
    SidebarAgentRow {
        key: AgentKey,
    },
    /// The per-row chevron: expands/collapses that row's detail. Separate from
    /// the row itself so clicking the row can do the useful thing instead.
    SidebarAgentRowChevron {
        key: AgentKey,
    },
    /// A labelled action button inside an expanded agent's detail.
    SidebarAgentAction {
        key: AgentKey,
        action: AgentRowAction,
    },
}

/// What a button in an expanded agent row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRowAction {
    /// Bring the agent's pane on screen.
    Focus,
    /// Re-launch a detached session through its adapter's resume command.
    Resume,
    /// Adapter-defined attach (e.g. reconnect to a remote session).
    Attach,
    /// Open the agent's log directory.
    Logs,
    /// Interrupt the agent with Ctrl-C.
    Stop,
    /// Open a full-screen activity log overlay for this agent.
    Log,
    /// Copy the agent's session id to the clipboard.
    CopyId,
    /// Reveal the agent's transcript/log directory in the file manager.
    Transcript,
}

impl AgentRowAction {
    /// Button label. Short: the row is as wide as the sidebar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::Resume => "Resume",
            Self::Attach => "Attach",
            Self::Logs => "Logs",
            Self::Stop => "Stop",
            Self::Log => "Log",
            Self::CopyId => "Copy Id",
            Self::Transcript => "Transcript",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UIItem {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub item_type: UIItemType,
}

/// Which surface emitted the close-tab right-click: controls the submenu's
/// labels and which direction the dropdown opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseTabSource {
    /// Horizontal top tab bar: labels say "to the Left" / "to the Right".
    TabBar,
    /// Vertical sidebar: labels say "Above" / "Below".
    Sidebar,
}

/// One row in the right-click close-tab submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseTabMenuAction {
    /// Close every tab above (sidebar) / to the left (tab bar) of the anchor.
    CloseAbove,
    /// Close every tab below (sidebar) / to the right (tab bar) of the anchor.
    CloseBelow,
    /// Close every tab except the anchor.
    CloseAllOther,
}

/// Anchor for the right-click close-tab submenu. Rendered as an anchored
/// dropdown mirroring the `agent_launch_menu` / `new_tab_menu` pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseTabMenuState {
    pub x: usize,
    pub y: usize,
    /// Which surface opened the menu — also carried on each row's item_type
    /// so mouse-item handlers know which surface to act on.
    pub source: CloseTabSource,
    /// The tab whose × was right-clicked; preserved across all batch actions.
    pub anchor_tab_idx: usize,
}

impl UIItem {
    pub fn hit_test(&self, x: isize, y: isize) -> bool {
        x >= self.x as isize
            && x <= (self.x + self.width) as isize
            && y >= self.y as isize
            && y <= (self.y + self.height) as isize
    }
}

#[derive(Clone, Debug, Default)]
pub struct SidebarSearchState {
    pub query: String,
}

#[derive(Clone, Debug)]
pub struct AgentCopyMenuState {
    pub pane_id: PaneId,
    pub x: usize,
    pub y: usize,
}

/// Which row of the agent launch dropdown has its submenu open.
///
/// Only ever one at a time: the dropdown grows upward from a button near the
/// bottom of the sidebar, so several expansions at once would quickly run out of
/// screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpandedMenuRow {
    /// An installed agent, showing its split / fullscreen / new tab targets.
    Agent(String),
    /// The "Resume session" row, showing past sessions found on disk.
    ResumeSessions,
}

/// Anchor for the sidebar agent launch dropdown. Unlike the copy menu this is
/// not tied to a pane: it lists installed agents, not a detected session.
#[derive(Clone, Debug)]
pub struct AgentLaunchMenuState {
    pub x: usize,
    pub y: usize,
    /// Row whose submenu is currently expanded, if any. The new-tab dropdown
    /// reuses this struct and always leaves it `None`.
    pub expanded: Option<ExpandedMenuRow>,
}

/// Anchor for the sidebar SSH quick-launch dropdown. Mirrors
/// `AgentLaunchMenuState` but lists pre-registered `ssh_domains` (and any
/// mosh/et sidecars) rather than installed agents. No expandable rows — every
/// entry is a single click that spawns in a new tab.
#[derive(Clone, Debug)]
pub struct SshLaunchMenuState {
    pub x: usize,
    pub y: usize,
}

/// What a new-tab dropdown row spawns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewTabTarget {
    /// A registered domain, spawned with its own default program.
    Domain(String),
    /// An explicit program discovered on this machine, or a `launch_menu`
    /// entry, spawned in the active pane's domain.
    Program(Vec<String>),
}

/// One row offered by the sidebar new-tab dropdown.
#[derive(Clone, Debug)]
pub struct NewTabMenuEntry {
    pub label: String,
    pub target: NewTabTarget,
    /// Rows are grouped shells / domains / launch_menu, with a divider
    /// between groups.
    pub group: u8,
}

/// One installed agent offered by the sidebar launcher.
#[derive(Clone, Debug)]
pub struct AgentLauncherEntry {
    pub adapter_id: String,
    pub label: String,
    pub short_label: String,
    pub color: ::window::color::LinearRgba,
    /// Adapter-level domain override; see `agent_ui.launcher.domain`.
    pub launch_domain: Option<String>,
    /// Fully resolved argv. Contains no `{...}` placeholders: the launcher
    /// passes the working directory out of band via `SpawnCommand::cwd`.
    pub argv: Vec<String>,
}

/// One pre-registered SSH connection offered by the sidebar SSH quick-launch
/// dropdown. Built from `config.ssh_domains`; see `ssh_quick_launch_entries`.
///
/// For `WezTerm`/`Ssh` transports `argv` is empty and the spawn goes through
/// `SpawnTabDomain::DomainName(domain_name)`. For `Mosh`/`Et` `argv` is the
/// fully resolved command (absolute program path + `user@host` + extras) and
/// the spawn runs in the local domain: those transports own their own
/// reconnect state and bypass the wezterm mux.
#[derive(Clone, Debug)]
pub struct SshQuickLaunchEntry {
    /// The `SshDomain::name` to pass to `SpawnTabDomain::DomainName` for
    /// mux-driven transports. Carried on the row's `UIItemType` so the mouse
    /// handler can look the entry up by name.
    pub domain_name: String,
    /// Display label — the bare host (with `user@` if set) for mosh/et,
    /// otherwise the domain name with any `SSH:`/`SSHMUX:` prefix stripped.
    pub label: String,
    /// One-word transport tag rendered as a trailing badge so several entries
    /// against the same host are distinguishable.
    pub transport: config::SshTransport,
    /// Resolved argv for `Mosh`/`Et` (absolute program path first); empty for
    /// `WezTerm`/`Ssh`, which spawn through the mux domain instead.
    pub argv: Vec<String>,
}

/// State for the agent section embedded in the sidebar.
///
/// Lives on TermWindow and is updated by a background refresh thread.
/// The section renders as part of the sidebar paint pass.
#[derive(Clone, Debug)]
pub struct AgentHerdState {
    /// Flattened agent list in display order.
    pub agents: Vec<crate::agent_herd::HerdAgent>,
    /// Which agent row is expanded inline, by identity rather than position:
    /// the list is rebuilt and re-sorted every paint.
    pub expanded: Option<crate::agent_herd::AgentKey>,
    /// Whether the section is collapsed (hidden).
    pub collapsed: bool,
    /// First agent row scrolled into view, for lists longer than the section.
    pub scroll_offset: usize,
    /// How many agent rows the current section layout fits (for scroll clamp).
    pub visible_rows: usize,
    /// Keyboard navigation: the row with the cursor, if nav mode is active.
    pub selection: Option<crate::agent_herd::AgentKey>,
    /// Whether arrow-key navigation of the section is capturing keys.
    pub nav_active: bool,
    /// Right-clicked row whose action menu is open, if any.
    pub context_menu: Option<crate::agent_herd::AgentKey>,
    /// Whether the list is scoped to the current project or shows all projects.
    pub view: crate::agent_herd::HerdView,
    /// Short-lived feedback for control actions. OS notifications may be denied.
    pub feedback: Option<(String, std::time::Instant)>,
}

impl Default for AgentHerdState {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            expanded: None,
            collapsed: false,
            scroll_offset: 0,
            visible_rows: 0,
            selection: None,
            nav_active: false,
            context_menu: None,
            view: crate::agent_herd::HerdView::CurrentProject,
            feedback: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct SemanticZoneCache {
    seqno: SequenceNo,
    zones: Vec<StableRowIndex>,
}

pub struct OverlayState {
    pub pane: Arc<dyn Pane>,
    pub key_table_state: KeyTableState,
}

#[derive(Default)]
pub struct PaneState {
    /// If is_some(), the top row of the visible screen.
    /// Otherwise, the viewport is at the bottom of the
    /// scrollback.
    viewport: Option<StableRowIndex>,
    selection: Selection,
    /// If is_some(), rather than display the actual tab
    /// contents, we're overlaying a little internal application
    /// tab.  We'll also route input to it.
    pub overlay: Option<OverlayState>,

    bell_start: Option<Instant>,
    pub mouse_terminal_coords: Option<(ClickPosition, StableRowIndex)>,
}

/// Data used when synchronously formatting pane and window titles
#[derive(Debug, Clone)]
pub struct TabInformation {
    pub tab_id: TabId,
    pub tab_index: usize,
    pub is_active: bool,
    pub is_last_active: bool,
    pub active_pane: Option<PaneInformation>,
    pub window_id: MuxWindowId,
    pub tab_title: String,
}

impl UserData for TabInformation {
    fn add_fields<'lua, F: UserDataFields<'lua, Self>>(fields: &mut F) {
        fields.add_field_method_get("tab_id", |_, this| Ok(this.tab_id));
        fields.add_field_method_get("tab_index", |_, this| Ok(this.tab_index));
        fields.add_field_method_get("is_active", |_, this| Ok(this.is_active));
        fields.add_field_method_get("is_last_active", |_, this| Ok(this.is_last_active));
        fields.add_field_method_get("active_pane", |_, this| {
            if let Some(pane) = &this.active_pane {
                Ok(Some(pane.clone()))
            } else {
                Ok(None)
            }
        });
        fields.add_field_method_get("panes", |_, this| {
            let mux = Mux::get();
            let mut panes = vec![];
            if let Some(tab) = mux.get_tab(this.tab_id) {
                panes = tab
                    .iter_panes()
                    .iter()
                    .map(TermWindow::pos_pane_to_pane_info)
                    .collect();
            }
            Ok(panes)
        });
        fields.add_field_method_get("window_id", |_, this| Ok(this.window_id));
        fields.add_field_method_get("tab_title", |_, this| Ok(this.tab_title.clone()));
        fields.add_field_method_get("window_title", |_, this| {
            let mux = Mux::get();
            let window = mux.get_window(this.window_id).ok_or_else(|| {
                mlua::Error::external(format!("window {} not found", this.window_id))
            })?;
            Ok(window.get_title().to_string())
        });
    }
}

/// Data used when synchronously formatting pane and window titles
#[derive(Debug, Clone)]
pub struct PaneInformation {
    pub pane_id: PaneId,
    pub pane_index: usize,
    pub is_active: bool,
    pub is_zoomed: bool,
    pub has_unseen_output: bool,
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
    pub title: String,
    pub user_vars: HashMap<String, String>,
    pub progress: Progress,
}

impl UserData for PaneInformation {
    fn add_fields<'lua, F: UserDataFields<'lua, Self>>(fields: &mut F) {
        fields.add_field_method_get("pane_id", |_, this| Ok(this.pane_id));
        fields.add_field_method_get("pane_index", |_, this| Ok(this.pane_index));
        fields.add_field_method_get("is_active", |_, this| Ok(this.is_active));
        fields.add_field_method_get("is_zoomed", |_, this| Ok(this.is_zoomed));
        fields.add_field_method_get("has_unseen_output", |_, this| Ok(this.has_unseen_output));
        fields.add_field_method_get("left", |_, this| Ok(this.left));
        fields.add_field_method_get("top", |_, this| Ok(this.top));
        fields.add_field_method_get("width", |_, this| Ok(this.width));
        fields.add_field_method_get("height", |_, this| Ok(this.height));
        fields.add_field_method_get("pixel_width", |_, this| Ok(this.pixel_width));
        fields.add_field_method_get("pixel_height", |_, this| Ok(this.pixel_height));
        fields.add_field_method_get("progress", |lua, this| lua.to_value(&this.progress));
        fields.add_field_method_get("title", |_, this| Ok(this.title.clone()));
        fields.add_field_method_get("user_vars", |_, this| Ok(this.user_vars.clone()));
        fields.add_field_method_get("foreground_process_name", |_, this| {
            let mut name = None;
            if let Some(mux) = Mux::try_get() {
                if let Some(pane) = mux.get_pane(this.pane_id) {
                    name = pane.get_foreground_process_name(CachePolicy::AllowStale);
                }
            }
            match name {
                Some(name) => Ok(name),
                None => Ok("".to_string()),
            }
        });
        fields.add_field_method_get("tty_name", |_, this| {
            let mut name = None;
            if let Some(mux) = Mux::try_get() {
                if let Some(pane) = mux.get_pane(this.pane_id) {
                    name = pane.tty_name();
                }
            }
            Ok(name)
        });
        fields.add_field_method_get("current_working_dir", |_, this| {
            if let Some(mux) = Mux::try_get() {
                if let Some(pane) = mux.get_pane(this.pane_id) {
                    return Ok(pane
                        .get_current_working_dir(CachePolicy::AllowStale)
                        .map(|url| url_funcs::Url { url }));
                }
            }
            Ok(None)
        });
        fields.add_field_method_get("domain_name", |_, this| {
            let mut name = None;
            if let Some(mux) = Mux::try_get() {
                if let Some(pane) = mux.get_pane(this.pane_id) {
                    let domain_id = pane.domain_id();
                    name = mux
                        .get_domain(domain_id)
                        .map(|dom| dom.domain_name().to_string());
                }
            }
            match name {
                Some(name) => Ok(name),
                None => Ok("".to_string()),
            }
        });
    }
}

#[derive(Default)]
pub struct TabState {
    /// If is_some(), rather than display the actual tab
    /// contents, we're overlaying a little internal application
    /// tab.  We'll also route input to it.
    pub overlay: Option<OverlayState>,
}

/// Manages the state/queue of lua based event handlers.
/// We don't want to queue more than 1 event at a time,
/// so we use this enum to allow for at most 1 executing
/// and 1 pending event.
#[derive(Copy, Clone, Debug)]
enum EventState {
    /// The event is not running
    None,
    /// The event is running
    InProgress,
    /// The event is running, and we have another one ready to
    /// run once it completes
    InProgressWithQueued(Option<PaneId>),
}

pub struct TermWindow {
    pub window: Option<Window>,
    pub config: ConfigHandle,
    pub config_overrides: wezterm_dynamic::Value,
    os_parameters: Option<parameters::Parameters>,
    /// When we most recently received keyboard focus
    pub focused: Option<Instant>,
    fonts: Rc<FontConfiguration>,
    /// Window dimensions and dpi
    pub dimensions: Dimensions,
    pub window_state: WindowState,
    pub resizes_pending: usize,
    is_repaint_pending: bool,
    pending_scale_changes: LinkedList<resize::ScaleChange>,
    /// Terminal dimensions
    terminal_size: TerminalSize,
    pub mux_window_id: MuxWindowId,
    pub mux_window_id_for_subscriptions: Arc<Mutex<MuxWindowId>>,
    /// `true` when the mux subscription must be unsubscribed from.
    /// This is done asynchronously to avoid races between mux events.
    mux_subscription_dead: Arc<AtomicBool>,
    pub render_metrics: RenderMetrics,
    render_state: Option<RenderState>,
    input_map: InputMap,
    /// If is_some, the LEADER modifier is active until the specified instant.
    leader_is_down: Option<std::time::Instant>,
    dead_key_status: DeadKeyStatus,
    key_table_state: KeyTableState,
    show_tab_bar: bool,
    show_scroll_bar: bool,
    tab_bar: TabBarState,
    sidebar_drag_width: Option<usize>,
    sidebar_auto_hide_open: bool,
    sidebar_auto_hide_close_after: Option<Instant>,
    sidebar_search: Option<SidebarSearchState>,
    agent_copy_menu: Option<AgentCopyMenuState>,
    agent_launch_menu: Option<AgentLaunchMenuState>,
    new_tab_menu: Option<AgentLaunchMenuState>,
    close_tab_menu: Option<CloseTabMenuState>,
    /// Sidebar SSH quick-launch dropdown anchor. `None` when closed.
    ssh_launch_menu: Option<SshLaunchMenuState>,
    /// Shells/domains offered by the new-tab dropdown. Probing for shells
    /// touches the filesystem, so this is rebuilt only when the config
    /// generation changes.
    new_tab_menu_cache: RefCell<Option<(usize, Arc<Vec<NewTabMenuEntry>>)>>,
    /// Sticky "launch agents at the project root" toggle. Seeded from
    /// `agent_ui.launcher.cwd` and then owned by the user's dropdown toggle,
    /// persisted via `tgz_ui_state`.
    agent_launcher_project_root: bool,
    agent_detection_cache: RefCell<HashMap<PaneId, AgentDetectionCacheEntry>>,
    /// Panes that are agent insight views. Membership is the identity check —
    /// these panes must never be badged as agents, split into, or picked as a
    /// target by anything that wants a shell.
    agent_herd_state: RefCell<AgentHerdState>,
    /// Vendor session scan result. Filesystem work runs off the GUI thread.
    agent_herd_session_cache: Option<(Instant, Arc<Vec<crate::agent_herd::vendor::VendorSession>>)>,
    agent_herd_scan_pending: bool,
    adapter_cache: RefCell<Option<(usize, Arc<Vec<(String, AgentAdapterConfig)>>)>>,
    /// Installed-agent launcher entries, rebuilt only when the config
    /// generation changes. Building probes `$PATH`, so this must never be
    /// recomputed per frame.
    launcher_cache: RefCell<Option<(usize, Arc<Vec<AgentLauncherEntry>>)>>,
    /// SSH quick-launch entries, rebuilt only when the config generation
    /// changes. Building probes `$PATH` for `mosh`/`et`, so this must never
    /// be recomputed per frame.
    ssh_launcher_cache: RefCell<Option<(usize, Arc<Vec<SshQuickLaunchEntry>>)>>,
    /// Past sessions offered by the launcher's "Resume session" submenu, with
    /// the instant they were scanned.
    ///
    /// Finding these means statting every transcript on disk and reading the
    /// head of the newest few, so the scan runs on a worker thread and lands
    /// here; the render path only ever reads this. Owned rather than a `RefCell`
    /// because it is written from the notification handler, not from paint.
    agent_session_cache: Option<(Instant, Arc<Vec<AgentSession>>)>,
    /// A scan is in flight. Keeps a held-open submenu from queueing a new scan
    /// per frame, and tells the renderer to show progress instead of "none".
    agent_session_scan_pending: bool,
    /// Agent sessions running in *this* window right now, in tab order.
    ///
    /// Recomputed from the herd join every paint, which is why it must stay a
    /// plain filter with no filesystem work. Also what the close handler
    /// persists, so it must be current rather than recomputed on the way out.
    agent_window_sessions: Vec<tgz_last_session::SnapshotSession>,
    /// What was last persisted for this window, and when. The list is the change
    /// detector (so an unchanged set writes nothing) and the instant is the rate
    /// limiter.
    agent_snapshot_written: Option<(Instant, Vec<tgz_last_session::SnapshotSession>)>,
    /// The set changed but the rate limit had not elapsed. A frame is scheduled
    /// so the write is not lost when the window then goes quiet.
    agent_snapshot_dirty: bool,
    /// Agent sessions from the last window of the previous run, loaded once at
    /// window creation. `None` means there is nothing to offer, so the launcher's
    /// restore row stays hidden.
    last_window_sessions: Option<Arc<Vec<tgz_last_session::SnapshotSession>>>,
    sidebar_scroll_offset: usize,
    sidebar_drop_flash: Option<(usize, Instant)>,
    /// Tabs whose pane children are shown in the sidebar. Persisted via
    /// `tgz_ui_state`; entries for tabs that no longer exist are harmless and
    /// simply never match.
    sidebar_expanded_tabs: HashSet<usize>,
    fancy_tab_bar: Option<box_model::ComputedElement>,
    pub right_status: String,
    pub left_status: String,
    last_ui_item: Option<UIItem>,
    pressed_ui_item: Option<UIItemType>,
    /// Tracks whether the current mouse-down event is part of click-focus.
    /// If so, we ignore mouse events until released
    is_click_to_focus_window: bool,
    last_mouse_coords: (usize, i64),
    window_drag_position: Option<MouseEvent>,
    current_mouse_event: Option<MouseEvent>,
    prev_cursor: PrevCursorPos,
    last_scroll_info: RenderableDimensions,

    tab_state: RefCell<HashMap<TabId, TabState>>,
    pane_state: RefCell<HashMap<PaneId, PaneState>>,
    semantic_zones: HashMap<PaneId, SemanticZoneCache>,

    window_background: Vec<LoadedBackgroundLayer>,

    current_modifier_and_leds: (Modifiers, KeyboardLedStatus),
    current_mouse_buttons: Vec<MousePress>,
    current_mouse_capture: Option<MouseCapture>,

    opengl_info: Option<String>,

    /// Keeps track of double and triple clicks
    last_mouse_click: Option<LastMouseClick>,

    /// The URL over which we are currently hovering
    current_highlight: Option<Arc<Hyperlink>>,

    quad_generation: usize,
    shape_generation: usize,
    shape_cache: RefCell<LfuCache<ShapeCacheKey, anyhow::Result<Rc<Vec<ShapedInfo>>>>>,
    line_to_ele_shape_cache: RefCell<LfuCache<LineToEleShapeCacheKey, LineToElementShapeItem>>,

    line_state_cache: RefCell<LfuCacheU64<Arc<CachedLineState>>>,
    next_line_state_id: u64,

    line_quad_cache: RefCell<LfuCache<LineQuadCacheKey, LineQuadCacheValue>>,

    last_status_call: Instant,
    cursor_blink_state: RefCell<ColorEase>,
    blink_state: RefCell<ColorEase>,
    rapid_blink_state: RefCell<ColorEase>,

    palette: Option<ColorPalette>,

    ui_items: Vec<UIItem>,
    dragging: Option<(UIItem, MouseEvent)>,

    modal: RefCell<Option<Rc<dyn Modal>>>,

    /// Ring of previous rich-input composer submissions, oldest first.
    composer_history: RefCell<Vec<String>>,

    /// Persistent Warp-style docked input strip state (rich_input.docked).
    docked_input: crate::termwindow::composer::DockedInput,

    event_states: HashMap<String, EventState>,
    pub current_event: Option<Value>,
    has_animation: RefCell<Option<Instant>>,
    /// We use this to attempt to do something reasonable
    /// if we run out of texture space
    allow_images: AllowImage,
    scheduled_animation: RefCell<Option<Instant>>,

    created: Instant,

    pub last_frame_duration: Duration,
    last_fps_check_time: Instant,
    num_frames: usize,
    pub fps: f32,

    connection_name: String,

    gl: Option<Rc<glium::backend::Context>>,
    webgpu: Option<Rc<WebGpuState>>,
    config_subscription: Option<config::ConfigSubscription>,
}

impl TermWindow {
    fn load_os_parameters(&mut self) {
        if let Some(ref window) = self.window {
            self.os_parameters = match window.get_os_parameters(&self.config, self.window_state) {
                Ok(os_parameters) => os_parameters,
                Err(err) => {
                    log::warn!("Error while getting OS parameters: {:#}", err);
                    None
                }
            };
        }
    }

    fn close_requested(&mut self, window: &Window) {
        let mux = Mux::get();
        // Record the agents while the panes still exist. One line here covers
        // all three exit paths below, including the confirmation overlay, which
        // closes the window later from somewhere with no access to this state.
        self.persist_agent_window_sessions_on_close();
        match self.config.window_close_confirmation {
            WindowCloseConfirmation::NeverPrompt => {
                // Immediately kill the tabs and allow the window to close
                mux.kill_window(self.mux_window_id);
                window.close();
                front_end().forget_known_window(window);
            }
            WindowCloseConfirmation::AlwaysPrompt => {
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => {
                        mux.kill_window(self.mux_window_id);
                        window.close();
                        front_end().forget_known_window(window);
                        return;
                    }
                };

                let mux_window_id = self.mux_window_id;

                let can_close = mux
                    .get_window(mux_window_id)
                    .map_or(false, |w| w.can_close_without_prompting());
                if can_close {
                    mux.kill_window(self.mux_window_id);
                    window.close();
                    front_end().forget_known_window(window);
                    return;
                }
                let window = self.window.clone().unwrap();
                let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                    confirm_close_window(term, mux_window_id, window, tab_id)
                });
                self.assign_overlay(tab.tab_id(), overlay);
                promise::spawn::spawn(future).detach();

                // Don't close right now; let the close happen from
                // the confirmation overlay
            }
        }
    }

    fn focus_changed(&mut self, focused: bool, window: &Window) {
        log::trace!("Setting focus to {:?}", focused);
        self.focused = if focused { Some(Instant::now()) } else { None };
        self.quad_generation += 1;
        self.load_os_parameters();

        if self.focused.is_none() {
            self.last_mouse_click = None;
            self.current_mouse_buttons.clear();
            self.current_mouse_capture = None;
            self.is_click_to_focus_window = false;

            for state in self.pane_state.borrow_mut().values_mut() {
                state.mouse_terminal_coords.take();
            }
        }

        // Reset the cursor blink phase
        self.prev_cursor.bump();

        // force cursor to be repainted
        window.invalidate();

        if let Some(pane) = self.get_active_pane_or_overlay() {
            pane.focus_changed(focused);
        }

        // macOS dock badge: surface the waiting-agent count while the app is
        // unfocused. Other platforms ignore it. Only meaningful when enabled.
        if self.config.agent_ui.dock_badge {
            let count = if focused {
                0
            } else {
                self.waiting_queue().len()
            };
            crate::macos_dock_badge::set_waiting_count(count);
        }

        self.update_title();
        self.emit_window_event("window-focus-changed", None);
    }

    fn created(&mut self, ctx: RenderContext) -> anyhow::Result<()> {
        self.render_state = None;

        let render_info = ctx.renderer_info();
        self.opengl_info.replace(render_info.clone());

        match RenderState::new(ctx, &self.fonts, &self.render_metrics, ATLAS_SIZE) {
            Ok(render_state) => {
                log::debug!(
                    "OpenGL initialized! {} wezterm version: {}",
                    render_info,
                    config::wezterm_version(),
                );
                self.render_state.replace(render_state);
            }
            Err(err) => {
                log::error!("failed to create RenderState: {}", err);
            }
        }

        if self.render_state.is_none() {
            panic!("No OpenGL");
        }

        Ok(())
    }
}

impl TermWindow {
    pub async fn new_window(mux_window_id: MuxWindowId) -> anyhow::Result<()> {
        let config = configuration();
        // Apply any TGZTerminal persisted UI toggles (e.g. sidebar auto-hide)
        // as a seeded config override so the very first frame reflects them.
        let config_overrides = tgz_ui_state::seed_config_overrides();
        let config = if config_overrides == wezterm_dynamic::Value::Null {
            config
        } else {
            config::overridden_config(&config_overrides).unwrap_or(config)
        };
        let dpi = config.dpi.unwrap_or_else(|| ::window::default_dpi()) as usize;
        let fontconfig = Rc::new(FontConfiguration::new(Some(config.clone()), dpi)?);

        let mux = Mux::get();
        let size = match mux.get_active_tab_for_window(mux_window_id) {
            Some(tab) => tab.get_size(),
            None => {
                log::debug!("new_window has no tabs... yet?");
                Default::default()
            }
        };
        let physical_rows = size.rows as usize;
        let physical_cols = size.cols as usize;

        let render_metrics = RenderMetrics::new(&fontconfig)?;
        log::trace!("using render_metrics {:#?}", render_metrics);

        // Initially we have only a single tab, so take that into account
        // for the tab bar state.
        let show_tab_bar = config.enable_tab_bar && !config.hide_tab_bar_if_only_one_tab;
        let tab_bar_height = if show_tab_bar && !config.sidebar_enabled {
            Self::tab_bar_pixel_height_impl(&config, &fontconfig, &render_metrics)? as usize
        } else {
            0
        };
        // Same DPI-scaled rule the paint path uses. Reading the raw config
        // values here instead made the reserved width disagree with the drawn
        // width by the whole density factor.
        let sidebar_width = if show_tab_bar && config.sidebar_enabled {
            crate::termwindow::render::sidebar::sidebar_reserved_width_for_config(
                &config, dpi as f64,
            )
        } else {
            0
        };

        let terminal_size = TerminalSize {
            rows: physical_rows,
            cols: physical_cols,
            pixel_width: (render_metrics.cell_size.width as usize * physical_cols),
            pixel_height: (render_metrics.cell_size.height as usize * physical_rows),
            dpi: dpi as u32,
        };

        if terminal_size != size {
            // DPI is different from the default assumed DPI when the mux
            // created the pty. We need to inform the kernel of the revised
            // pixel geometry now
            log::trace!(
                "Initial geometry was {:?} but dpi-adjusted geometry \
                        is {:?}; update the kernel pixel geometry for the ptys!",
                size,
                terminal_size,
            );
            if let Some(window) = mux.get_window(mux_window_id) {
                for tab in window.iter() {
                    tab.resize(terminal_size);
                }
            };
        }

        let h_context = DimensionContext {
            dpi: dpi as f32,
            pixel_max: terminal_size.pixel_width as f32,
            pixel_cell: render_metrics.cell_size.width as f32,
        };
        let padding_left = config.window_padding.left.evaluate_as_pixels(h_context) as usize;
        let padding_right = resize::effective_right_padding(&config, h_context) as usize;
        let v_context = DimensionContext {
            dpi: dpi as f32,
            pixel_max: terminal_size.pixel_height as f32,
            pixel_cell: render_metrics.cell_size.height as f32,
        };
        let padding_top = config.window_padding.top.evaluate_as_pixels(v_context) as usize;
        let padding_bottom = config.window_padding.bottom.evaluate_as_pixels(v_context) as usize;

        let mut dimensions = Dimensions {
            pixel_width: (terminal_size.pixel_width + padding_left + padding_right) as usize,
            pixel_height: ((terminal_size.rows * render_metrics.cell_size.height as usize)
                + padding_top
                + padding_bottom) as usize
                + tab_bar_height,
            dpi,
        };
        dimensions.pixel_width += sidebar_width;

        let border = Self::get_os_border_impl(&None, &config, &dimensions, &render_metrics);

        dimensions.pixel_height += (border.top + border.bottom).get() as usize;
        dimensions.pixel_width += (border.left + border.right).get() as usize;

        let window_background = load_background_image(&config, &dimensions, &render_metrics);

        log::trace!(
            "TermWindow::new_window called with mux_window_id {} {:?} {:?}",
            mux_window_id,
            terminal_size,
            dimensions
        );

        let render_state = None;

        let connection_name = Connection::get().unwrap().name();

        let myself = Self {
            created: Instant::now(),
            connection_name,
            last_fps_check_time: Instant::now(),
            num_frames: 0,
            last_frame_duration: Duration::ZERO,
            fps: 0.,
            config_subscription: None,
            os_parameters: None,
            gl: None,
            webgpu: None,
            window: None,
            window_background,
            config: config.clone(),
            config_overrides,
            palette: None,
            focused: None,
            mux_window_id,
            mux_window_id_for_subscriptions: Arc::new(Mutex::new(mux_window_id)),
            mux_subscription_dead: Arc::new(AtomicBool::new(false)),
            fonts: Rc::clone(&fontconfig),
            render_metrics,
            dimensions,
            window_state: WindowState::default(),
            resizes_pending: 0,
            is_repaint_pending: false,
            pending_scale_changes: LinkedList::new(),
            terminal_size,
            render_state,
            input_map: InputMap::new(&config),
            leader_is_down: None,
            dead_key_status: DeadKeyStatus::None,
            show_tab_bar,
            show_scroll_bar: config.enable_scroll_bar,
            tab_bar: TabBarState::default(),
            sidebar_drag_width: None,
            sidebar_auto_hide_open: false,
            sidebar_auto_hide_close_after: None,
            sidebar_search: None,
            agent_copy_menu: None,
            agent_launch_menu: None,
            new_tab_menu: None,
            close_tab_menu: None,
            ssh_launch_menu: None,
            new_tab_menu_cache: RefCell::new(None),
            agent_launcher_project_root: tgz_ui_state::load_agent_launcher_project_root()
                .unwrap_or(config.agent_ui.launcher.cwd == config::AgentLauncherCwd::ProjectRoot),
            agent_detection_cache: RefCell::new(HashMap::new()),
            agent_herd_state: RefCell::new(AgentHerdState {
                collapsed: tgz_ui_state::load_agent_section_collapsed().unwrap_or(false),
                view: tgz_ui_state::load_agent_section_view()
                    .unwrap_or(crate::agent_herd::HerdView::CurrentProject),
                ..AgentHerdState::default()
            }),
            agent_herd_session_cache: None,
            agent_herd_scan_pending: false,
            adapter_cache: RefCell::new(None),
            launcher_cache: RefCell::new(None),
            ssh_launcher_cache: RefCell::new(None),
            agent_session_cache: None,
            agent_session_scan_pending: false,
            agent_window_sessions: Vec::new(),
            agent_snapshot_written: None,
            agent_snapshot_dirty: false,
            // Read once here, never from paint: the launcher's restore row only
            // ever consults this copy.
            last_window_sessions: (config.agent_ui.launcher.restore_last_window_sessions > 0)
                .then(tgz_last_session::load_last_window)
                .flatten()
                .map(Arc::new),
            composer_history: RefCell::new(Vec::new()),
            docked_input: crate::termwindow::composer::DockedInput::new(),
            sidebar_scroll_offset: 0,
            sidebar_drop_flash: None,
            sidebar_expanded_tabs: tgz_ui_state::load_sidebar_expanded_tabs().unwrap_or_default(),
            fancy_tab_bar: None,
            right_status: String::new(),
            left_status: String::new(),
            last_mouse_coords: (0, -1),
            pressed_ui_item: None,
            window_drag_position: None,
            current_mouse_event: None,
            current_modifier_and_leds: Default::default(),
            prev_cursor: PrevCursorPos::new(),
            last_scroll_info: RenderableDimensions::default(),
            tab_state: RefCell::new(HashMap::new()),
            pane_state: RefCell::new(HashMap::new()),
            current_mouse_buttons: vec![],
            current_mouse_capture: None,
            last_mouse_click: None,
            current_highlight: None,
            quad_generation: 0,
            shape_generation: 0,
            shape_cache: RefCell::new(LfuCache::new(
                "shape_cache.hit.rate",
                "shape_cache.miss.rate",
                |config| config.shape_cache_size,
                &config,
            )),
            line_state_cache: RefCell::new(LfuCacheU64::new(
                "line_state_cache.hit.rate",
                "line_state_cache.miss.rate",
                |config| config.line_state_cache_size,
                &config,
            )),
            next_line_state_id: 0,
            line_quad_cache: RefCell::new(LfuCache::new(
                "line_quad_cache.hit.rate",
                "line_quad_cache.miss.rate",
                |config| config.line_quad_cache_size,
                &config,
            )),
            line_to_ele_shape_cache: RefCell::new(LfuCache::new(
                "line_to_ele_shape_cache.hit.rate",
                "line_to_ele_shape_cache.miss.rate",
                |config| config.line_to_ele_shape_cache_size,
                &config,
            )),
            last_status_call: Instant::now(),
            cursor_blink_state: RefCell::new(ColorEase::new(
                config.cursor_blink_rate,
                config.cursor_blink_ease_in,
                config.cursor_blink_rate,
                config.cursor_blink_ease_out,
                None,
            )),
            blink_state: RefCell::new(ColorEase::new(
                config.text_blink_rate,
                config.text_blink_ease_in,
                config.text_blink_rate,
                config.text_blink_ease_out,
                None,
            )),
            rapid_blink_state: RefCell::new(ColorEase::new(
                config.text_blink_rate_rapid,
                config.text_blink_rapid_ease_in,
                config.text_blink_rate_rapid,
                config.text_blink_rapid_ease_out,
                None,
            )),
            event_states: HashMap::new(),
            current_event: None,
            has_animation: RefCell::new(None),
            scheduled_animation: RefCell::new(None),
            allow_images: AllowImage::Yes,
            semantic_zones: HashMap::new(),
            ui_items: vec![],
            dragging: None,
            last_ui_item: None,
            is_click_to_focus_window: false,
            key_table_state: KeyTableState::default(),
            modal: RefCell::new(None),
            opengl_info: None,
        };

        let tw = Rc::new(RefCell::new(myself));
        let tw_event = Rc::clone(&tw);

        let mut x = None;
        let mut y = None;
        let mut origin = GeometryOrigin::default();

        if let Some(position) = mux
            .get_window(mux_window_id)
            .and_then(|window| window.get_initial_position().clone())
            .or_else(|| POSITION.lock().unwrap().take())
        {
            x.replace(position.x);
            y.replace(position.y);
            origin = position.origin;
        }

        let geometry = RequestedWindowGeometry {
            width: Dimension::Pixels(dimensions.pixel_width as f32),
            height: Dimension::Pixels(dimensions.pixel_height as f32),
            x,
            y,
            origin,
        };
        log::trace!("{:?}", geometry);

        let window = Window::new_window(
            &get_window_class(),
            "wezterm",
            geometry,
            Some(&config),
            Rc::clone(&fontconfig),
            move |event, window| {
                let mut tw = tw_event.borrow_mut();
                if let Err(err) = tw.dispatch_window_event(event, window) {
                    log::error!("dispatch_window_event: {:#}", err);
                }
            },
        )
        .await?;
        tw.borrow_mut().window.replace(window.clone());

        Self::apply_icon(&window)?;

        let config_subscription = config::subscribe_to_config_reload({
            let window = window.clone();
            move || {
                window.notify(TermWindowNotif::Apply(Box::new(|tw| {
                    tw.config_was_reloaded()
                })));
                true
            }
        });

        let gl = match config.front_end {
            FrontEndSelection::WebGpu => None,
            _ => Some(window.enable_opengl().await?),
        };

        {
            let mut myself = tw.borrow_mut();
            let webgpu = match config.front_end {
                FrontEndSelection::WebGpu => Some(Rc::new(
                    WebGpuState::new(&window, dimensions, &config).await?,
                )),
                _ => None,
            };
            myself.config_subscription.replace(config_subscription);
            if config.use_resize_increments {
                window.set_resize_increments(
                    ResizeIncrementCalculator {
                        x: myself.render_metrics.cell_size.width as u16,
                        y: myself.render_metrics.cell_size.height as u16,
                        padding_left: padding_left,
                        padding_top: padding_top,
                        padding_right: padding_right,
                        padding_bottom: padding_bottom,
                        border: border,
                        tab_bar_height: tab_bar_height,
                    }
                    .into(),
                );
            }

            if let Some(gl) = gl {
                myself.gl.replace(Rc::clone(&gl));
                myself.created(RenderContext::Glium(Rc::clone(&gl)))?;
            }
            if let Some(webgpu) = webgpu {
                myself.webgpu.replace(Rc::clone(&webgpu));
                myself.created(RenderContext::WebGpu(Rc::clone(&webgpu)))?;
            }
            myself.load_os_parameters();
            window.show();
            myself.subscribe_to_pane_updates();
            myself.emit_window_event("window-config-reloaded", None);
            myself.emit_status_event();
        }

        crate::update::start_update_checker();
        front_end().record_known_window(window, mux_window_id);

        Ok(())
    }

    fn dispatch_window_event(
        &mut self,
        event: WindowEvent,
        window: &Window,
    ) -> anyhow::Result<bool> {
        log::debug!("{event:?}");
        match event {
            WindowEvent::Destroyed => {
                // Ensure that we cancel any overlays we had running, so
                // that the mux can empty out, otherwise the mux keeps
                // the TermWindow alive via the frontend even though
                // the window is gone and we'll linger forever.
                // <https://github.com/wezterm/wezterm/issues/3522>
                self.clear_all_overlays();
                Ok(false)
            }
            WindowEvent::CloseRequested => {
                self.close_requested(window);
                Ok(true)
            }
            WindowEvent::AppearanceChanged(appearance) => {
                log::debug!("Appearance is now {:?}", appearance);
                // This is a bit fugly; we get per-window notifications
                // for appearance changes which successfully updates the
                // per-window config, but we need to explicitly tell the
                // global config to reload, otherwise things that acces
                // the config via config::configuration() will see the
                // prior version of the config.
                // What's fugly about this is that we'll reload the
                // global config here once per window, which could
                // be nasty for folks with a lot of windows.
                // <https://github.com/wezterm/wezterm/issues/2295>
                config::reload();
                self.config_was_reloaded();
                Ok(true)
            }
            WindowEvent::PerformKeyAssignment(action) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    self.perform_key_assignment(&pane, &action)?;
                    window.invalidate();
                }
                Ok(true)
            }
            WindowEvent::FocusChanged(focused) => {
                self.focus_changed(focused, window);
                Ok(true)
            }
            WindowEvent::MouseEvent(event) => {
                self.mouse_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::MouseLeave => {
                self.mouse_leave_impl(window);
                Ok(true)
            }
            WindowEvent::Resized {
                dimensions,
                window_state,
                live_resizing,
            } => {
                self.resize(dimensions, window_state, window, live_resizing);
                Ok(true)
            }
            WindowEvent::SetInnerSizeCompleted => {
                self.resizes_pending -= 1;
                if self.is_repaint_pending {
                    self.is_repaint_pending = false;
                    if self.webgpu.is_some() {
                        self.do_paint_webgpu()?;
                    } else {
                        self.do_paint(window);
                    }
                }
                self.apply_pending_scale_changes();
                Ok(true)
            }
            WindowEvent::AdviseModifiersLedStatus(modifiers, leds) => {
                self.current_modifier_and_leds = (modifiers, leds);
                self.update_title();
                window.invalidate();
                Ok(true)
            }
            WindowEvent::RawKeyEvent(event) => {
                self.raw_key_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::KeyEvent(event) => {
                self.key_event_impl(event, window);
                Ok(true)
            }
            WindowEvent::AdviseDeadKeyStatus(status) => {
                if self.config.debug_key_events {
                    log::info!("DeadKeyStatus now: {:?}", status);
                } else {
                    log::trace!("DeadKeyStatus now: {:?}", status);
                }
                self.dead_key_status = status;
                self.update_title();
                // Ensure that we repaint so that any composing
                // text is updated
                window.invalidate();
                Ok(true)
            }
            WindowEvent::NeedRepaint => {
                if self.resizes_pending > 0 {
                    self.is_repaint_pending = true;
                    Ok(true)
                } else if self.webgpu.is_some() {
                    self.do_paint_webgpu()
                } else {
                    Ok(self.do_paint(window))
                }
            }
            WindowEvent::Notification(item) => {
                if let Ok(notif) = item.downcast::<TermWindowNotif>() {
                    self.dispatch_notif(*notif, window)
                        .context("dispatch_notif")?;
                }
                Ok(true)
            }
            WindowEvent::DroppedString(text) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                pane.send_paste(text.as_str())?;
                Ok(true)
            }
            WindowEvent::DroppedUrl(urls) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                let urls = urls
                    .iter()
                    .map(|url| self.config.quote_dropped_files.escape(&url.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " ";
                pane.send_paste(urls.as_str())?;
                Ok(true)
            }
            WindowEvent::DroppedFile(paths) => {
                let pane = match self.get_active_pane_or_overlay() {
                    Some(pane) => pane,
                    None => return Ok(true),
                };
                let paths = paths
                    .iter()
                    .map(|path| {
                        self.config
                            .quote_dropped_files
                            .escape(&path.to_string_lossy())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
                    + " ";
                pane.send_paste(&paths)?;
                Ok(true)
            }
            WindowEvent::DraggedFile(_) => Ok(true),
        }
    }

    fn do_paint(&mut self, window: &Window) -> bool {
        let gl = match self.gl.as_ref() {
            Some(gl) => gl,
            None => return false,
        };

        if gl.is_context_lost() {
            log::error!("opengl context was lost; should reinit");
            window.close();
            front_end().forget_known_window(window);
            return false;
        }

        let mut frame = glium::Frame::new(
            Rc::clone(&gl),
            (
                self.dimensions.pixel_width as u32,
                self.dimensions.pixel_height as u32,
            ),
        );
        self.paint_impl(&mut RenderFrame::Glium(&mut frame));
        window.finish_frame(frame).is_ok()
    }

    fn do_paint_webgpu(&mut self) -> anyhow::Result<bool> {
        self.webgpu.as_mut().unwrap().resize(self.dimensions);
        match self.do_paint_webgpu_impl() {
            Ok(ok) => Ok(ok),
            Err(err) => {
                match err.downcast_ref::<wgpu::SurfaceError>() {
                    Some(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        self.webgpu.as_mut().unwrap().resize(self.dimensions);
                        return self.do_paint_webgpu_impl();
                    }
                    _ => {}
                }
                Err(err)
            }
        }
    }

    fn do_paint_webgpu_impl(&mut self) -> anyhow::Result<bool> {
        self.paint_impl(&mut RenderFrame::WebGpu);
        Ok(true)
    }

    fn dispatch_notif(&mut self, notif: TermWindowNotif, window: &Window) -> anyhow::Result<()> {
        fn chan_err<T>(e: smol::channel::TrySendError<T>) -> anyhow::Error {
            anyhow::anyhow!("{}", e)
        }

        match notif {
            TermWindowNotif::InvalidateShapeCache => {
                self.shape_generation += 1;
                self.shape_cache.borrow_mut().clear();
                self.invalidate_modal();
                window.invalidate();
            }
            TermWindowNotif::PerformAssignment {
                pane_id,
                assignment,
                tx,
            } => {
                let mux = Mux::get();
                let result = || -> anyhow::Result<()> {
                    // The CopyMode overlay doesn't exist in the mux, but aliases
                    // itself with the overlaid pane's pane_id.
                    // So we do a bit of fancy footwork here to resolve the overlay
                    // and use that if it has the same pane_id, but otherwise fall
                    // back to what we get from the mux.
                    // <https://github.com/wezterm/wezterm/issues/3209>
                    let active_pane = self
                        .get_active_pane_or_overlay()
                        .ok_or_else(|| anyhow!("there is no active pane!?"))?;
                    let pane = if active_pane.pane_id() == pane_id {
                        active_pane
                    } else {
                        mux.get_pane(pane_id)
                            .ok_or_else(|| anyhow!("pane id {} is not valid", pane_id))?
                    };
                    self.perform_key_assignment(&pane, &assignment)
                        .context("perform_key_assignment")?;
                    Ok(())
                }();
                window.invalidate();
                if let Some(tx) = tx {
                    tx.try_send(result).ok();
                }
            }
            TermWindowNotif::SetRightStatus(status) => {
                if status != self.right_status {
                    self.right_status = status;
                    self.update_title_post_status();
                } else {
                    self.schedule_next_status_update();
                }
            }
            TermWindowNotif::SetLeftStatus(status) => {
                if status != self.left_status {
                    self.left_status = status;
                    self.update_title_post_status();
                } else {
                    self.schedule_next_status_update();
                }
            }
            TermWindowNotif::GetDimensions(tx) => {
                tx.try_send((self.dimensions, self.window_state))
                    .map_err(chan_err)
                    .context("send GetDimensions response")?;
            }
            TermWindowNotif::GetEffectiveConfig(tx) => {
                tx.try_send(self.config.clone())
                    .map_err(chan_err)
                    .context("send GetEffectiveConfig response")?;
            }
            TermWindowNotif::FinishWindowEvent { name, again } => {
                self.finish_window_event(&name, again);
            }
            TermWindowNotif::GetConfigOverrides(tx) => {
                tx.try_send(self.config_overrides.clone())
                    .map_err(chan_err)
                    .context("send GetConfigOverrides response")?;
            }
            TermWindowNotif::SetConfigOverrides(value) => {
                if value != self.config_overrides {
                    self.config_overrides = value;
                    self.config_was_reloaded();
                }
            }
            TermWindowNotif::CancelOverlayForPane(pane_id) => {
                self.cancel_overlay_for_pane(pane_id);
            }
            TermWindowNotif::CancelOverlayForTab { tab_id, pane_id } => {
                self.cancel_overlay_for_tab(tab_id, pane_id);
            }
            TermWindowNotif::MuxNotification(n) => match n {
                MuxNotification::Alert {
                    alert: Alert::SetUserVar { name, value },
                    pane_id,
                } => {
                    self.emit_user_var_event(pane_id, name, value);
                }
                MuxNotification::WindowTitleChanged { .. }
                | MuxNotification::Alert {
                    alert:
                        Alert::OutputSinceFocusLost
                        | Alert::CurrentWorkingDirectoryChanged
                        | Alert::WindowTitleChanged(_)
                        | Alert::TabTitleChanged(_)
                        | Alert::IconTitleChanged(_)
                        | Alert::Progress(_),
                    ..
                } => {
                    self.update_title();
                }
                MuxNotification::Alert {
                    alert: Alert::PaletteChanged,
                    pane_id,
                } => {
                    // Shape cache includes color information, so
                    // ensure that we invalidate that as part of
                    // this overall invalidation for the palette
                    self.dispatch_notif(TermWindowNotif::InvalidateShapeCache, window)?;
                    self.mux_pane_output_event(pane_id);
                }
                MuxNotification::Alert {
                    alert: Alert::Bell,
                    pane_id,
                } => {
                    if !self.window_contains_pane(pane_id) {
                        return Ok(());
                    }

                    match self.config.audible_bell {
                        AudibleBell::SystemBeep => {
                            Connection::get().expect("on main thread").beep();
                        }
                        AudibleBell::Disabled => {}
                    }

                    log::trace!("Ding! (this is the bell) in pane {}", pane_id);
                    self.emit_window_event("bell", Some(pane_id));

                    let mut per_pane = self.pane_state(pane_id);
                    per_pane.bell_start.replace(Instant::now());
                    window.invalidate();
                }
                MuxNotification::Alert {
                    alert: Alert::ToastNotification { .. },
                    ..
                } => {}
                MuxNotification::TabAddedToWindow {
                    window_id: _,
                    tab_id,
                } => {
                    let mux = Mux::get();
                    let mut size = self.terminal_size;
                    if let Some(tab) = mux.get_tab(tab_id) {
                        // If we attached to a remote domain and loaded in
                        // a tab async, we need to fixup its size, either
                        // by resizing it or resizes ourselves.
                        // The strategy here is to adjust both by taking
                        // the maximal size in both horizontal and vertical
                        // dimensions and applying that. In practice that
                        // means that a new local client will resize larger
                        // to adjust to the size of an existing client.
                        let tab_size = tab.get_size();
                        size.rows = size.rows.max(tab_size.rows);
                        size.cols = size.cols.max(tab_size.cols);

                        if size.rows != self.terminal_size.rows
                            || size.cols != self.terminal_size.cols
                            || size.pixel_width != self.terminal_size.pixel_width
                            || size.pixel_height != self.terminal_size.pixel_height
                        {
                            self.set_window_size(size, window)?;
                        } else if tab_size.dpi == 0 {
                            log::debug!("fixup dpi in newly added tab");
                            tab.resize(self.terminal_size);
                        }
                    }
                }
                MuxNotification::PaneOutput(pane_id) => {
                    self.mux_pane_output_event(pane_id);
                }
                MuxNotification::WindowInvalidated(_) => {
                    window.invalidate();
                    self.update_title_post_status();
                }
                MuxNotification::WindowRemoved(_window_id) => {
                    // Handled by frontend
                }
                MuxNotification::AssignClipboard { .. } => {
                    // Handled by frontend
                }
                MuxNotification::SaveToDownloads { .. } => {
                    // Handled by frontend
                }
                MuxNotification::PaneFocused(_) => {
                    // Also handled by clientpane
                    self.update_title_post_status();
                }
                MuxNotification::TabResized(_) => {
                    // Also handled by wezterm-client
                    self.update_title_post_status();
                }
                MuxNotification::TabTitleChanged { .. } => {
                    self.update_title_post_status();
                }
                MuxNotification::PaneAdded(_)
                | MuxNotification::WorkspaceRenamed { .. }
                | MuxNotification::PaneRemoved(_)
                | MuxNotification::WindowWorkspaceChanged(_)
                | MuxNotification::ActiveWorkspaceChanged(_)
                | MuxNotification::Empty
                | MuxNotification::WindowCreated(_) => {}
            },
            TermWindowNotif::EmitStatusUpdate => {
                self.emit_status_event();
            }
            TermWindowNotif::GetSelectionForPane { pane_id, tx } => {
                let mux = Mux::get();
                let pane = mux
                    .get_pane(pane_id)
                    .ok_or_else(|| anyhow!("pane id {} is not valid", pane_id))?;

                tx.try_send(self.selection_text(&pane))
                    .map_err(chan_err)
                    .context("send GetSelectionForPane response")?;
            }
            TermWindowNotif::Apply(func) => {
                func(self);
            }
            TermWindowNotif::SwitchToMuxWindow(mux_window_id) => {
                self.mux_window_id = mux_window_id;
                *self.mux_window_id_for_subscriptions.lock().unwrap() = mux_window_id;

                self.clear_all_overlays();
                self.current_highlight.take();
                self.invalidate_fancy_tab_bar();
                self.invalidate_modal();

                let mux = Mux::get();
                if let Some(window) = mux.get_window(self.mux_window_id) {
                    for tab in window.iter() {
                        tab.resize(self.terminal_size);
                    }
                };
                self.update_title();
                window.invalidate();
            }
            TermWindowNotif::SetInnerSize { width, height } => {
                self.set_inner_size(window, width, height);
            }
        }

        Ok(())
    }

    fn set_inner_size(&mut self, window: &Window, width: usize, height: usize) {
        self.resizes_pending += 1;
        window.set_inner_size(width, height);
    }

    /// Take care to remove our panes from the mux, otherwise
    /// we can leave the mux with no windows but some panes
    /// and it won't believe that we are empty.
    fn clear_all_overlays(&mut self) {
        let overlay_panes_to_cancel = self
            .pane_state
            .borrow()
            .iter()
            .filter_map(|(_, state)| state.overlay.as_ref().map(|overlay| overlay.pane.pane_id()))
            .collect::<Vec<_>>();

        for pane_id in overlay_panes_to_cancel {
            self.cancel_overlay_for_pane(pane_id);
        }

        let tab_overlays_to_cancel = self
            .tab_state
            .borrow()
            .iter()
            .filter_map(|(tab_id, state)| state.overlay.as_ref().map(|_| *tab_id))
            .collect::<Vec<_>>();

        for tab_id in tab_overlays_to_cancel {
            self.cancel_overlay_for_tab(tab_id, None);
        }

        self.pane_state.borrow_mut().clear();
        self.tab_state.borrow_mut().clear();
    }

    fn apply_icon(window: &Window) -> anyhow::Result<()> {
        let image = image::load_from_memory(ICON_DATA)?.into_rgba8();
        let (width, height) = image.dimensions();
        window.set_icon(Image::with_rgba32(
            width as usize,
            height as usize,
            width as usize * 4,
            image.as_raw(),
        ));
        Ok(())
    }

    fn schedule_status_update(&self) {
        if let Some(window) = self.window.as_ref() {
            window.notify(TermWindowNotif::EmitStatusUpdate);
        }
    }

    fn is_pane_visible(&mut self, pane_id: PaneId) -> bool {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return false,
        };

        let tab_id = tab.tab_id();
        if let Some(tab_overlay) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            return tab_overlay.pane_id() == pane_id;
        }

        tab.contains_pane(pane_id)
    }

    fn mux_pane_output_event(&mut self, pane_id: PaneId) {
        metrics::histogram!("mux.pane_output_event.rate").record(1.);
        if self.is_pane_visible(pane_id) {
            if let Some(ref win) = self.window {
                win.invalidate();
            }
        }
    }

    fn mux_pane_output_event_callback(
        n: MuxNotification,
        window: &Window,
        mux_window_id: MuxWindowId,
        dead: &Arc<AtomicBool>,
    ) -> bool {
        if dead.load(Ordering::Relaxed) {
            // Subscription cancelled asynchronously
            return false;
        }

        match n {
            MuxNotification::Alert {
                pane_id,
                alert:
                    Alert::OutputSinceFocusLost
                    | Alert::CurrentWorkingDirectoryChanged
                    | Alert::WindowTitleChanged(_)
                    | Alert::TabTitleChanged(_)
                    | Alert::IconTitleChanged(_)
                    | Alert::Progress(_)
                    | Alert::SetUserVar { .. }
                    | Alert::Bell,
            }
            | MuxNotification::PaneFocused(pane_id)
            | MuxNotification::PaneRemoved(pane_id)
            | MuxNotification::PaneOutput(pane_id) => {
                // Check window validity and propagate to the window event handler
                // that will do the full pane visibility check.
                let mux = Mux::get();
                if mux.get_window(mux_window_id).is_none() {
                    // If the window is not found, the mux_window_id may be stale during
                    // a workspace switch - skip this notif but keep the subscription.
                    // (next notifs should finish the workspace switch & reconcile the state)
                    return true;
                }
                let _ = pane_id;
            }
            MuxNotification::PaneAdded(_pane_id) => {
                // If some other client spawns a pane inside this window, this
                // gives us an opportunity to attach it to the clipboard.
                let mux = Mux::get();
                return mux.get_window(mux_window_id).is_some();
            }
            MuxNotification::TabAddedToWindow { window_id, .. }
            | MuxNotification::WindowTitleChanged { window_id, .. }
            | MuxNotification::WindowInvalidated(window_id) => {
                if window_id != mux_window_id {
                    return true;
                }
            }
            MuxNotification::WindowRemoved(window_id) => {
                if window_id != mux_window_id {
                    return true;
                }
                // The removed window matches our current mux_window_id.
                // During workspace switches, mux_window_id may be stale.
                // Skip this notification but keep the subscription alive.
                // (next notifs should finish the workspace switch & reconcile the state)
                return true;
            }
            MuxNotification::TabResized(tab_id)
            | MuxNotification::TabTitleChanged { tab_id, .. } => {
                let mux = Mux::get();
                if mux.window_containing_tab(tab_id) == Some(mux_window_id) {
                    // fall through
                } else {
                    return true;
                }
            }
            MuxNotification::Alert {
                alert: Alert::ToastNotification { .. },
                ..
            }
            | MuxNotification::AssignClipboard { .. }
            | MuxNotification::SaveToDownloads { .. }
            | MuxNotification::WindowCreated(_)
            | MuxNotification::ActiveWorkspaceChanged(_)
            | MuxNotification::WorkspaceRenamed { .. }
            | MuxNotification::Empty
            | MuxNotification::WindowWorkspaceChanged(_) => return true,
            MuxNotification::Alert {
                alert: Alert::PaletteChanged { .. },
                ..
            } => {
                // fall through
            }
        }

        window.notify(TermWindowNotif::MuxNotification(n));

        true
    }

    fn subscribe_to_pane_updates(&self) {
        let window = self.window.clone().expect("window to be valid on startup");
        let mux_window_id = Arc::clone(&self.mux_window_id_for_subscriptions);
        let mux = Mux::get();
        let dead = Arc::clone(&self.mux_subscription_dead);
        mux.subscribe(move |n| {
            if dead.load(Ordering::Relaxed) {
                // Unsubscribe this handler from the mux
                return false;
            }
            let mux_window_id = *mux_window_id.lock().unwrap();
            let window = window.clone();
            let dead = dead.clone();
            promise::spawn::spawn_into_main_thread(async move {
                Self::mux_pane_output_event_callback(n, &window, mux_window_id, &dead)
            })
            .detach();
            true
        });
    }

    fn emit_status_event(&mut self) {
        self.emit_window_event("update-right-status", None);
        self.emit_window_event("update-status", None);
    }

    fn schedule_window_event(&mut self, name: &str, pane_id: Option<PaneId>) {
        let window = GuiWin::new(self);
        let pane = match pane_id {
            Some(pane_id) => Mux::get().get_pane(pane_id),
            None => None,
        };
        let pane = match pane {
            Some(pane) => pane,
            None => match self.get_active_pane_or_overlay() {
                Some(pane) => pane,
                None => return,
            },
        };
        let pane = MuxPane(pane.pane_id());
        let name = name.to_string();

        async fn do_event(
            lua: Option<Rc<mlua::Lua>>,
            name: String,
            window: GuiWin,
            pane: MuxPane,
        ) -> anyhow::Result<()> {
            let again = if let Some(lua) = lua {
                let args = lua.pack_multi((window.clone(), pane))?;

                if let Err(err) = config::lua::emit_event(&lua, (name.clone(), args)).await {
                    log::error!("while processing {} event: {:#}", name, err);
                }
                true
            } else {
                false
            };

            window
                .window
                .notify(TermWindowNotif::FinishWindowEvent { name, again });

            Ok(())
        }

        promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
            do_event(lua, name, window, pane)
        }))
        .detach();
    }

    /// Called as part of finishing up a callout to lua.
    /// If again==false it means that there isn't a lua config
    /// to execute against, so we should just mark as done.
    /// Otherwise, if there is a queued item, schedule it now.
    fn finish_window_event(&mut self, name: &str, again: bool) {
        let state = self
            .event_states
            .entry(name.to_string())
            .or_insert(EventState::None);
        if again {
            match state {
                EventState::InProgress => {
                    *state = EventState::None;
                }
                EventState::InProgressWithQueued(pane) => {
                    let pane = *pane;
                    *state = EventState::InProgress;
                    self.schedule_window_event(name, pane);
                }
                EventState::None => {}
            }
        } else {
            *state = EventState::None;
        }
    }

    pub fn emit_window_event(&mut self, name: &str, pane_id: Option<PaneId>) {
        if self.get_active_pane_or_overlay().is_none() || self.window.is_none() {
            return;
        }

        let state = self
            .event_states
            .entry(name.to_string())
            .or_insert(EventState::None);
        match state {
            EventState::InProgress => {
                // Flag that we want to run again when the currently
                // executing event calls finish_window_event().
                *state = EventState::InProgressWithQueued(pane_id);
                return;
            }
            EventState::InProgressWithQueued(other_pane) => {
                // We've already got one copy executing and another
                // pending dispatch, so don't queue another.
                if pane_id != *other_pane {
                    log::warn!(
                        "Cannot queue {} event for pane {:?}, as \
                         there is already an event queued for pane {:?} \
                         in the same window",
                        name,
                        pane_id,
                        other_pane
                    );
                }
                return;
            }
            EventState::None => {
                // Nothing pending, so schedule a call now
                *state = EventState::InProgress;
                self.schedule_window_event(name, pane_id);
            }
        }
    }

    fn check_for_dirty_lines_and_invalidate_selection(&mut self, pane: &Arc<dyn Pane>) {
        let dims = pane.get_dimensions();
        let viewport = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top);
        let visible_range = viewport..viewport + dims.viewport_rows as StableRowIndex;
        let seqno = self.selection(pane.pane_id()).seqno;
        let dirty = pane.get_changed_since(visible_range, seqno);

        if dirty.is_empty() {
            return;
        }
        if pane.downcast_ref::<CopyOverlay>().is_none()
            && pane.downcast_ref::<QuickSelectOverlay>().is_none()
        {
            // If any of the changed lines intersect with the
            // selection, then we need to clear the selection, but not
            // when the search overlay is active; the search overlay
            // marks lines as dirty to force invalidate them for
            // highlighting purpose but also manipulates the selection
            // and we want to allow it to retain the selection it made!

            let clear_selection =
                if let Some(selection_range) = self.selection(pane.pane_id()).range.as_ref() {
                    let selection_rows = selection_range.rows();
                    selection_rows.into_iter().any(|row| dirty.contains(row))
                } else {
                    false
                };

            if clear_selection {
                self.selection(pane.pane_id()).range.take();
                self.selection(pane.pane_id()).origin.take();
                self.selection(pane.pane_id()).seqno = pane.get_current_seqno();
            }
        }
    }
}

impl TermWindow {
    fn palette(&mut self) -> &ColorPalette {
        if self.palette.is_none() {
            self.palette
                .replace(config::TermConfig::new().color_palette());
        }
        self.palette.as_ref().unwrap()
    }

    pub fn config_was_reloaded(&mut self) {
        log::debug!(
            "config was reloaded, overrides: {:?}",
            self.config_overrides
        );
        self.key_table_state.clear_stack();
        self.connection_name = Connection::get().unwrap().name();
        let config = match config::overridden_config(&self.config_overrides) {
            Ok(config) => config,
            Err(err) => {
                log::error!(
                    "Failed to apply config overrides to window: {:#}: {:?}",
                    err,
                    self.config_overrides
                );
                configuration()
            }
        };
        self.config = config.clone();
        self.palette.take();

        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return,
        };
        if window.len() == 1 {
            self.show_tab_bar = config.enable_tab_bar && !config.hide_tab_bar_if_only_one_tab;
        } else {
            self.show_tab_bar = config.enable_tab_bar;
        }
        *self.cursor_blink_state.borrow_mut() = ColorEase::new(
            config.cursor_blink_rate,
            config.cursor_blink_ease_in,
            config.cursor_blink_rate,
            config.cursor_blink_ease_out,
            None,
        );
        *self.blink_state.borrow_mut() = ColorEase::new(
            config.text_blink_rate,
            config.text_blink_ease_in,
            config.text_blink_rate,
            config.text_blink_ease_out,
            None,
        );
        *self.rapid_blink_state.borrow_mut() = ColorEase::new(
            config.text_blink_rate_rapid,
            config.text_blink_rapid_ease_in,
            config.text_blink_rate_rapid,
            config.text_blink_rapid_ease_out,
            None,
        );

        self.show_scroll_bar = config.enable_scroll_bar;
        self.shape_generation += 1;
        {
            let mut shape_cache = self.shape_cache.borrow_mut();
            shape_cache.update_config(&config);
            shape_cache.clear();
        }
        self.line_state_cache.borrow_mut().update_config(&config);
        self.line_quad_cache.borrow_mut().update_config(&config);
        self.line_to_ele_shape_cache
            .borrow_mut()
            .update_config(&config);
        self.fancy_tab_bar.take();
        self.invalidate_fancy_tab_bar();
        self.invalidate_modal();
        self.input_map = InputMap::new(&config);
        self.leader_is_down = None;
        self.render_state.as_mut().map(|rs| rs.config_changed());
        let dimensions = self.dimensions;

        if let Err(err) = self.fonts.config_changed(&config) {
            log::error!("Failed to load font configuration: {:#}", err);
        }

        if let Some(window) = mux.get_window(self.mux_window_id) {
            let term_config: Arc<dyn TerminalConfiguration> =
                Arc::new(TermConfig::with_config(config.clone()));
            for tab in window.iter() {
                for pane in tab.iter_panes_ignoring_zoom() {
                    pane.pane.set_config(Arc::clone(&term_config));
                }
            }
            for state in self.pane_state.borrow().values() {
                if let Some(overlay) = &state.overlay {
                    overlay.pane.set_config(Arc::clone(&term_config));
                }
            }
            for state in self.tab_state.borrow().values() {
                if let Some(overlay) = &state.overlay {
                    overlay.pane.set_config(Arc::clone(&term_config));
                }
            }
        }

        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            self.load_os_parameters();
            self.apply_scale_change(&dimensions, self.fonts.get_font_scale());
            self.apply_dimensions(&dimensions, None, &window);
            window.config_did_change(&config);
            window.invalidate();
        }

        // Do this after we've potentially adjusted scaling based on config/padding
        // and window size
        self.window_background = reload_background_image(
            &config,
            &self.window_background,
            &self.dimensions,
            &self.render_metrics,
        );

        self.invalidate_modal();
        self.emit_window_event("window-config-reloaded", None);
    }

    fn invalidate_modal(&mut self) {
        if let Some(modal) = self.get_modal() {
            modal.reconfigure(self);
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    pub fn cancel_modal(&self) {
        self.modal.borrow_mut().take();
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn set_modal(&self, modal: Rc<dyn Modal>) {
        self.modal.borrow_mut().replace(modal);
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn get_modal(&self) -> Option<Rc<dyn Modal>> {
        self.modal.borrow().as_ref().map(|m| Rc::clone(&m))
    }

    fn update_scrollbar(&mut self) {
        if !self.show_scroll_bar {
            return;
        }

        let tab = match self.get_active_pane_or_overlay() {
            Some(tab) => tab,
            None => return,
        };

        let render_dims = tab.get_dimensions();
        if render_dims == self.last_scroll_info {
            return;
        }

        self.last_scroll_info = render_dims;

        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Called by various bits of code to update the title bar.
    /// Let's also trigger the status event so that it can choose
    /// to update the right-status.
    fn update_title(&mut self) {
        self.schedule_status_update();
        self.update_title_impl();
    }

    fn window_contains_pane(&mut self, pane_id: PaneId) -> bool {
        let mux = Mux::get();

        let (_domain, window_id, _tab_id) = match mux.resolve_pane_id(pane_id) {
            Some(tuple) => tuple,
            None => return false,
        };

        return window_id == self.mux_window_id;
    }

    fn emit_user_var_event(&mut self, pane_id: PaneId, name: String, value: String) {
        if !self.window_contains_pane(pane_id) {
            return;
        }

        let mux = Mux::get();
        let window = GuiWin::new(self);
        let pane = match mux.get_pane(pane_id) {
            Some(pane) => mux_lua::MuxPane(pane.pane_id()),
            None => return,
        };

        async fn do_event(
            lua: Option<Rc<mlua::Lua>>,
            name: String,
            value: String,
            window: GuiWin,
            pane: MuxPane,
        ) -> anyhow::Result<()> {
            if let Some(lua) = lua {
                let args = lua.pack_multi((window.clone(), pane, name, value))?;
                if let Err(err) =
                    config::lua::emit_event(&lua, ("user-var-changed".to_string(), args)).await
                {
                    log::error!("while processing user-var-changed event: {:#}", err);
                }
            }

            window
                .window
                .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    term_window.update_title();
                })));

            Ok(())
        }

        promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
            do_event(lua, name, value, window, pane)
        }))
        .detach();
    }

    /// Called by window:set_right_status after the status has
    /// been updated; let's update the bar
    pub fn update_title_post_status(&mut self) {
        self.update_title_impl();
    }

    fn update_title_impl(&mut self) {
        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return,
        };
        let tabs = self.get_tab_information();
        let panes = self.get_pane_information();
        let active_tab = tabs.iter().find(|t| t.is_active).cloned();
        let active_pane = panes.iter().find(|p| p.is_active).cloned();

        let hovering_in_tab_bar = if self.sidebar_is_active() {
            false
        } else {
            let border = self.get_os_border();
            let tab_bar_height = self.tab_bar_pixel_height().unwrap_or(0.);
            let tab_bar_y = if self.config.tab_bar_at_bottom {
                ((self.dimensions.pixel_height as f32)
                    - (tab_bar_height + border.bottom.get() as f32))
                    .max(0.)
            } else {
                border.top.get() as f32
            };

            match &self.current_mouse_event {
                Some(event) => {
                    let mouse_y = event.coords.y as f32;
                    mouse_y >= tab_bar_y as f32 && mouse_y < tab_bar_y as f32 + tab_bar_height
                }
                None => false,
            }
        };

        let new_tab_bar = TabBarState::new(
            self.dimensions.pixel_width / self.render_metrics.cell_size.width as usize,
            if hovering_in_tab_bar {
                Some(self.last_mouse_coords.0)
            } else {
                None
            },
            &tabs,
            &panes,
            self.config.resolved_palette.tab_bar.as_ref(),
            &self.config,
            &self.left_status,
            &self.right_status,
        );
        if new_tab_bar != self.tab_bar {
            self.tab_bar = new_tab_bar;
            self.invalidate_fancy_tab_bar();
            self.invalidate_modal();
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }

        let num_tabs = window.len();
        if num_tabs == 0 {
            return;
        }
        drop(window);

        let title = match config::run_immediate_with_lua_config(|lua| {
            if let Some(lua) = lua {
                let tabs = lua.create_sequence_from(tabs.clone().into_iter())?;
                let panes = lua.create_sequence_from(panes.clone().into_iter())?;

                let v = config::lua::emit_sync_callback(
                    &*lua,
                    (
                        "format-window-title".to_string(),
                        (
                            active_tab.clone(),
                            active_pane.clone(),
                            tabs,
                            panes,
                            (*self.config).clone(),
                        ),
                    ),
                )?;
                match &v {
                    mlua::Value::Nil => Ok(None),
                    _ => Ok(Some(String::from_lua(v, &*lua)?)),
                }
            } else {
                Ok(None)
            }
        }) {
            Ok(s) => s,
            Err(err) => {
                log::warn!("format-window-title: {}", err);
                None
            }
        };

        let title = match title {
            Some(title) => title,
            None => {
                if let (Some(pos), Some(tab)) = (active_pane, active_tab) {
                    if num_tabs == 1 {
                        format!("{}{}", if pos.is_zoomed { "[Z] " } else { "" }, pos.title)
                    } else {
                        format!(
                            "{}[{}/{}] {}",
                            if pos.is_zoomed { "[Z] " } else { "" },
                            tab.tab_index + 1,
                            num_tabs,
                            pos.title
                        )
                    }
                } else {
                    "".to_string()
                }
            }
        };

        if let Some(window) = self.window.as_ref() {
            window.set_title(&title);

            let show_tab_bar = if num_tabs == 1 {
                self.config.enable_tab_bar && !self.config.hide_tab_bar_if_only_one_tab
            } else {
                self.config.enable_tab_bar
            };

            // If the number of tabs changed and caused the tab bar to
            // hide/show, then we'll need to resize things.  It is simplest
            // to piggy back on the config reloading code for that, so that
            // is what we're doing.
            if show_tab_bar != self.show_tab_bar {
                self.config_was_reloaded();
            }
        }
        self.schedule_next_status_update();
    }

    fn schedule_next_status_update(&mut self) {
        if let Some(window) = self.window.as_ref() {
            let now = Instant::now();
            if self.last_status_call <= now {
                let interval = Duration::from_millis(self.config.status_update_interval);
                let target = now + interval;
                self.last_status_call = target;

                let window = window.clone();
                promise::spawn::spawn(async move {
                    Timer::at(target).await;
                    window.notify(TermWindowNotif::EmitStatusUpdate);
                })
                .detach();
            }
        }
    }

    fn update_text_cursor(&mut self, pos: &PositionedPane) {
        if let Some(win) = self.window.as_ref() {
            let cursor = pos.pane.get_cursor_position();
            let top = pos.pane.get_dimensions().physical_top;
            let tab_bar_height =
                if self.show_tab_bar && !self.sidebar_is_active() && !self.config.tab_bar_at_bottom
                {
                    self.tab_bar_pixel_height().unwrap()
                } else {
                    0.0
                };
            let (padding_left, padding_top) = self.padding_left_top();

            let r = Rect::new(
                Point::new(
                    (((cursor.x + pos.left) as isize).max(0) * self.render_metrics.cell_size.width)
                        .add(padding_left as isize),
                    ((cursor.y + pos.top as isize - top).max(0)
                        * self.render_metrics.cell_size.height)
                        .add(tab_bar_height as isize)
                        .add(padding_top as isize),
                ),
                self.render_metrics.cell_size,
            );
            win.set_text_cursor_position(r);
        }
    }

    fn activate_window(&mut self, window_idx: usize) -> anyhow::Result<()> {
        let windows = front_end().gui_windows();
        if let Some(win) = windows.get(window_idx) {
            win.window.focus();
        }
        Ok(())
    }

    fn activate_window_relative(&mut self, delta: isize, wrap: bool) -> anyhow::Result<()> {
        let windows = front_end().gui_windows();
        let my_idx = windows
            .iter()
            .position(|w| Some(&w.window) == self.window.as_ref())
            .ok_or_else(|| anyhow!("I'm not in the window list!?"))?;

        let idx = my_idx as isize + delta;

        let idx = if wrap {
            let idx = if idx < 0 {
                windows.len() as isize + idx
            } else {
                idx
            };
            idx as usize % windows.len()
        } else {
            if idx < 0 {
                0
            } else if idx >= windows.len() as isize {
                windows.len().saturating_sub(1)
            } else {
                idx as usize
            }
        };

        if let Some(win) = windows.get(idx) {
            win.window.focus();
        }

        Ok(())
    }

    fn activate_pane_by_id(&mut self, pane_id: PaneId) -> anyhow::Result<bool> {
        let target_tab = {
            let mux = Mux::get();
            let window = mux
                .get_window(self.mux_window_id)
                .ok_or_else(|| anyhow!("no such window"))?;
            let mut found = None;
            for tab_idx in 0..window.len() {
                if let Some(tab) = window.get_by_idx(tab_idx) {
                    if let Some(pane) = tab.get_active_pane() {
                        if pane.pane_id() == pane_id {
                            found = Some(tab_idx);
                            break;
                        }
                    }
                }
            }
            found
        };
        let Some(tab_idx) = target_tab else {
            return Ok(false);
        };
        let mux = Mux::get();
        let mut window = mux
            .get_window_mut(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;
        if tab_idx != window.get_active_idx() {
            window.save_and_then_set_active(tab_idx);
        }
        drop(window);
        if let Some(pane) = self.get_active_pane_or_overlay() {
            pane.focus_changed(true);
        }
        self.update_title();
        self.update_scrollbar();
        Ok(true)
    }

    fn activate_tab(&mut self, tab_idx: isize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let mut window = mux
            .get_window_mut(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        // This logic is coupled with the CliSubCommand::ActivateTab
        // logic in wezterm/src/main.rs. If you update this, update that!
        let max = window.len();

        let tab_idx = if tab_idx < 0 {
            max.saturating_sub(tab_idx.abs() as usize)
        } else {
            tab_idx as usize
        };

        if tab_idx < max {
            window.save_and_then_set_active(tab_idx);

            drop(window);

            if let Some(pane) = self.get_active_pane_or_overlay() {
                pane.focus_changed(true);
            }

            self.update_title();
            self.update_scrollbar();
        }
        Ok(())
    }

    fn activate_tab_relative(&mut self, delta: isize, wrap: bool) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        // This logic is coupled with the CliSubCommand::ActivateTab
        // logic in wezterm/src/main.rs. If you update this, update that!
        let active = window.get_active_idx() as isize;
        let tab = active + delta;
        let tab = if wrap {
            let tab = if tab < 0 { max as isize + tab } else { tab };
            (tab as usize % max) as isize
        } else {
            if tab < 0 {
                0
            } else if tab >= max as isize {
                max as isize - 1
            } else {
                tab
            }
        };
        drop(window);
        self.activate_tab(tab)
    }

    fn activate_last_tab(&mut self) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let last_idx = window.get_last_active_idx();
        drop(window);
        match last_idx {
            Some(idx) => self.activate_tab(idx as isize),
            None => Ok(()),
        }
    }

    fn move_tab(&mut self, tab_idx: usize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let mut window = mux
            .get_window_mut(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        let active = window.get_active_idx();

        ensure!(tab_idx < max, "cannot move a tab out of range");

        let tab_inst = window.remove_by_idx(active);
        window.insert(tab_idx, &tab_inst);
        window.set_active_without_saving(tab_idx);

        drop(window);
        self.update_title();
        self.update_scrollbar();

        Ok(())
    }

    fn show_input_selector(&mut self, args: &config::keyassignment::InputSelector) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        // Ignore any current overlay: we're going to cancel it out below
        // and we don't want this new one to reference that cancelled pane
        let pane = match self.get_active_pane_no_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::selector::selector(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_prompt_input_line(&mut self, args: &PromptInputLine) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::prompt::show_line_prompt_overlay(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_confirmation(&mut self, args: &Confirmation) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let args = args.clone();

        let gui_win = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::confirm::show_confirmation_overlay(term, args, gui_win, pane)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    fn show_debug_overlay(&mut self) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let gui_win = GuiWin::new(self);

        let opengl_info = self.opengl_info.as_deref().unwrap_or("Unknown").to_string();
        let connection_info = self.connection_name.clone();

        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::show_debug_overlay(term, gui_win, opengl_info, connection_info)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    /// Open the activity-log overlay for a single agent. The snapshot is cloned
    /// into the overlay closure; the overlay re-reads the transcript live.
    fn show_agent_log(&mut self, agent: crate::agent_herd::HerdAgent) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let (overlay, future) = start_overlay(self, &tab, move |_tab_id, term| {
            crate::overlay::show_agent_log_overlay(term, agent)
        });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    /// Enter keyboard navigation of the agent section: pick the first
    /// attention-needing agent (else the first agent) and put the cursor there.
    fn activate_agent_section_nav(&mut self) {
        let mut state = self.agent_herd_state.borrow_mut();
        if state.agents.is_empty() {
            return;
        }
        let first_attention = state
            .agents
            .iter()
            .find(|agent| {
                agent
                    .display_status(std::time::SystemTime::now())
                    .is_attention()
            })
            .or_else(|| state.agents.first());
        state.selection = first_attention.map(|agent| agent.key());
        state.nav_active = true;
        state.scroll_offset = state
            .scroll_offset
            .min(state.agents.len().saturating_sub(state.visible_rows));
        let sel = state.selection.clone();
        drop(state);
        if let Some(sel) = sel {
            self.scroll_agent_selection_into_view(&sel);
        }
    }

    /// Keep the keyboard-selected agent row on screen.
    pub(crate) fn scroll_agent_selection_into_view(&mut self, key: &crate::agent_herd::AgentKey) {
        let mut state = self.agent_herd_state.borrow_mut();
        if let Some(idx) = state.agents.iter().position(|agent| &agent.key() == key) {
            if idx < state.scroll_offset {
                state.scroll_offset = idx;
            } else if idx >= state.scroll_offset + state.visible_rows.max(1) {
                state.scroll_offset = idx + 1 - state.visible_rows.max(1);
            }
        }
    }

    /// Handle a navigation key while `nav_active`. Returns true if consumed.
    pub(crate) fn agent_section_nav_key(&mut self, keycode: &KeyCode) -> bool {
        let nav_active = self.agent_herd_state.borrow().nav_active;
        if !nav_active {
            return false;
        }
        // Every agent vanished (closed or rescoped): leave nav mode rather
        // than swallowing keys with nothing to navigate.
        if self.agent_herd_state.borrow().agents.is_empty() {
            self.agent_herd_state.borrow_mut().nav_active = false;
            return false;
        }
        match keycode {
            KeyCode::UpArrow => {
                self.agent_section_nav_move(-1);
                true
            }
            KeyCode::DownArrow => {
                self.agent_section_nav_move(1);
                true
            }
            KeyCode::Char(' ') | KeyCode::RightArrow => {
                let key = self.agent_herd_state.borrow().selection.clone();
                if let Some(key) = key {
                    self.toggle_herd_row_expansion(&key);
                }
                true
            }
            KeyCode::Char('\r') | KeyCode::LeftArrow => {
                let key = self.agent_herd_state.borrow().selection.clone();
                if let Some(key) = key {
                    self.agent_section_nav_activate(&key);
                }
                true
            }
            KeyCode::Char('\u{1b}') => {
                self.agent_herd_state.borrow_mut().nav_active = false;
                true
            }
            _ => false,
        }
    }

    fn agent_section_nav_move(&mut self, delta: isize) {
        let mut state = self.agent_herd_state.borrow_mut();
        if state.agents.is_empty() {
            return;
        }
        let current = state
            .selection
            .as_ref()
            .and_then(|key| state.agents.iter().position(|agent| &agent.key() == key));
        let next = match current {
            Some(idx) => {
                let len = state.agents.len() as isize;
                let raw = idx as isize + delta;
                (((raw % len) + len) % len) as usize
            }
            None => 0,
        };
        state.selection = Some(state.agents[next].key());
        let key = state.agents[next].key();
        drop(state);
        self.scroll_agent_selection_into_view(&key);
    }

    fn agent_section_nav_activate(&mut self, key: &crate::agent_herd::AgentKey) {
        let agent = self.herd_agent_by_key(key);
        match agent {
            Some(agent) if !agent.is_detached() => {
                self.focus_herd_agent(&agent);
                self.agent_herd_state.borrow_mut().nav_active = false;
            }
            Some(_) => {
                self.toggle_herd_row_expansion(key);
            }
            None => {}
        }
    }

    fn show_tab_navigator(&mut self) {
        let mux = Mux::get();
        let active_tab_idx = match mux.get_window(self.mux_window_id) {
            Some(mux_window) => mux_window.get_active_idx(),
            None => return,
        };
        let title = "Tab Navigator".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::TABS,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, active_tab_idx);
    }

    fn show_launcher(&mut self) {
        let title = "Launcher".to_string();
        let args = LauncherActionArgs {
            title: Some(title),
            flags: LauncherFlags::LAUNCH_MENU_ITEMS
                | LauncherFlags::WORKSPACES
                | LauncherFlags::DOMAINS
                | LauncherFlags::KEY_ASSIGNMENTS
                | LauncherFlags::COMMANDS,
            help_text: None,
            fuzzy_help_text: None,
            alphabet: None,
        };
        self.show_launcher_impl(args, 0);
    }

    fn show_launcher_impl(&mut self, args: LauncherActionArgs, initial_choice_idx: usize) {
        let mux_window_id = self.mux_window_id;
        let window = self.window.as_ref().unwrap().clone();

        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };

        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let domain_id_of_current_pane = tab
            .get_active_pane()
            .expect("tab has no panes!")
            .domain_id();
        let pane_id = pane.pane_id();
        let tab_id = tab.tab_id();
        let title = args.title.unwrap();
        let flags = args.flags;
        let help_text = args.help_text.unwrap_or(
            "Select an item and press Enter=launch  \
             Esc=cancel  /=filter"
                .to_string(),
        );
        let fuzzy_help_text = args
            .fuzzy_help_text
            .unwrap_or("Fuzzy matching: ".to_string());

        let config = &self.config;
        let alphabet = args.alphabet.unwrap_or(config.launcher_alphabet.clone());

        promise::spawn::spawn(async move {
            let args = LauncherArgs::new(
                &title,
                flags,
                mux_window_id,
                pane_id,
                domain_id_of_current_pane,
                &help_text,
                &fuzzy_help_text,
                &alphabet,
            )
            .await;

            let win = window.clone();
            win.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                let mux = Mux::get();
                if let Some(tab) = mux.get_tab(tab_id) {
                    let window = window.clone();
                    let (overlay, future) =
                        start_overlay(term_window, &tab, move |_tab_id, term| {
                            launcher(args, term, window, initial_choice_idx)
                        });

                    term_window.assign_overlay(tab_id, overlay);
                    promise::spawn::spawn(future).detach();
                }
            })));
        })
        .detach();
    }

    /// Returns the Prompt semantic zones
    fn get_semantic_prompt_zones(&mut self, pane: &Arc<dyn Pane>) -> &[StableRowIndex] {
        let cache = self
            .semantic_zones
            .entry(pane.pane_id())
            .or_insert_with(SemanticZoneCache::default);

        let seqno = pane.get_current_seqno();
        if cache.seqno != seqno {
            let zones = pane.get_semantic_zones().unwrap_or_else(|_| vec![]);
            let mut zones: Vec<StableRowIndex> = zones
                .into_iter()
                .filter_map(|zone| {
                    if zone.semantic_type == wezterm_term::SemanticType::Prompt {
                        Some(zone.start_y)
                    } else {
                        None
                    }
                })
                .collect();
            // dedup to avoid issues where both left and right prompts are
            // defined: we only care if there were 1+ prompts on a line,
            // not about how many prompts are on a line.
            // <https://github.com/wezterm/wezterm/issues/1121>
            zones.dedup();
            cache.zones = zones;
            cache.seqno = seqno;
        }
        &cache.zones
    }

    fn scroll_to_prompt(&mut self, amount: isize, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top);
        let zone = {
            let zones = self.get_semantic_prompt_zones(&pane);
            let idx = match zones.binary_search(&position) {
                Ok(idx) | Err(idx) => idx,
            };
            let idx = ((idx as isize) + amount).max(0) as usize;
            zones.get(idx).cloned()
        };
        if let Some(zone) = zone {
            self.set_viewport(pane.pane_id(), Some(zone), dims);
        }
        Ok(())
    }

    fn scroll_by_page(&mut self, amount: f64, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top) as f64
            + (amount * dims.viewport_rows as f64);
        self.set_viewport(pane.pane_id(), Some(position as isize), dims);
        Ok(())
    }

    fn scroll_by_current_event_wheel_delta(&mut self, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        if let Some(event) = &self.current_mouse_event {
            let amount = match event.kind {
                MouseEventKind::VertWheel(amount) => -amount,
                _ => return Ok(()),
            };
            self.scroll_by_line(amount.into(), pane)?;
        }
        Ok(())
    }

    fn scroll_by_line(&mut self, amount: isize, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let dims = pane.get_dimensions();
        let position = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            .saturating_add(amount);
        self.set_viewport(pane.pane_id(), Some(position), dims);
        Ok(())
    }

    fn move_tab_relative(&mut self, delta: isize) -> anyhow::Result<()> {
        let mux = Mux::get();
        let window = mux
            .get_window(self.mux_window_id)
            .ok_or_else(|| anyhow!("no such window"))?;

        let max = window.len();
        ensure!(max > 0, "no more tabs");

        let active = window.get_active_idx();
        let tab = active as isize + delta;
        let tab = if tab < 0 {
            0usize
        } else if tab >= max as isize {
            max - 1
        } else {
            tab as usize
        };

        drop(window);
        self.move_tab(tab)
    }

    fn file_browser_cwd(&self, pane: &Arc<dyn Pane>) -> Option<PathBuf> {
        pane.get_current_working_dir(CachePolicy::AllowStale)
            .and_then(|url| url.to_file_path().ok())
    }

    fn parse_ssh_destination_from_argv(argv: &[String]) -> Option<(String, Option<u16>)> {
        if !Self::is_ssh_client_argv(argv) {
            return None;
        }

        let is_mosh = argv.first().is_some_and(|arg| {
            Path::new(arg)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("mosh"))
        });
        let mut iter = argv.iter().skip(1);
        let mut port: Option<u16> = None;
        let mut user: Option<String> = None;
        let mut after_options = false;
        while let Some(arg) = iter.next() {
            if after_options {
                if arg.is_empty() {
                    return None;
                }
                return Some((
                    if let Some(user) = user {
                        format!("{user}@{arg}")
                    } else {
                        arg.clone()
                    },
                    port,
                ));
            }
            if arg == "--" {
                after_options = true;
                continue;
            }
            if arg == "-p" {
                if let Some(p) = iter.next() {
                    if !is_mosh {
                        if let Ok(n) = p.parse::<u16>() {
                            port = Some(n);
                        }
                    }
                    continue;
                }
            }
            if arg.starts_with("-p") && arg.len() > 2 {
                if !is_mosh {
                    if let Ok(n) = arg[2..].parse::<u16>() {
                        port = Some(n);
                    }
                }
                continue;
            }
            if arg == "-l" {
                user = iter.next().filter(|user| !user.is_empty()).cloned();
                continue;
            }
            if let Some(value) = arg.strip_prefix("-l") {
                if !value.is_empty() {
                    user = Some(value.to_string());
                    continue;
                }
            }
            if arg.starts_with("-o") {
                let rest = if arg == "-o" {
                    iter.next().map(String::as_str).unwrap_or_default()
                } else {
                    &arg[2..]
                };
                if let Some((key, value)) = rest.split_once('=') {
                    match key.trim().to_ascii_lowercase().as_str() {
                        "port" => {
                            if let Ok(n) = value.trim().parse::<u16>() {
                                port = Some(n);
                            }
                        }
                        "user" if !value.trim().is_empty() => {
                            user = Some(value.trim().to_string());
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if arg.starts_with('-') {
                if matches!(
                    arg.as_str(),
                    "-B" | "-c"
                        | "-D"
                        | "-E"
                        | "-F"
                        | "-I"
                        | "-i"
                        | "-J"
                        | "-L"
                        | "-m"
                        | "-O"
                        | "-Q"
                        | "-R"
                        | "-S"
                        | "-W"
                        | "-w"
                ) {
                    iter.next();
                }
                continue;
            }
            return Some((
                if let Some(user) = user {
                    format!("{user}@{arg}")
                } else {
                    arg.clone()
                },
                port,
            ));
        }
        None
    }

    fn is_ssh_client_argv(argv: &[String]) -> bool {
        let Some(executable) = argv
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .and_then(|name| name.to_str())
        else {
            return false;
        };
        matches!(executable.to_ascii_lowercase().as_str(), "ssh" | "mosh")
    }

    fn local_hostname_label() -> Option<String> {
        hostname::get().ok().and_then(|h| {
            h.to_str()
                .map(|s| s.split('.').next().unwrap_or(s).to_ascii_lowercase())
        })
    }

    fn host_looks_remote(url_host: &str, local_label: Option<&str>) -> bool {
        let host = url_host.trim();
        if host.is_empty() {
            return false;
        }
        let first = host.split('.').next().unwrap_or(host).to_ascii_lowercase();
        if matches!(first.as_str(), "localhost" | "127.0.0.1" | "::1" | "") {
            return false;
        }
        // host with the ip forms won't split on '.' meaningfully; guard full forms too
        if matches!(host, "127.0.0.1" | "::1") {
            return false;
        }
        match local_label {
            Some(local) => first != local,
            None => true,
        }
    }

    /// Every signal that says "this pane is not running on this machine",
    /// gathered in one pass.
    ///
    /// Collecting them together matters: `get_foreground_process_info` is
    /// fetched with `FetchImmediate` and is the expensive part, so callers that
    /// need both the verdict and the individual signals must not pay for it
    /// twice. `file_browser_remote_context` and the agent launcher both go
    /// through here so there is exactly one definition of "remote".
    fn pane_remote_signals(&self, pane: &Arc<dyn Pane>) -> PaneRemoteSignals {
        let working_dir_url = pane.get_current_working_dir(CachePolicy::AllowStale);
        let working_dir_path = working_dir_url.as_ref().and_then(|url| {
            percent_decode_str(url.path())
                .decode_utf8()
                .ok()
                .map(|path| path.into_owned())
                .filter(|path| !path.is_empty())
        });

        // A single fresh fetch of the foreground process info; AllowStale can
        // return an empty argv on macOS (KERN_PROCARGS2 failures), which used
        // to cause the ssh-argv signal below to silently disappear.
        let fg_argv = pane
            .get_foreground_process_info(CachePolicy::FetchImmediate)
            .map(|info| info.argv);

        let name_is_ssh = matches!(
            self.pane_command_basename(pane).as_deref(),
            Some("ssh" | "mosh")
        );
        let argv_is_ssh = fg_argv
            .as_ref()
            .is_some_and(|argv| Self::is_ssh_client_argv(argv));

        let ssh_scheme = working_dir_url
            .as_ref()
            .is_some_and(|url| url.scheme() == "ssh");

        let remote_ssh_domain = Mux::get()
            .get_domain(pane.domain_id())
            .is_some_and(|domain| domain.downcast_ref::<mux::ssh::RemoteSshDomain>().is_some());

        let local_label = Self::local_hostname_label();
        let osc7_remote_host = working_dir_url.as_ref().is_some_and(|url| {
            url.scheme() == "file"
                && url
                    .host_str()
                    .filter(|host| !host.is_empty())
                    .is_some_and(|host| Self::host_looks_remote(host, local_label.as_deref()))
        });

        PaneRemoteSignals {
            working_dir_url,
            working_dir_path,
            fg_argv,
            name_is_ssh,
            argv_is_ssh,
            ssh_scheme,
            remote_ssh_domain,
            osc7_remote_host,
        }
    }

    /// True when the pane appears to be a session on another machine.
    pub fn pane_looks_remote(&self, pane: &Arc<dyn Pane>) -> bool {
        self.pane_remote_signals(pane).looks_remote()
    }

    /// Working directory of the most recently active pane that is running on
    /// this machine, searched across every tab in this window.
    ///
    /// Used when the launcher is forced local from a remote pane: the remote
    /// pane's cwd is meaningless here, so fall back to wherever the user last
    /// was locally. `None` means there is no local pane to borrow from and the
    /// caller should pick its own fallback.
    pub fn newest_local_pane_cwd(&self) -> Option<PathBuf> {
        let mux = Mux::get();
        let window = mux.get_window(self.mux_window_id)?;

        // Active tab first, then the rest, so the answer tracks where the user
        // most plausibly just was.
        let active_idx = window.get_active_idx();
        let tabs: Vec<_> = window.iter().cloned().collect();
        let ordered = std::iter::once(active_idx)
            .chain((0..tabs.len()).filter(|idx| *idx != active_idx))
            .filter_map(|idx| tabs.get(idx));

        for tab in ordered {
            let panes = tab.iter_panes_ignoring_zoom();
            let active_pane_idx = panes.iter().position(|pos| pos.is_active).unwrap_or(0);
            let pane_order = std::iter::once(active_pane_idx)
                .chain((0..panes.len()).filter(|idx| *idx != active_pane_idx))
                .filter_map(|idx| panes.get(idx));

            for pos in pane_order {
                let signals = self.pane_remote_signals(&pos.pane);
                if signals.looks_remote() {
                    continue;
                }
                if let Some(path) = signals.working_dir_path.as_ref() {
                    return Some(PathBuf::from(
                        crate::termwindow::composer::normalize_cwd_path(path.clone()),
                    ));
                }
            }
        }

        None
    }

    fn file_browser_remote_context(
        &self,
        pane: &Arc<dyn Pane>,
    ) -> (Option<RemoteFileBrowserContext>, bool) {
        let signals = self.pane_remote_signals(pane);
        let pane_looks_remote = signals.looks_remote();
        let PaneRemoteSignals {
            working_dir_url,
            working_dir_path,
            fg_argv,
            name_is_ssh,
            argv_is_ssh,
            ssh_scheme,
            remote_ssh_domain: _,
            osc7_remote_host,
        } = signals;
        let mux = Mux::get();

        // 1. ssh:// working-directory URL (e.g. reported via OSC 7 by a remote shell).
        if let Some(url) = working_dir_url.as_ref() {
            if url.scheme() == "ssh" {
                if let Some(host) = url.host_str().filter(|host| !host.is_empty()) {
                    let destination = if url.username().is_empty() {
                        host.to_string()
                    } else {
                        format!("{}@{}", url.username(), host)
                    };
                    return (
                        Some(RemoteFileBrowserContext {
                            destination,
                            port: url.port(),
                            path: working_dir_path.clone().unwrap_or_else(|| "~".to_string()),
                        }),
                        pane_looks_remote,
                    );
                }
            }
        }

        // 2. `ssh`/`mosh` invocation visible in the foreground process argv.
        if argv_is_ssh {
            if let Some(argv) = fg_argv.as_ref() {
                if let Some((destination, port)) = Self::parse_ssh_destination_from_argv(argv) {
                    return (
                        Some(RemoteFileBrowserContext {
                            destination,
                            port,
                            path: working_dir_path.clone().unwrap_or_else(|| "~".to_string()),
                        }),
                        pane_looks_remote,
                    );
                }
            }
        }

        // 3. OSC-7 file:// URL pointing at a host that isn't this machine, but
        //    only when there is corroborating ssh evidence (process name,
        //    argv, or ssh:// scheme) so we don't misclassify local panes that
        //    merely report an unfamiliar hostname.
        if osc7_remote_host && (name_is_ssh || argv_is_ssh || ssh_scheme) {
            if let Some(host) = working_dir_url.as_ref().and_then(|url| url.host_str()) {
                return (
                    Some(RemoteFileBrowserContext {
                        destination: host.to_string(),
                        port: None,
                        path: working_dir_path.clone().unwrap_or_else(|| "~".to_string()),
                    }),
                    pane_looks_remote,
                );
            }
        }

        // 4. Domain-level ssh connection descriptor.
        if let Some(domain) = mux.get_domain(pane.domain_id()) {
            if let Some(ssh_dom) = domain.downcast_ref::<mux::ssh::RemoteSshDomain>() {
                if let Some(descriptor) = ssh_dom.ssh_connection_descriptor() {
                    return (
                        Some(RemoteFileBrowserContext {
                            destination: descriptor.destination,
                            port: descriptor.port,
                            path: working_dir_path.unwrap_or_else(|| "~".to_string()),
                        }),
                        pane_looks_remote,
                    );
                }
            }
        }

        (None, pane_looks_remote)
    }

    fn file_browser_script(&self) -> String {
        r#"set -e
umask 077
printf '\033]0;Worktree\007'
printf '\033]1337;SetUserVar=tgzterminal.worktree=MQ==\007'

tmp="${TMPDIR:-/tmp}/tgzterminal-worktree.$$"
trap 'rm -f "$tmp" "$tmp".*' EXIT
target_pane="${TGZTERMINAL_TARGET_PANE:-}"
wezterm_bin="${TGZTERMINAL_BIN:-tgzterminal}"
fzf_bin="${TGZTERMINAL_FZF_BIN:-fzf}"
editor_cmd="${TGZTERMINAL_EDITOR_COMMAND:-${VISUAL:-${EDITOR:-vim}}}"
remote_dest="${TGZTERMINAL_REMOTE_DEST:-}"
remote_port="${TGZTERMINAL_REMOTE_PORT:-}"
remote_cwd="${TGZTERMINAL_REMOTE_CWD:-}"
remote_domain_id="${TGZTERMINAL_REMOTE_DOMAIN_ID:-}"
cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/tgzterminal"
mkdir -p "$cache_dir" 2>/dev/null || true
connection_key=$(printf '%s:%s' "$remote_dest" "$remote_port" | cksum | awk '{ print $1 }')
control_path="$cache_dir/worktree-connection-$connection_key.sock"

quote_path() {
  printf "'"
  printf "%s" "$1" | sed "s/'/'\\\\''/g"
  printf "'"
}

is_remote() {
  [ -n "$remote_dest" ]
}

# Local-open command line: honors TGZTERMINAL_EDITOR_COMMAND/$VISUAL/$EDITOR
# when the user configured one, otherwise hands off to the platform's default
# file handler instead of assuming `vim` is installed (it usually isn't on a
# native Windows shell or a bare WSL image). $1 = already shell-quoted path,
# $2 = raw path.
local_editor_invocation() {
  if [ -n "${TGZTERMINAL_EDITOR_COMMAND:-}" ] || [ -n "${VISUAL:-}" ] || [ -n "${EDITOR:-}" ]; then
    printf '%s %s' "$editor_cmd" "$1"
    return 0
  fi
  if [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/version 2>/dev/null; then
    winpath=$(wslpath -w "$2" 2>/dev/null) || winpath="$2"
    printf 'explorer.exe %s' "$(quote_path "$winpath")"
    return 0
  fi
  if [ -n "${MSYSTEM:-}" ]; then
    printf 'cmd.exe /c start "" %s' "$1"
    return 0
  fi
  printf '%s %s' "$editor_cmd" "$1"
}

# --- SSH connection strategy -------------------------------------------------
# The worktree browser must never force a second interactive login when the
# terminal already has a working connection to the host. The strategy is
# resolved once per open and cached in $ssh_mode:
#   shared -> reuse the user's OWN connection with ZERO prompts. Attaches to an
#             existing OpenSSH ControlMaster socket (the one the interactive ssh
#             in the terminal created, when multiplexing is enabled) or succeeds
#             via key/agent auth. BatchMode=yes guarantees it never prompts and
#             fails fast when no reusable connection exists.
#   owned  -> nothing was reusable, so we stand up our OWN persistent master
#             exactly once, INTERACTIVELY, so password/2FA/host-key prompts work
#             (the old code ran every probe non-interactively, so any prompt
#             turned into an instant "Unable to connect"). ControlPersist keeps
#             it warm for 600s so later probes reuse it with no further prompts.
#
# ssh_probe_failed latches after a failed resolve so the loop never re-launches
# the interactive login on its own — otherwise a host that cannot authenticate
# non-interactively re-prompts for a password every 2 seconds forever. It is
# cleared only when the user explicitly asks to retry.
ssh_mode=""
ssh_probe_failed=0
ssh_last_error=""

ssh_shared() {
  # Zero-prompt attempt: reuse the user's existing connection/master, or
  # passwordless key/agent auth. Never prompts; fails fast otherwise.
  if [ -n "$remote_port" ]; then
    ssh -o BatchMode=yes -o ConnectTimeout=5 -p "$remote_port" "$remote_dest" "$@"
  else
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$remote_dest" "$@"
  fi
}

ssh_owned() {
  # Over our own persistent control socket (established by ensure_owned_master).
  if [ -n "$remote_port" ]; then
    ssh -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=5 -p "$remote_port" "$remote_dest" "$@"
  else
    ssh -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=5 "$remote_dest" "$@"
  fi
}

owned_master_alive() {
  if [ -n "$remote_port" ]; then
    ssh -O check -o ControlPath="$control_path" -p "$remote_port" "$remote_dest" >/dev/null 2>&1
  else
    ssh -O check -o ControlPath="$control_path" "$remote_dest" >/dev/null 2>&1
  fi
}

ensure_owned_master() {
  # One-time interactive login that leaves a backgrounded ControlPersist master.
  # Runs against the controlling terminal so ssh can prompt for a password, 2FA
  # code, or host-key confirmation even while our stdout is being captured.
  owned_master_alive && return 0
  login_rc=0
  if [ -n "$remote_port" ]; then
    ssh -t -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=20 -p "$remote_port" "$remote_dest" true </dev/tty >/dev/tty 2>/dev/tty || login_rc=$?
  else
    ssh -t -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=20 "$remote_dest" true </dev/tty >/dev/tty 2>/dev/tty || login_rc=$?
  fi
  if owned_master_alive; then
    ssh_last_error=""
    return 0
  fi
  # Distinguish auth/connection failure from "logged in, but multiplexing never
  # came up" so the retry prompt can tell the user which one they hit.
  if [ "$login_rc" -ne 0 ]; then
    ssh_last_error="SSH login to $remote_dest failed (exit $login_rc). See the messages above."
  else
    ssh_last_error="Logged in to $remote_dest, but SSH connection sharing (ControlMaster) did not start. The host may disable multiplexing, or $control_path is not writable."
  fi
  return 1
}

resolve_ssh_mode() {
  [ -z "$ssh_mode" ] || return 0
  # Latched failure: never silently re-launch the interactive login. The caller
  # (retry_connection) clears this flag when the user asks to try again.
  [ "$ssh_probe_failed" -eq 1 ] && return 1
  # 0. wezterm SSH domain: wezterm already holds the authenticated session, so
  #    list over it via the CLI verb with ZERO re-auth. Probe once; if the
  #    session is gone, fall through to the ssh transport below.
  if [ -n "$remote_domain_id" ]; then
    if printf 'true' | "$wezterm_bin" cli worktree-exec --domain-id "$remote_domain_id" >/dev/null 2>&1; then
      ssh_mode=domain
      return 0
    fi
  fi
  # 1. Reuse the terminal's connection with no prompt if we possibly can.
  if [ -n "$remote_dest" ] && ssh_shared true >/dev/null 2>&1; then
    ssh_mode=shared
    return 0
  fi
  # 2. Otherwise stand up our own master, authenticating interactively once.
  #    Capture stderr so a real ssh failure (bad key, host-key mismatch,
  #    multiplexing disabled) can be shown instead of a generic message.
  if [ -n "$remote_dest" ]; then
    if ensure_owned_master; then
      ssh_mode=owned
      return 0
    fi
  fi
  ssh_probe_failed=1
  return 1
}

ssh_remote() {
  case "$ssh_mode" in
    domain) printf '%s' "$*" | "$wezterm_bin" cli worktree-exec --domain-id "$remote_domain_id" ;;
    shared) ssh_shared "$@" ;;
    owned) ssh_owned "$@" ;;
    *) return 1 ;;
  esac
}

remote_eval() {
  # Quote the complete command once so paths containing shell metacharacters
  # cannot change the remote command structure.
  ssh_remote "sh -lc $(quote_path "$1")"
}

# Awk formatters below run on ALREADY-FETCHED raw listing data so the same
# formatting works for both the local path and the single remote round trip.
format_git_index() {
  awk -v root="$root" '
      function base(path, parts, n) {
        n = split(path, parts, "/")
        return parts[n]
      }
      function indent(depth, s, i) {
        s = ""
        for (i = 0; i < depth; i++) {
          s = s "  "
        }
        return s
      }
      function emit_dir(rel, depth, abs, label) {
        if (rel == "" || seen_dir[rel]++) {
          return
        }
        abs = root "/" rel
        label = indent(depth) base(rel) "/"
        print label "\t" abs "\td"
      }
      BEGIN {
        print base(root) "/\t" root "\td"
      }
      {
        file = $0
        if (file == "") {
          next
        }
        n = split(file, parts, "/")
        rel = ""
        for (i = 1; i < n; i++) {
          rel = rel == "" ? parts[i] : rel "/" parts[i]
          emit_dir(rel, i - 1)
        }
        print indent(n - 1) parts[n] "\t" root "/" file "\tf"
      }
    ' \
    | sort -t "$(printf '\t')" -k2,2 -k3,3 -u
}

format_find_index() {
  awk -F '\t' -v root="$root" '
      function base(path, parts, n) {
        n = split(path, parts, "/")
        return parts[n]
      }
      function indent(depth, s, i) {
        s = ""
        for (i = 0; i < depth; i++) {
          s = s "  "
        }
        return s
      }
      {
        kind = $1
        path = $2
        rel = path
        sub("^" root "/?", "", rel)
        if (path == root || rel == "") {
          print base(root) "/\t" root "\td"
        } else {
          depth = split(rel, parts, "/") - 1
          suffix = kind == "d" ? "/" : ""
          print indent(depth) parts[depth + 1] suffix "\t" path "\t" kind
        }
      }
    ' \
    | sort -t "$(printf '\t')" -k2,2 -k3,3 -u
}

# --- Local-only stamp helpers (the remote side computes its stamp itself). ---
local_path_mtime() {
  path="$1"
  stat -f %m "$path" 2>/dev/null || stat -c %Y "$path" 2>/dev/null || printf 0
}

local_git_index_path() {
  git_dir=$(git -C "$root" rev-parse --git-dir 2>/dev/null || true)
  case "$git_dir" in
    '') return 1 ;;
    /*) printf '%s/index\n' "$git_dir" ;;
    *) printf '%s/%s/index\n' "$root" "$git_dir" ;;
  esac
}

local_stamp() {
  git_head=$(git -C "$root" rev-parse HEAD 2>/dev/null || printf 'nogit')
  git_index=$(local_git_index_path || true)
  index_mtime=$(local_path_mtime "$git_index")
  root_mtime=$(local_path_mtime "$root")
  printf '%s:%s:%s:%s\n' "$root" "$git_head" "$index_mtime" "$root_mtime"
}

# --- Single remote round trip -------------------------------------------------
# One remote program resolves the git/work-tree root, computes the cache stamp,
# and (for a full fetch) emits the raw listing. Sections are delimited by
# sentinel lines parsed locally: __TGZ_ROOT__, __TGZ_STAMP__, and either
# __TGZ_LIST_GIT__ or __TGZ_LIST_FIND__. The whole program is passed as a single
# quoted `sh -lc` argument (see remote_eval), so paths stay safe.
# The program bodies are written to temp files via file-redirect heredocs.
# (A here-doc INSIDE $(...) is miscompiled by bash 3.2, still the /bin/sh on
# macOS, so we avoid that construct entirely.)
meta_prog="$tmp.metaprog"
list_prog="$tmp.listprog"
cat > "$meta_prog" <<'REMOTE_META'
root=$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)
cd -- "$root" 2>/dev/null || true
head=$(git rev-parse HEAD 2>/dev/null || printf 'nogit')
gitdir=$(git rev-parse --git-dir 2>/dev/null || true)
case "$gitdir" in
  '') index='' ;;
  /*) index="$gitdir/index" ;;
  *) index="$root/$gitdir/index" ;;
esac
imt=$(stat -c %Y "$index" 2>/dev/null || stat -f %m "$index" 2>/dev/null || printf 0)
rmt=$(stat -c %Y "$root" 2>/dev/null || stat -f %m "$root" 2>/dev/null || printf 0)
printf '__TGZ_ROOT__\n%s\n' "$root"
printf '__TGZ_STAMP__\n%s:%s:%s:%s\n' "$root" "$head" "$imt" "$rmt"
REMOTE_META
cat > "$list_prog" <<'REMOTE_LIST'
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  printf '__TGZ_LIST_GIT__\n'
  git ls-files -z --cached --others --exclude-standard 2>/dev/null | tr '\0' '\n'
else
  printf '__TGZ_LIST_FIND__\n'
  find "$root" -maxdepth 3 \( -name .git -o -name target -o -name node_modules -o -name .venv -o -name .pytest_cache -o -name .ruff_cache -o -name Library -o -name .cache -o -name .Trash -o -name .npm -o -name .cargo -o -name .rustup -o -name .gradle -o -name .m2 \) -prune -o \( -type d -o -type f \) -exec sh -c 'for path do if [ -d "$path" ]; then printf "d\t%s\n" "$path"; else printf "f\t%s\n" "$path"; fi; done' sh {} + 2>/dev/null
fi
REMOTE_LIST

remote_probe() {
  # $1 = stamp|full. One ssh exec over the persisted control socket.
  quoted_start=$(quote_path "$start_dir")
  {
    printf 'cd -- %s 2>/dev/null || exit 1\n' "$quoted_start"
    cat "$meta_prog"
    if [ "$1" = full ]; then
      cat "$list_prog"
    fi
  } > "$tmp.prog"
  remote_eval "$(cat "$tmp.prog")"
}

set_cache_paths() {
  cache_key=$(printf '%s:%s:%s' "$remote_dest" "$remote_port" "$root" | cksum | awk '{ print $1 }')
  cache_file="$cache_dir/worktree-$cache_key.tsv"
  stamp_file="$cache_dir/worktree-$cache_key.stamp"
  check_file="$cache_dir/worktree-$cache_key.checked"
}

parse_probe_meta() {
  root=$(awk '/^__TGZ_ROOT__$/ { getline; print; exit }' "$1")
  stamp=$(awk '/^__TGZ_STAMP__$/ { getline; print; exit }' "$1")
  [ -n "$root" ] || root="${remote_cwd:-.}"
  set_cache_paths
  # Persist the resolved root per connection so the cache key is known before
  # any ssh call on the next open (enables the zero-exec instant first paint).
  printf '%s\n' "$root" > "$root_hint_file" 2>/dev/null || true
}

build_from_probe() {
  probe="$1"
  next_tmp="$tmp.next"
  raw="$tmp.raw"
  if grep -q '^__TGZ_LIST_GIT__$' "$probe"; then
    awk 'f { print } /^__TGZ_LIST_GIT__$/ { f = 1 }' "$probe" > "$raw"
    format_git_index < "$raw" > "$next_tmp" || {
      rm -f "$raw" "$next_tmp"
      return 1
    }
  elif grep -q '^__TGZ_LIST_FIND__$' "$probe"; then
    awk 'f { print } /^__TGZ_LIST_FIND__$/ { f = 1 }' "$probe" > "$raw"
    format_find_index < "$raw" > "$next_tmp" || {
      rm -f "$raw" "$next_tmp"
      return 1
    }
  else
    rm -f "$raw"
    return 1
  fi
  rm -f "$raw"
  mv "$next_tmp" "$tmp"
  cp "$tmp" "$cache_file" 2>/dev/null || true
  printf '%s\n' "$stamp" > "$stamp_file" 2>/dev/null || true
}

build_list_remote() {
  now=$(date +%s)
  probe="$tmp.probe"
  if ! resolve_ssh_mode; then
    if [ -n "$ssh_last_error" ]; then
      printf 'Unable to connect to %s: %s\n' "$remote_dest" "$ssh_last_error" >&2
    else
      printf 'Unable to connect to %s.\n' "$remote_dest" >&2
    fi
    return 1
  fi
  if [ -n "${cache_file:-}" ] && [ -s "$cache_file" ] && [ -r "$stamp_file" ]; then
    # Warm path: a lightweight stamp-only probe; reuse the cache if unchanged.
    remote_probe stamp > "$probe" 2>/dev/null || {
      rm -f "$probe"
      printf 'Unable to connect to %s.\n' "$remote_dest" >&2
      return 1
    }
    parse_probe_meta "$probe"
    printf '%s\n' "$now" > "$check_file" 2>/dev/null || true
    if [ -s "$cache_file" ] && [ "$(cat "$stamp_file" 2>/dev/null)" = "$stamp" ]; then
      rm -f "$probe"
      cp "$cache_file" "$tmp" 2>/dev/null && return 0
    fi
    rm -f "$probe"
  fi

  # Cold or changed: one full exec returns root + stamp + raw list together.
  remote_probe full > "$probe" 2>/dev/null || {
    rm -f "$probe"
    printf 'Unable to connect to %s.\n' "$remote_dest" >&2
    return 1
  }
  parse_probe_meta "$probe"
  printf '%s\n' "$now" > "$check_file" 2>/dev/null || true
  build_from_probe "$probe" || {
    rm -f "$probe"
    return 1
  }
  rm -f "$probe"
}

build_list_local() {
  now=$(date +%s)
  stamp=$(local_stamp) || return 1
  printf '%s\n' "$now" > "$check_file" 2>/dev/null || true
  if [ -s "$cache_file" ] && [ -r "$stamp_file" ] && [ "$(cat "$stamp_file" 2>/dev/null)" = "$stamp" ]; then
    cp "$cache_file" "$tmp" 2>/dev/null && return 0
  fi

  next_tmp="$tmp.next"
  quoted_root=$(quote_path "$root")
  find_script="find $quoted_root -maxdepth 3 \\( -name .git -o -name target -o -name node_modules -o -name .venv -o -name .pytest_cache -o -name .ruff_cache -o -name Library -o -name .cache -o -name .Trash -o -name .npm -o -name .cargo -o -name .rustup -o -name .gradle -o -name .m2 \\) -prune -o \\( -type d -o -type f \\) -exec sh -c 'for path do if [ -d \"\$path\" ]; then printf \"d\\t%s\\n\" \"\$path\"; else printf \"f\\t%s\\n\" \"\$path\"; fi; done' sh {} +"
  if git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$root" ls-files -z --cached --others --exclude-standard 2>/dev/null \
      | tr '\0' '\n' \
      | format_git_index > "$next_tmp" || {
      rm -f "$next_tmp"
      return 1
    }
  else
    eval "$find_script" 2>/dev/null | format_find_index > "$next_tmp" || {
      rm -f "$next_tmp"
      return 1
    }
  fi

  mv "$next_tmp" "$tmp"
  cp "$tmp" "$cache_file" 2>/dev/null || true
  printf '%s\n' "$stamp" > "$stamp_file" 2>/dev/null || true
}

build_list() {
  now=$(date +%s)
  # Throttle revalidation: reuse the cache when checked within the last 2s.
  if [ -n "${cache_file:-}" ] && [ -r "$cache_file" ] && [ -r "$check_file" ]; then
    checked=$(cat "$check_file" 2>/dev/null || printf 0)
    case "$checked" in
      ''|*[!0-9]*) checked=0 ;;
    esac
    if [ "$now" -le $((checked + 2)) ]; then
      cp "$cache_file" "$tmp" 2>/dev/null && return 0
    fi
  fi

  if is_remote; then
    build_list_remote
  else
    build_list_local
  fi
}

# Resolve cache paths up front WITHOUT any ssh so a cached list can paint
# instantly. For remote we reuse the root resolved by a previous open.
if is_remote; then
  start_dir="${remote_cwd:-.}"
  root_hint_file="$cache_dir/worktree-connection-$connection_key.root"
  if [ -r "$root_hint_file" ]; then
    root=$(sed -n '1p' "$root_hint_file")
  fi
  if [ -n "${root:-}" ]; then
    set_cache_paths
  fi
else
  root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
  root=$(cd "$root" 2>/dev/null && pwd -P || pwd)
  set_cache_paths
fi

path_is_dir() {
  selection="$1"
  quoted=$(quote_path "$selection")
  if is_remote; then
    remote_eval "[ -d $quoted ]"
  else
    [ -d "$selection" ]
  fi
}

path_is_file() {
  selection="$1"
  quoted=$(quote_path "$selection")
  if is_remote; then
    remote_eval "[ -f $quoted ]"
  else
    [ -f "$selection" ]
  fi
}

send_folder() {
  selection="$1"
  [ -n "$target_pane" ] || return 0
  path_is_dir "$selection" || return 0
  quoted=$(quote_path "$selection")
  printf '\025cd -- %s\nclear\n' "$quoted" \
    | "$wezterm_bin" cli send-text --pane-id "$target_pane" --no-paste >/dev/null 2>&1 || return 0
  "$wezterm_bin" cli activate-pane --pane-id "$target_pane" >/dev/null 2>&1 || true
}

open_file() {
  selection="$1"
  [ -n "$target_pane" ] || return 0
  path_is_file "$selection" || return 0
  quoted=$(quote_path "$selection")
  if is_remote; then
    # Run the editor ON the remote host, reusing the same connection strategy
    # resolved for the listing so opening a file never triggers a second login.
    resolve_ssh_mode || return 0
    if [ "$ssh_mode" = domain ]; then
      # Open the editor natively in the ssh domain: the split-pane inherits the
      # target pane's domain, reusing the held session via request_pty. No shell
      # interprets the path here, so pass the raw selection (not the quoted form).
      "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 -- \
        $editor_cmd "$selection" >/dev/null 2>&1 || return 0
    elif [ "$ssh_mode" = owned ]; then
      # Reuse our own persistent master.
      if [ -n "$remote_port" ]; then
        "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 -- \
          ssh -t -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=5 -p "$remote_port" "$remote_dest" "$editor_cmd $quoted" >/dev/null 2>&1 || return 0
      else
        "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 -- \
          ssh -t -o ControlMaster=auto -o ControlPersist=600s -o ControlPath="$control_path" -o ConnectTimeout=5 "$remote_dest" "$editor_cmd $quoted" >/dev/null 2>&1 || return 0
      fi
    else
      # Reuse the user's own connection/master (shared mode); no ControlPath
      # override so ssh follows their config exactly as the terminal did.
      if [ -n "$remote_port" ]; then
        "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 -- \
          ssh -t -o ConnectTimeout=5 -p "$remote_port" "$remote_dest" "$editor_cmd $quoted" >/dev/null 2>&1 || return 0
      else
        "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 -- \
          ssh -t -o ConnectTimeout=5 "$remote_dest" "$editor_cmd $quoted" >/dev/null 2>&1 || return 0
      fi
    fi
  else
    dir=$(dirname "$selection")
    cmdline=$(local_editor_invocation "$quoted" "$selection")
    "$wezterm_bin" cli split-pane --pane-id "$target_pane" --right --percent 50 --cwd "$dir" -- \
      sh -lc "printf '\033]0;Editor\007'; $cmdline || { printf '\\nFailed to launch editor\\n'; sleep 4; }" >/dev/null 2>&1 || return 0
  fi
}

close_worktree() {
  if [ -n "${WEZTERM_PANE:-}" ]; then
    "$wezterm_bin" cli kill-pane --pane-id "$WEZTERM_PANE" >/dev/null 2>&1 || true
  fi
  exit 0
}

# Instant first paint: if a cached list already exists for this connection and
# root, show it immediately with ZERO ssh execs, then revalidate on the next
# loop iteration.
instant_paint=0
if [ -n "${cache_file:-}" ] && [ -s "$cache_file" ] && cp "$cache_file" "$tmp" 2>/dev/null; then
  instant_paint=1
fi

skip_build=$instant_paint
while :; do
  if [ "$skip_build" -eq 1 ]; then
    skip_build=0
  else
    if is_remote && [ ! -s "$tmp" ]; then
      # Never leave the pane blank while ssh authenticates on a cold open.
      printf 'Connecting to %s...\n' "$remote_dest"
    fi
    # build_list reports its own connection error; never abort on failure.
    build_list || true
  fi

  if [ ! -s "$tmp" ]; then
    if is_remote; then
      # Remote listing failed. Do NOT spin: a bare `sleep; continue` loop would
      # re-run resolve_ssh_mode -> ensure_owned_master every 2s and re-prompt for
      # a password forever. Wait for an explicit choice instead. resolve_ssh_mode
      # has latched ssh_probe_failed, so nothing re-authenticates until 'r'.
      printf '\n'
      if [ -n "$ssh_last_error" ]; then
        printf 'Could not reach %s.\n  %s\n' "$remote_dest" "$ssh_last_error"
      else
        printf 'Could not reach %s.\n' "$remote_dest"
      fi
      printf 'Press r to retry, q to quit: '
      ans=''
      IFS= read -r ans </dev/tty || close_worktree
      case "$ans" in
        r|R) ssh_mode=''; ssh_probe_failed=0; ssh_last_error=''; continue ;;
        q|Q) close_worktree ;;
        *) continue ;;
      esac
    else
      printf 'No folders found.\n'
      sleep 2
      continue
    fi
  fi

  if command -v "$fzf_bin" >/dev/null 2>&1; then
    selection_line=$("$fzf_bin" --height=100% --layout=reverse --no-sort --cycle --prompt='Worktree > ' --pointer='>' --marker='+' --border=none --bind="q:execute-silent($wezterm_bin cli kill-pane --pane-id ${WEZTERM_PANE:-})+abort,ctrl-r:execute-silent(rm -f $cache_file $stamp_file $check_file)+abort" --color='bg:-1,bg+:#444444,fg:#b8b8b8,fg+:#eeeeee,hl:#d86f8f,hl+:#f18fb0,pointer:#d86f8f,prompt:#8fb4d8,spinner:#d86f8f,info:#d8c06f,border:#555555' --delimiter="$(printf '\t')" --with-nth=1 < "$tmp") || {
      continue
    }
  else
    awk -F '\t' '{ print NR ": " $1 }' "$tmp"
    printf 'Worktree folder number: '
    IFS= read -r number
    case "$number" in
      ''|*[!0-9]*) continue ;;
    esac
    selection_line=$(sed -n "${number}p" "$tmp")
  fi

  selection=$(printf '%s\n' "$selection_line" | awk -F '\t' '{ print $2 }')
  kind=$(printf '%s\n' "$selection_line" | awk -F '\t' '{ print $3 }')
  if [ "$kind" = f ]; then
    open_file "$selection"
  else
    send_folder "$selection"
  fi
done
"#
        .to_string()
    }

    fn find_worktree_pane(&self) -> Option<PaneId> {
        let mux = Mux::get();
        let tab = mux.get_active_tab_for_window(self.mux_window_id)?;
        tab.iter_panes_ignoring_zoom().iter().find_map(|pos| {
            let pane = &pos.pane;
            let vars = pane.copy_user_vars();
            if vars.contains_key("tgzterminal.worktree") || pane.get_title() == "Worktree" {
                Some(pane.pane_id())
            } else {
                None
            }
        })
    }

    fn close_file_browser(&self, pane_id: PaneId) {
        Mux::get().remove_pane(pane_id);
    }

    fn pane_command_basename(&self, pane: &Arc<dyn Pane>) -> Option<String> {
        pane.get_foreground_process_name(CachePolicy::AllowStale)
            .and_then(|name| {
                Path::new(&name)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase())
            })
    }

    fn is_worktree_pane_for_file_browser(&self, pane: &Arc<dyn Pane>) -> bool {
        pane.copy_user_vars().contains_key("tgzterminal.worktree")
            || pane.get_title().trim() == "Worktree"
    }

    fn is_shell_pane_for_file_browser(&self, pane: &Arc<dyn Pane>) -> bool {
        matches!(
            self.pane_command_basename(pane).as_deref(),
            Some("bash" | "fish" | "nu" | "pwsh" | "powershell" | "sh" | "zsh")
        )
    }

    fn is_ssh_pane_for_file_browser(&self, pane: &Arc<dyn Pane>) -> bool {
        if matches!(
            self.pane_command_basename(pane).as_deref(),
            Some("ssh" | "mosh")
        ) {
            return true;
        }
        pane.get_foreground_process_info(CachePolicy::AllowStale)
            .is_some_and(|info| Self::is_ssh_client_argv(&info.argv))
    }

    fn file_browser_target_pane(&self, requested: &Arc<dyn Pane>) -> Arc<dyn Pane> {
        if !self.is_worktree_pane_for_file_browser(requested)
            && (self.is_shell_pane_for_file_browser(requested)
                || self.is_ssh_pane_for_file_browser(requested))
        {
            return Arc::clone(requested);
        }

        let mux = Mux::get();
        let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) else {
            return Arc::clone(requested);
        };

        // Prefer an ssh/mosh pane over a plain shell: it is more likely the
        // machine the user is actually working on.
        if let Some(ssh) = tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|pos| {
                !self.is_worktree_pane_for_file_browser(&pos.pane)
                    && self.is_ssh_pane_for_file_browser(&pos.pane)
            })
            .map(|pos| pos.pane.clone())
        {
            return ssh;
        }

        if let Some(shell) = tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|pos| {
                !self.is_worktree_pane_for_file_browser(&pos.pane)
                    && self.is_shell_pane_for_file_browser(&pos.pane)
            })
            .map(|pos| pos.pane.clone())
        {
            return shell;
        }

        // Last resort: any pane that is not a utility view.
        tab.iter_panes_ignoring_zoom()
            .iter()
            .find(|pos| !self.is_worktree_pane_for_file_browser(&pos.pane))
            .map(|pos| pos.pane.clone())
            .unwrap_or_else(|| Arc::clone(requested))
    }

    fn open_file_browser(&self, pane: &Arc<dyn Pane>) {
        if let Some(pane_id) = self.find_worktree_pane() {
            self.close_file_browser(pane_id);
            return;
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let split_size = self.config.file_browser.split_size_percent.clamp(5, 95);
        let target_pane = self.file_browser_target_pane(pane);
        let (remote_context, pane_looks_remote) = self.file_browser_remote_context(&target_pane);
        // If the pane appears to be a remote/ssh session but we couldn't
        // resolve a remote context, do not silently fall back to spawning
        // the browser locally. Bail out early so the user isn't confused.
        if pane_looks_remote && remote_context.is_none() {
            log::warn!("pane looks like a remote/ssh session but could not resolve destination; aborting worktree open");
            wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
                title: "Worktree unavailable".to_string(),
                message: "Could not resolve the SSH destination for this pane".to_string(),
                url: None,
                timeout: Some(Duration::from_millis(3000)),
            });
            return;
        }
        let mut set_environment_variables = HashMap::new();
        set_environment_variables.insert(
            "TGZTERMINAL_TARGET_PANE".to_string(),
            target_pane.pane_id().to_string(),
        );
        let bin = cli_bin_for_script(
            std::env::current_exe().ok().as_deref(),
            cfg!(windows),
            &|path| path.exists(),
        );
        set_environment_variables.insert("TGZTERMINAL_BIN".to_string(), bin);
        // Prefer the fzf binary vendored alongside the app (see
        // ci/fetch-fzf.sh + ci/build-macos-bundle.sh / the Windows release
        // workflow) over whatever may or may not be on $PATH, so the
        // worktree picker's fzf UI works out of the box on every platform.
        if let Some(fzf_bin) = std::env::current_exe().ok().and_then(|path| {
            let fzf_name = if cfg!(windows) { "fzf.exe" } else { "fzf" };
            let candidate = path.parent()?.join(fzf_name);
            candidate.exists().then_some(candidate)
        }) {
            set_environment_variables.insert(
                "TGZTERMINAL_FZF_BIN".to_string(),
                // Same POSIX-script quoting concern as TGZTERMINAL_BIN.
                shell_path(&fzf_bin, cfg!(windows)),
            );
        }
        if let Some(editor_command) = self
            .config
            .file_browser
            .editor_command
            .as_ref()
            .and_then(|args| shlex::try_join(args.iter().map(|arg| arg.as_str())).ok())
        {
            set_environment_variables
                .insert("TGZTERMINAL_EDITOR_COMMAND".to_string(), editor_command);
        }
        if let Some(remote) = remote_context.as_ref() {
            set_environment_variables.insert(
                "TGZTERMINAL_REMOTE_DEST".to_string(),
                remote.destination.clone(),
            );
            set_environment_variables
                .insert("TGZTERMINAL_REMOTE_CWD".to_string(), remote.path.clone());
            if let Some(port) = remote.port {
                set_environment_variables
                    .insert("TGZTERMINAL_REMOTE_PORT".to_string(), port.to_string());
            }
        }
        // If the target pane belongs to a wezterm SSH domain, wezterm already
        // holds the authenticated session. Hand the domain id to the script so
        // it lists over that live connection (via `cli worktree-exec`) with zero
        // re-auth, falling back to the ssh transport only if the session is gone.
        let target_domain_id = target_pane.domain_id();
        if Mux::get()
            .get_domain(target_domain_id)
            .is_some_and(|domain| domain.downcast_ref::<mux::ssh::RemoteSshDomain>().is_some())
        {
            set_environment_variables.insert(
                "TGZTERMINAL_REMOTE_DOMAIN_ID".to_string(),
                target_domain_id.to_string(),
            );
        }

        let spawn = SpawnCommand {
            label: Some("Worktree".to_string()),
            args: Some(vec![shell, "-lc".to_string(), self.file_browser_script()]),
            cwd: remote_context
                .is_none()
                .then(|| self.file_browser_cwd(&target_pane))
                .flatten(),
            set_environment_variables,
            domain: if remote_context.is_some() {
                config::keyassignment::SpawnTabDomain::DomainName("local".to_string())
            } else {
                config::keyassignment::SpawnTabDomain::CurrentPaneDomain
            },
            ..Default::default()
        };
        self.spawn_command(
            &spawn,
            SpawnWhere::SplitPane(SplitRequest {
                direction: SplitDirection::Horizontal,
                target_is_second: false,
                size: MuxSplitSize::Percent(split_size),
                top_level: false,
            }),
        );
    }

    pub fn perform_key_assignment(
        &mut self,
        pane: &Arc<dyn Pane>,
        assignment: &KeyAssignment,
    ) -> anyhow::Result<PerformAssignmentResult> {
        use KeyAssignment::*;

        if let Some(modal) = self.get_modal() {
            if modal.perform_assignment(assignment, self) {
                return Ok(PerformAssignmentResult::Handled);
            }
        }

        match pane.perform_assignment(assignment) {
            PerformAssignmentResult::Unhandled => {}
            result => return Ok(result),
        }

        let window = self.window.as_ref().map(|w| w.clone());

        match assignment {
            ActivateKeyTable {
                name,
                timeout_milliseconds,
                replace_current,
                one_shot,
                until_unknown,
                prevent_fallback,
            } => {
                anyhow::ensure!(
                    self.input_map.has_table(name),
                    "ActivateKeyTable: no key_table named {}",
                    name
                );
                self.key_table_state.activate(KeyTableArgs {
                    name,
                    timeout_milliseconds: *timeout_milliseconds,
                    replace_current: *replace_current,
                    one_shot: *one_shot,
                    until_unknown: *until_unknown,
                    prevent_fallback: *prevent_fallback,
                });
                self.update_title();
            }
            PopKeyTable => {
                self.key_table_state.pop();
                self.update_title();
            }
            ClearKeyTableStack => {
                self.key_table_state.clear_stack();
                self.update_title();
            }
            Multiple(actions) => {
                for a in actions {
                    self.perform_key_assignment(pane, a)?;
                }
            }
            SpawnTab(spawn_where) => {
                self.spawn_tab(spawn_where);
            }
            SpawnWindow => {
                self.spawn_command(&SpawnCommand::default(), SpawnWhere::NewWindow);
            }
            SpawnCommandInNewTab(spawn) => {
                self.spawn_command(spawn, SpawnWhere::NewTab);
            }
            SpawnCommandInNewWindow(spawn) => {
                self.spawn_command(spawn, SpawnWhere::NewWindow);
            }
            SplitHorizontal(spawn) => {
                log::trace!("SplitHorizontal {:?}", spawn);
                self.spawn_command(
                    spawn,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: SplitDirection::Horizontal,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    }),
                );
            }
            SplitVertical(spawn) => {
                log::trace!("SplitVertical {:?}", spawn);
                self.spawn_command(
                    spawn,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: SplitDirection::Vertical,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    }),
                );
            }
            ToggleFullScreen => {
                self.window.as_ref().unwrap().toggle_fullscreen();
            }
            ToggleAlwaysOnTop => {
                let window = self.window.clone().unwrap();
                let current_level = self.window_state.as_window_level();

                match current_level {
                    WindowLevel::AlwaysOnTop => {
                        window.set_window_level(WindowLevel::Normal);
                    }
                    WindowLevel::AlwaysOnBottom | WindowLevel::Normal => {
                        window.set_window_level(WindowLevel::AlwaysOnTop);
                    }
                }
            }
            ToggleAlwaysOnBottom => {
                let window = self.window.clone().unwrap();
                let current_level = self.window_state.as_window_level();

                match current_level {
                    WindowLevel::AlwaysOnBottom => {
                        window.set_window_level(WindowLevel::Normal);
                    }
                    WindowLevel::AlwaysOnTop | WindowLevel::Normal => {
                        window.set_window_level(WindowLevel::AlwaysOnBottom);
                    }
                }
            }
            SetWindowLevel(level) => {
                let window = self.window.clone().unwrap();
                window.set_window_level(level.clone());
            }
            CopyTo(dest) => {
                let text = self.selection_text(pane);
                self.copy_to_clipboard(*dest, text);
            }
            CopyTextTo { text, destination } => {
                self.copy_to_clipboard(*destination, text.clone());
            }
            PasteFrom(source) => {
                self.paste_from_clipboard(pane, *source);
            }
            ActivateTabRelative(n) => {
                self.activate_tab_relative(*n, true)?;
            }
            ActivateTabRelativeNoWrap(n) => {
                self.activate_tab_relative(*n, false)?;
            }
            ActivateLastTab => self.activate_last_tab()?,
            DecreaseFontSize => self.decrease_font_size(),
            IncreaseFontSize => self.increase_font_size(),
            ResetFontSize => self.reset_font_size(),
            ResetFontAndWindowSize => {
                if let Some(w) = window.as_ref() {
                    self.reset_font_and_window_size(&w)?
                }
            }
            ActivateTab(n) => {
                self.activate_tab(*n)?;
            }
            ActivateWindow(n) => {
                self.activate_window(*n)?;
            }
            ActivateWindowRelative(n) => {
                self.activate_window_relative(*n, true)?;
            }
            ActivateWindowRelativeNoWrap(n) => {
                self.activate_window_relative(*n, false)?;
            }
            SendString(s) => pane.writer().write_all(s.as_bytes())?,
            SendKey(key) => {
                use keyevent::Key;
                let mods = key.mods;
                if let Key::Code(key) = self.win_key_code_to_termwiz_key_code(
                    &key.key.resolve(self.config.key_map_preference),
                ) {
                    pane.key_down(key, mods)?;
                }
            }
            Hide => {
                if let Some(w) = window.as_ref() {
                    w.hide();
                }
            }
            Show => {
                if let Some(w) = window.as_ref() {
                    w.show();
                }
            }
            CloseCurrentTab { confirm } => self.close_current_tab(*confirm),
            CloseCurrentPane { confirm } => self.close_current_pane(*confirm),
            Nop | DisableDefaultAssignment => {}
            ReloadConfiguration => config::reload(),
            MoveTab(n) => self.move_tab(*n)?,
            MoveTabRelative(n) => self.move_tab_relative(*n)?,
            ScrollByPage(n) => self.scroll_by_page(**n, pane)?,
            ScrollByLine(n) => self.scroll_by_line(*n, pane)?,
            ScrollByCurrentEventWheelDelta => self.scroll_by_current_event_wheel_delta(pane)?,
            ScrollToPrompt(n) => self.scroll_to_prompt(*n, pane)?,
            ScrollToTop => self.scroll_to_top(pane),
            ScrollToBottom => self.scroll_to_bottom(pane),
            ShowTabNavigator => self.show_tab_navigator(),
            ShowDebugOverlay => self.show_debug_overlay(),
            ActivateAgentSection => self.activate_agent_section_nav(),
            CheckForUpdates => crate::update::check_for_updates_now(),
            ShowLauncher => self.show_launcher(),
            ShowLauncherArgs(args) => {
                let title = args.title.clone().unwrap_or("Launcher".to_string());
                let args = LauncherActionArgs {
                    title: Some(title),
                    flags: args.flags,
                    help_text: args.help_text.clone(),
                    fuzzy_help_text: args.fuzzy_help_text.clone(),
                    alphabet: args.alphabet.clone(),
                };
                self.show_launcher_impl(args, 0);
            }
            HideApplication => {
                let con = Connection::get().expect("call on gui thread");
                con.hide_application();
            }
            QuitApplication => {
                let mux = Mux::get();
                let config = &self.config;
                log::info!("QuitApplication over here (window)");

                match config.window_close_confirmation {
                    WindowCloseConfirmation::NeverPrompt => {
                        let con = Connection::get().expect("call on gui thread");
                        con.terminate_message_loop();
                    }
                    WindowCloseConfirmation::AlwaysPrompt => {
                        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                            Some(tab) => tab,
                            None => anyhow::bail!("no active tab!?"),
                        };

                        let window = self.window.clone().unwrap();
                        let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                            confirm_quit_program(term, window, tab_id)
                        });
                        self.assign_overlay(tab.tab_id(), overlay);
                        promise::spawn::spawn(future).detach();
                    }
                }
            }
            SelectTextAtMouseCursor(mode) => self.select_text_at_mouse_cursor(*mode, pane),
            ExtendSelectionToMouseCursor(mode) => {
                self.extend_selection_at_mouse_cursor(*mode, pane)
            }
            ClearSelection => {
                self.clear_selection(pane);
            }
            StartWindowDrag => {
                self.window_drag_position = self.current_mouse_event.clone();
            }
            OpenLinkAtMouseCursor => {
                self.do_open_link_at_mouse_cursor(pane);
            }
            EmitEvent(name) => {
                self.emit_window_event(name, None);
            }
            CompleteSelectionOrOpenLinkAtMouseCursor(dest) => {
                let text = self.selection_text(pane);
                if !text.is_empty() {
                    self.copy_to_clipboard(*dest, text);
                    let window = self.window.as_ref().unwrap();
                    window.invalidate();
                } else {
                    self.do_open_link_at_mouse_cursor(pane);
                }
            }
            CompleteSelection(dest) => {
                let text = self.selection_text(pane);
                if !text.is_empty() {
                    self.copy_to_clipboard(*dest, text);
                    let window = self.window.as_ref().unwrap();
                    window.invalidate();
                }
            }
            ClearScrollback(erase_mode) => {
                pane.erase_scrollback(*erase_mode);
                let window = self.window.as_ref().unwrap();
                window.invalidate();
            }
            Search(pattern) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let mut replace_current = false;
                    if let Some(existing) = pane.downcast_ref::<CopyOverlay>() {
                        let mut params = existing.get_params();
                        params.editing_search = true;
                        if !pattern.is_empty() {
                            params.pattern = self.resolve_search_pattern(pattern.clone(), &pane);
                        }
                        existing.apply_params(params);
                        replace_current = true;
                    } else {
                        let search = CopyOverlay::with_pane(
                            self,
                            &pane,
                            CopyModeParams {
                                pattern: self.resolve_search_pattern(pattern.clone(), &pane),
                                editing_search: true,
                            },
                        )?;
                        self.assign_overlay_for_pane(pane.pane_id(), search);
                    }
                    self.pane_state(pane.pane_id())
                        .overlay
                        .as_mut()
                        .map(|overlay| {
                            overlay.key_table_state.activate(KeyTableArgs {
                                name: "search_mode",
                                timeout_milliseconds: None,
                                replace_current,
                                one_shot: false,
                                until_unknown: false,
                                prevent_fallback: false,
                            });
                        });
                }
            }
            QuickSelect => {
                if let Some(pane) = self.get_active_pane_no_overlay() {
                    let qa = QuickSelectOverlay::with_pane(
                        self,
                        &pane,
                        &QuickSelectArguments::default(),
                    );
                    self.assign_overlay_for_pane(pane.pane_id(), qa);
                }
            }
            QuickSelectArgs(args) => {
                if let Some(pane) = self.get_active_pane_no_overlay() {
                    let qa = QuickSelectOverlay::with_pane(self, &pane, args);
                    self.assign_overlay_for_pane(pane.pane_id(), qa);
                }
            }
            ActivateCopyMode => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let mut replace_current = false;
                    if let Some(existing) = pane.downcast_ref::<CopyOverlay>() {
                        let mut params = existing.get_params();
                        params.editing_search = false;
                        existing.apply_params(params);
                        replace_current = true;
                    } else {
                        let copy = CopyOverlay::with_pane(
                            self,
                            &pane,
                            CopyModeParams {
                                pattern: MuxPattern::default(),
                                editing_search: false,
                            },
                        )?;
                        self.assign_overlay_for_pane(pane.pane_id(), copy);
                    }
                    self.pane_state(pane.pane_id())
                        .overlay
                        .as_mut()
                        .map(|overlay| {
                            overlay.key_table_state.activate(KeyTableArgs {
                                name: "copy_mode",
                                timeout_milliseconds: None,
                                replace_current,
                                one_shot: false,
                                until_unknown: false,
                                prevent_fallback: false,
                            });
                        });
                }
            }
            AdjustPaneSize(direction, amount) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    tab.adjust_pane_size(*direction, *amount);
                }
            }
            ActivatePaneByIndex(index) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    let panes = tab.iter_panes();
                    if panes.iter().position(|p| p.index == *index).is_some() {
                        tab.set_active_idx(*index);
                    }
                }
            }
            ActivatePaneDirection(direction) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };

                let tab_id = tab.tab_id();

                if self.tab_state(tab_id).overlay.is_none() {
                    tab.activate_pane_direction(*direction);
                }
            }
            TogglePaneZoomState => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                tab.toggle_zoom();
            }
            SetPaneZoomState(zoomed) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                tab.set_zoomed(*zoomed);
            }
            SwitchWorkspaceRelative(delta) => {
                let mux = Mux::get();
                let workspace = mux.active_workspace();
                let workspaces = mux.iter_workspaces();
                let idx = workspaces.iter().position(|w| *w == workspace).unwrap_or(0);
                let new_idx = idx as isize + delta;
                let new_idx = if new_idx < 0 {
                    workspaces.len() as isize + new_idx
                } else {
                    new_idx
                };
                let new_idx = new_idx as usize % workspaces.len();
                if let Some(w) = workspaces.get(new_idx) {
                    front_end().switch_workspace(w);
                }
            }
            SwitchToWorkspace { name, spawn } => {
                let activity = crate::Activity::new();
                let mux = Mux::get();
                let name = name
                    .as_ref()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| mux.generate_workspace_name());
                let switcher = crate::frontend::WorkspaceSwitcher::new(&name);
                mux.set_active_workspace(&name);

                if mux.iter_windows_in_workspace(&name).is_empty() {
                    let spawn = spawn.as_ref().map(|s| s.clone()).unwrap_or_default();
                    let size = self.terminal_size;
                    let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
                    let src_window_id = self.mux_window_id;

                    promise::spawn::spawn(async move {
                        if let Err(err) = crate::spawn::spawn_command_internal(
                            spawn,
                            SpawnWhere::NewWindow,
                            size,
                            Some(src_window_id),
                            term_config,
                        )
                        .await
                        {
                            log::error!("Failed to spawn: {:#}", err);
                        }
                        switcher.do_switch();
                        drop(activity);
                    })
                    .detach();
                } else {
                    switcher.do_switch();
                }
            }
            DetachDomain(domain) => {
                let domain = Mux::get().resolve_spawn_tab_domain(Some(pane.pane_id()), domain)?;
                domain.detach()?;
            }
            AttachDomain(domain) => {
                let window = self.mux_window_id;
                let domain = domain.to_string();
                let dpi = self.dimensions.dpi as u32;

                promise::spawn::spawn(async move {
                    let mux = Mux::get();
                    let domain = mux
                        .get_domain_by_name(&domain)
                        .ok_or_else(|| anyhow!("{} is not a valid domain name", domain))?;
                    domain.attach(Some(window)).await?;

                    let have_panes_in_domain = mux
                        .iter_panes()
                        .iter()
                        .any(|p| p.domain_id() == domain.domain_id());

                    if !have_panes_in_domain {
                        let config = config::configuration();
                        let _tab = domain
                            .spawn(
                                config.initial_size(
                                    dpi,
                                    Some(crate::cell_pixel_dims(&config, dpi as f64)?),
                                ),
                                None,
                                None,
                                window,
                            )
                            .await?;
                    }

                    Result::<(), anyhow::Error>::Ok(())
                })
                .detach();
            }
            CopyMode(_) => {
                // NOP here; handled by the overlay directly
            }
            RotatePanes(direction) => {
                let mux = Mux::get();
                let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
                    Some(tab) => tab,
                    None => return Ok(PerformAssignmentResult::Handled),
                };
                match direction {
                    RotationDirection::Clockwise => tab.rotate_clockwise(),
                    RotationDirection::CounterClockwise => tab.rotate_counter_clockwise(),
                }
            }
            SplitPane(split) => {
                log::trace!("SplitPane {:?}", split);
                self.spawn_command(
                    &split.command,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction: match split.direction {
                            PaneDirection::Down | PaneDirection::Up => SplitDirection::Vertical,
                            PaneDirection::Left | PaneDirection::Right => {
                                SplitDirection::Horizontal
                            }
                            PaneDirection::Next | PaneDirection::Prev => {
                                log::error!(
                                    "Invalid direction {:?} for SplitPane",
                                    split.direction
                                );
                                return Ok(PerformAssignmentResult::Handled);
                            }
                        },
                        target_is_second: match split.direction {
                            PaneDirection::Down | PaneDirection::Right => true,
                            PaneDirection::Up | PaneDirection::Left => false,
                            PaneDirection::Next | PaneDirection::Prev => unreachable!(),
                        },
                        size: match split.size {
                            SplitSize::Percent(n) => MuxSplitSize::Percent(n),
                            SplitSize::Cells(n) => MuxSplitSize::Cells(n),
                        },
                        top_level: split.top_level,
                    }),
                );
            }
            PaneSelect(args) => {
                let modal = crate::termwindow::paneselect::PaneSelector::new(self, args);
                self.set_modal(Rc::new(modal));
            }
            CharSelect(args) => {
                let modal = crate::termwindow::charselect::CharSelector::new(self, args);
                self.set_modal(Rc::new(modal));
            }
            ResetTerminal => {
                pane.perform_actions(vec![termwiz::escape::Action::Esc(
                    termwiz::escape::Esc::Code(termwiz::escape::EscCode::FullReset),
                )]);
            }
            OpenUri(link) => {
                wezterm_open_url::open_url(link);
            }
            OpenFileBrowser => {
                self.open_file_browser(pane);
            }
            ActivateCommandPalette => {
                let modal = crate::termwindow::palette::CommandPalette::new(self);
                self.set_modal(Rc::new(modal));
            }
            ActivateComposer => {
                if let Some(modal) = crate::termwindow::composer::Composer::new(self, &pane) {
                    self.set_modal(Rc::new(modal));
                }
            }
            ToggleDockedInput => {
                self.toggle_docked_input();
            }
            PromptInputLine(args) => self.show_prompt_input_line(args),
            InputSelector(args) => self.show_input_selector(args),
            Confirmation(args) => self.show_confirmation(args),
            CycleWaitingAgent => {
                let queue: Vec<PaneId> =
                    self.waiting_queue().into_iter().map(|(id, _)| id).collect();
                if let Some(target) = render::sidebar::cycle_waiting_target(&queue, pane.pane_id())
                {
                    self.activate_pane_by_id(target)?;
                }
            }
        };
        Ok(PerformAssignmentResult::Handled)
    }

    fn do_open_link_at_mouse_cursor(&self, pane: &Arc<dyn Pane>) {
        // They clicked on a link, so let's open it!
        // We need to ensure that we spawn the `open` call outside of the context
        // of our window loop; on Windows it can cause a panic due to
        // triggering our WndProc recursively.
        // We get that assurance for free as part of the async dispatch that we
        // perform below; here we allow the user to define an `open-uri` event
        // handler that can bypass the normal `open_url` functionality.
        if let Some(link) = self.current_highlight.as_ref().cloned() {
            let window = GuiWin::new(self);
            let pane = MuxPane(pane.pane_id());

            async fn open_uri(
                lua: Option<Rc<mlua::Lua>>,
                window: GuiWin,
                pane: MuxPane,
                link: String,
            ) -> anyhow::Result<()> {
                let default_click = match lua {
                    Some(lua) => {
                        let args = lua.pack_multi((window, pane, link.clone()))?;
                        config::lua::emit_event(&lua, ("open-uri".to_string(), args))
                            .await
                            .map_err(|e| {
                                log::error!("while processing open-uri event: {:#}", e);
                                e
                            })?
                    }
                    None => true,
                };
                if default_click {
                    log::info!("clicking {}", link);
                    wezterm_open_url::open_url(&link);
                }
                Ok(())
            }

            promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
                open_uri(lua, window, pane, link.uri().to_string())
            }))
            .detach();
        }
    }
    /// Show or hide a tab's pane rows in the sidebar, and remember the choice.
    pub fn toggle_sidebar_tab_expanded(&mut self, tab_idx: usize) {
        if !self.sidebar_expanded_tabs.remove(&tab_idx) {
            self.sidebar_expanded_tabs.insert(tab_idx);
        }
        tgz_ui_state::save_sidebar_expanded_tabs(&self.sidebar_expanded_tabs);
    }

    /// Focus the pane a sidebar pane row refers to, switching tabs if needed.
    /// Returns false when the pane does not live in this window, so callers
    /// that can reach other windows know to fall back instead of doing nothing.
    pub fn activate_sidebar_pane(&mut self, pane_id: PaneId) -> bool {
        let mux = Mux::get();
        let Some(window) = mux.get_window(self.mux_window_id) else {
            return false;
        };
        let Some((tab_idx, tab, pane)) = window.iter().enumerate().find_map(|(idx, tab)| {
            tab.iter_panes_ignoring_zoom()
                .iter()
                .find(|pos| pos.pane.pane_id() == pane_id)
                .map(|pos| (idx, tab.clone(), pos.pane.clone()))
        }) else {
            return false;
        };
        // Drop the borrow before activate_tab, which reaches back into the mux.
        drop(window);

        tab.set_active_pane(&pane);
        // An error here just means the tab is already active; the pane change
        // above still stands, so there is nothing to recover from.
        let _ = self.activate_tab(tab_idx as isize);
        self.update_title();
        true
    }

    /// Close one specific pane, chosen from the sidebar rather than by focus.
    ///
    /// Mirrors `close_current_pane`: a pane that says it cannot close silently
    /// gets the standard confirmation overlay, so a running agent is never
    /// killed by a stray click on a small target.
    pub fn close_sidebar_pane(&mut self, pane_id: PaneId) {
        let mux = Mux::get();
        let Some((_domain_id, _window_id, tab_id)) = mux.resolve_pane_id(pane_id) else {
            return;
        };
        let Some(tab) = mux.get_tab(tab_id) else {
            return;
        };
        let panes = tab.iter_panes_ignoring_zoom();
        let Some(pane) = panes
            .iter()
            .find(|pos| pos.pane.pane_id() == pane_id)
            .map(|pos| pos.pane.clone())
        else {
            return;
        };

        // The last pane in a tab is the tab: closing it through the pane path
        // would leave an empty tab behind, so hand it to the tab-close flow.
        if panes.len() <= 1 {
            let tab_idx = mux.get_window(self.mux_window_id).and_then(|window| {
                window
                    .iter()
                    .position(|candidate| candidate.tab_id() == tab_id)
            });
            if let Some(tab_idx) = tab_idx {
                self.close_specific_tab(tab_idx, true);
            }
            return;
        }

        if pane.can_close_without_prompting(CloseReason::Pane) {
            mux.remove_pane(pane_id);
            return;
        }

        let mux_window_id = self.mux_window_id;
        let window = match self.window.clone() {
            Some(window) => window,
            None => return,
        };
        let (overlay, future) = start_overlay_pane(self, &pane, move |pane_id, term| {
            confirm_close_pane(pane_id, term, mux_window_id, window)
        });
        self.assign_overlay_for_pane(pane_id, overlay);
        promise::spawn::spawn(future).detach();
    }

    fn close_current_pane(&mut self, confirm: bool) {
        let mux_window_id = self.mux_window_id;
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let pane = match tab.get_active_pane() {
            Some(p) => p,
            None => return,
        };

        let pane_id = pane.pane_id();
        if confirm && !pane.can_close_without_prompting(CloseReason::Pane) {
            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay_pane(self, &pane, move |pane_id, term| {
                confirm_close_pane(pane_id, term, mux_window_id, window)
            });
            self.assign_overlay_for_pane(pane_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_pane(pane_id);
        }
    }

    fn close_specific_tab(&mut self, tab_idx: usize, confirm: bool) {
        let mux = Mux::get();
        let mux_window_id = self.mux_window_id;
        let mux_window = match mux.get_window(mux_window_id) {
            Some(w) => w,
            None => return,
        };

        let is_last_tab = mux_window.len() <= 1;

        let tab = match mux_window.get_by_idx(tab_idx) {
            Some(tab) => Arc::clone(tab),
            None => return,
        };
        drop(mux_window);

        let tab_id = tab.tab_id();
        // Closing the last tab in a window also closes the window, so always
        // confirm in that case even if the tab itself would otherwise skip
        // prompting — a stray sidebar click should never silently take the
        // whole window (and, if it's the last window, the app) down. This
        // must never be a silent no-op: that used to leave a stuck pane with
        // no way to close it from the sidebar at all.
        if confirm && (is_last_tab || !tab.can_close_without_prompting(CloseReason::Tab)) {
            if self.activate_tab(tab_idx as isize).is_err() {
                return;
            }

            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                confirm_close_tab(tab_id, term, mux_window_id, window)
            });
            self.assign_overlay(tab_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_tab(tab_id);
        }
    }

    fn close_current_tab(&mut self, confirm: bool) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let tab_id = tab.tab_id();
        let mux_window_id = self.mux_window_id;
        if confirm && !tab.can_close_without_prompting(CloseReason::Tab) {
            let window = self.window.clone().unwrap();
            let (overlay, future) = start_overlay(self, &tab, move |tab_id, term| {
                confirm_close_tab(tab_id, term, mux_window_id, window)
            });
            self.assign_overlay(tab_id, overlay);
            promise::spawn::spawn(future).detach();
        } else {
            mux.remove_tab(tab_id);
        }
    }

    /// Collect the tab_ids for indices in `range` (exclusive of the anchor),
    /// drop the mux window borrow, then remove them in descending order so
    /// earlier indices remain valid during the sweep.
    fn close_tabs_in_range(&mut self, range: std::ops::Range<usize>) {
        let mux = Mux::get();
        let mux_window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return,
        };
        let mut tab_ids: Vec<TabId> = Vec::new();
        for i in range {
            if let Some(tab) = mux_window.get_by_idx(i) {
                tab_ids.push(tab.tab_id());
            }
        }
        drop(mux_window);
        for tab_id in tab_ids.into_iter().rev() {
            mux.remove_tab(tab_id);
        }
    }

    /// Close every tab above (idx < anchor). Preserves the anchor tab.
    pub fn close_tabs_above(&mut self, anchor_tab_idx: usize) {
        if anchor_tab_idx == 0 {
            return;
        }
        self.close_tabs_in_range(0..anchor_tab_idx);
    }

    /// Close every tab below (idx > anchor). Preserves the anchor tab.
    pub fn close_tabs_below(&mut self, anchor_tab_idx: usize) {
        let mux = Mux::get();
        let len = match mux.get_window(self.mux_window_id) {
            Some(w) => w.len(),
            None => return,
        };
        drop(mux);
        if anchor_tab_idx + 1 >= len {
            return;
        }
        self.close_tabs_in_range((anchor_tab_idx + 1)..len);
    }

    /// Close every tab except the anchor. Preserves the anchor tab.
    ///
    /// Collects all target tab_ids in a single pass before any removal so the
    /// index shifts from closing the above-tabs don't invalidate the below-tab
    /// indices — a problem two separate `close_tabs_in_range` calls would have.
    pub fn close_all_other_tabs(&mut self, anchor_tab_idx: usize) {
        let mux = Mux::get();
        let mux_window = match mux.get_window(self.mux_window_id) {
            Some(w) => w,
            None => return,
        };
        let len = mux_window.len();
        if len <= 1 {
            return;
        }
        let mut tab_ids: Vec<TabId> = Vec::new();
        for i in 0..len {
            if i != anchor_tab_idx {
                if let Some(tab) = mux_window.get_by_idx(i) {
                    tab_ids.push(tab.tab_id());
                }
            }
        }
        drop(mux_window);
        for tab_id in tab_ids.into_iter().rev() {
            mux.remove_tab(tab_id);
        }
    }

    pub fn pane_state(&self, pane_id: PaneId) -> RefMut<'_, PaneState> {
        RefMut::map(self.pane_state.borrow_mut(), |state| {
            state.entry(pane_id).or_insert_with(PaneState::default)
        })
    }

    pub fn tab_state(&self, tab_id: TabId) -> RefMut<'_, TabState> {
        RefMut::map(self.tab_state.borrow_mut(), |state| {
            state.entry(tab_id).or_insert_with(TabState::default)
        })
    }

    /// Resize overlays to match their corresponding tab/pane dimensions
    pub fn resize_overlays(&self) {
        let mux = Mux::get();
        for (_, state) in self.tab_state.borrow().iter() {
            if let Some(overlay) = state.overlay.as_ref().map(|o| &o.pane) {
                overlay.resize(self.terminal_size).ok();
            }
        }
        for (pane_id, state) in self.pane_state.borrow().iter() {
            if let Some(overlay) = state.overlay.as_ref().map(|o| &o.pane) {
                if let Some(pane) = mux.get_pane(*pane_id) {
                    let dims = pane.get_dimensions();
                    overlay
                        .resize(TerminalSize {
                            cols: dims.cols,
                            rows: dims.viewport_rows,
                            dpi: self.terminal_size.dpi,
                            pixel_height: (self.terminal_size.pixel_height
                                / self.terminal_size.rows)
                                * dims.viewport_rows,
                            pixel_width: (self.terminal_size.pixel_width / self.terminal_size.cols)
                                * dims.cols,
                        })
                        .ok();
                }
            }
        }
    }

    pub fn get_viewport(&self, pane_id: PaneId) -> Option<StableRowIndex> {
        self.pane_state(pane_id).viewport
    }

    pub fn set_viewport(
        &mut self,
        pane_id: PaneId,
        position: Option<StableRowIndex>,
        dims: RenderableDimensions,
    ) {
        let pos = match position {
            Some(pos) => {
                // Drop out of scrolling mode if we're off the bottom
                if pos >= dims.physical_top {
                    None
                } else {
                    Some(pos.max(dims.scrollback_top))
                }
            }
            None => None,
        };

        let mut state = self.pane_state(pane_id);
        if pos != state.viewport {
            state.viewport = pos;

            // This is a bit gross.  If we add other overlays that need this information,
            // this should get extracted out into a trait
            if let Some(overlay) = state.overlay.as_ref() {
                if let Some(copy) = overlay.pane.downcast_ref::<CopyOverlay>() {
                    copy.viewport_changed(pos);
                } else if let Some(qs) = overlay.pane.downcast_ref::<QuickSelectOverlay>() {
                    qs.viewport_changed(pos);
                }
            }
            self.window.as_ref().unwrap().invalidate();
        }
    }

    fn maybe_scroll_to_bottom_for_input(&mut self, pane: &Arc<dyn Pane>) {
        if self.config.scroll_to_bottom_on_input {
            self.scroll_to_bottom(pane);
        }
    }

    fn scroll_to_top(&mut self, pane: &Arc<dyn Pane>) {
        let dims = pane.get_dimensions();
        self.set_viewport(pane.pane_id(), Some(dims.scrollback_top), dims);
    }

    fn scroll_to_bottom(&mut self, pane: &Arc<dyn Pane>) {
        self.pane_state(pane.pane_id()).viewport = None;
    }

    fn get_active_pane_no_overlay(&self) -> Option<Arc<dyn Pane>> {
        let mux = Mux::get();
        mux.get_active_tab_for_window(self.mux_window_id)
            .and_then(|tab| tab.get_active_pane())
    }

    /// Returns a Pane that we can interact with; this will typically be
    /// the active tab for the window, but if the window has a tab-wide
    /// overlay (such as the launcher / tab navigator),
    /// then that will be returned instead.  Otherwise, if the pane has
    /// an active overlay (such as search or copy mode) then that will
    /// be returned.
    pub fn get_active_pane_or_overlay(&self) -> Option<Arc<dyn Pane>> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return None,
        };

        let tab_id = tab.tab_id();

        if let Some(tab_overlay) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            Some(tab_overlay)
        } else {
            let pane = tab.get_active_pane()?;
            let pane_id = pane.pane_id();
            self.pane_state(pane_id)
                .overlay
                .as_ref()
                .map(|overlay| overlay.pane.clone())
                .or_else(|| Some(pane))
        }
    }

    fn get_splits(&mut self) -> Vec<PositionedSplit> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return vec![],
        };

        let tab_id = tab.tab_id();

        if self.tab_state(tab_id).overlay.is_some() {
            vec![]
        } else {
            tab.iter_splits()
        }
    }

    fn pos_pane_to_pane_info(pos: &PositionedPane) -> PaneInformation {
        PaneInformation {
            pane_id: pos.pane.pane_id(),
            pane_index: pos.index,
            is_active: pos.is_active,
            is_zoomed: pos.is_zoomed,
            has_unseen_output: pos.pane.has_unseen_output(),
            left: pos.left,
            top: pos.top,
            width: pos.width,
            height: pos.height,
            pixel_width: pos.pixel_width,
            pixel_height: pos.pixel_height,
            title: pos.pane.get_title(),
            user_vars: pos.pane.copy_user_vars(),
            progress: pos.pane.get_progress(),
        }
    }

    fn get_tab_information(&mut self) -> Vec<TabInformation> {
        let mux = Mux::get();
        let window = match mux.get_window(self.mux_window_id) {
            Some(window) => window,
            _ => return vec![],
        };
        let tab_index = window.get_active_idx();

        window
            .iter()
            .enumerate()
            .map(|(idx, tab)| {
                let panes = self.get_pos_panes_for_tab(tab);

                TabInformation {
                    tab_index: idx,
                    tab_id: tab.tab_id(),
                    is_active: tab_index == idx,
                    is_last_active: window
                        .get_last_active_idx()
                        .map(|last_active| last_active == idx)
                        .unwrap_or(false),
                    window_id: self.mux_window_id,
                    tab_title: tab.get_title(),
                    active_pane: panes
                        .iter()
                        .find(|p| p.is_active)
                        .map(Self::pos_pane_to_pane_info),
                }
            })
            .collect()
    }

    fn get_pane_information(&self) -> Vec<PaneInformation> {
        self.get_panes_to_render()
            .iter()
            .map(Self::pos_pane_to_pane_info)
            .collect()
    }

    fn get_pos_panes_for_tab(&self, tab: &Arc<Tab>) -> Vec<PositionedPane> {
        let tab_id = tab.tab_id();

        if let Some(pane) = self
            .tab_state(tab_id)
            .overlay
            .as_ref()
            .map(|overlay| overlay.pane.clone())
        {
            let size = tab.get_size();
            vec![PositionedPane {
                index: 0,
                is_active: true,
                is_zoomed: false,
                left: 0,
                top: 0,
                width: size.cols as _,
                height: size.rows as _,
                pixel_width: size.cols as usize * self.render_metrics.cell_size.width as usize,
                pixel_height: size.rows as usize * self.render_metrics.cell_size.height as usize,
                pane,
            }]
        } else {
            let mut panes = tab.iter_panes();
            for p in &mut panes {
                if let Some(overlay) = self.pane_state(p.pane.pane_id()).overlay.as_ref() {
                    p.pane = Arc::clone(&overlay.pane);
                }
            }
            panes
        }
    }

    fn get_panes_to_render(&self) -> Vec<PositionedPane> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return vec![],
        };

        self.get_pos_panes_for_tab(&tab)
    }

    /// if pane_id.is_none(), removes any overlay for the specified tab.
    /// Otherwise: if the overlay is the specified pane for that tab, remove it.
    fn cancel_overlay_for_tab(&mut self, tab_id: TabId, pane_id: Option<PaneId>) {
        if pane_id.is_some() {
            let current = self
                .tab_state(tab_id)
                .overlay
                .as_ref()
                .map(|o| o.pane.pane_id());
            if current != pane_id {
                return;
            }
        }
        if let Some(overlay) = self.tab_state(tab_id).overlay.take() {
            Mux::get().remove_pane(overlay.pane.pane_id());
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn schedule_cancel_overlay(window: Window, tab_id: TabId, pane_id: Option<PaneId>) {
        window.notify(TermWindowNotif::CancelOverlayForTab { tab_id, pane_id });
    }

    fn cancel_overlay_for_pane(&mut self, pane_id: PaneId) {
        if let Some(overlay) = self.pane_state(pane_id).overlay.take() {
            // Ungh, when I built the CopyOverlay, its pane doesn't get
            // added to the mux and instead it reports the overlaid
            // pane id.  Take care to avoid killing ourselves off
            // when closing the CopyOverlay
            if pane_id != overlay.pane.pane_id() {
                Mux::get().remove_pane(overlay.pane.pane_id());
            }
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    pub fn schedule_cancel_overlay_for_pane(window: Window, pane_id: PaneId) {
        window.notify(TermWindowNotif::CancelOverlayForPane(pane_id));
    }

    pub fn assign_overlay_for_pane(&mut self, pane_id: PaneId, pane: Arc<dyn Pane>) {
        self.cancel_overlay_for_pane(pane_id);
        self.pane_state(pane_id).overlay.replace(OverlayState {
            pane,
            key_table_state: KeyTableState::default(),
        });
        self.update_title();
    }

    pub fn assign_overlay(&mut self, tab_id: TabId, overlay: Arc<dyn Pane>) {
        self.cancel_overlay_for_tab(tab_id, None);
        self.tab_state(tab_id).overlay.replace(OverlayState {
            pane: overlay,
            key_table_state: KeyTableState::default(),
        });
        self.update_title();
    }

    fn resolve_search_pattern(&self, pattern: Pattern, pane: &Arc<dyn Pane>) -> MuxPattern {
        match pattern {
            Pattern::CaseSensitiveString(s) => MuxPattern::CaseSensitiveString(s),
            Pattern::CaseInSensitiveString(s) => MuxPattern::CaseInSensitiveString(s),
            Pattern::CaseSmartString(s) => MuxPattern::CaseSmartString(s),
            Pattern::Regex(s) => MuxPattern::Regex(s),
            Pattern::CurrentSelectionOrEmptyString => {
                let text = self.selection_text(pane);
                let first_line = text
                    .lines()
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                MuxPattern::CaseSensitiveString(first_line)
            }
        }
    }
}

impl Drop for TermWindow {
    fn drop(&mut self) {
        // Mark the mux subscription as dead.
        // (will actually unsubscribe on the next notif from mux)
        self.mux_subscription_dead.store(true, Ordering::Relaxed);
        self.clear_all_overlays();
        if let Some(window) = self.window.take() {
            if let Some(fe) = try_front_end() {
                fe.forget_known_window(&window);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_simple_user_host() {
        let argv = vec!["ssh".to_string(), "user@host".to_string()];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("user@host".to_string(), None)));
    }

    #[test]
    fn parse_ssh_with_p_flag_and_user_host() {
        let argv = vec![
            "ssh".to_string(),
            "-p".to_string(),
            "2222".to_string(),
            "user@host".to_string(),
        ];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("user@host".to_string(), Some(2222))));
    }

    #[test]
    fn parse_ssh_with_p_attached_and_host() {
        let argv = vec!["ssh".to_string(), "-p2222".to_string(), "host".to_string()];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("host".to_string(), Some(2222))));
    }

    #[test]
    fn parse_ssh_with_o_port() {
        let argv = vec![
            "ssh".to_string(),
            "-oPort=2222".to_string(),
            "foo".to_string(),
        ];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("foo".to_string(), Some(2222))));
    }

    #[test]
    fn parse_ssh_with_separate_user_option() {
        let argv = vec![
            "ssh".to_string(),
            "-o".to_string(),
            "User=alice".to_string(),
            "foo".to_string(),
        ];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("alice@foo".to_string(), None)));
    }

    #[test]
    fn parse_mosh_does_not_treat_udp_port_as_ssh_port() {
        let argv = vec![
            "mosh".to_string(),
            "-p".to_string(),
            "60000".to_string(),
            "user@foo".to_string(),
        ];
        let got = TermWindow::parse_ssh_destination_from_argv(&argv);
        assert_eq!(got, Some(("user@foo".to_string(), None)));
    }

    #[test]
    fn host_looks_remote_when_different_from_local() {
        assert!(TermWindow::host_looks_remote("build01", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_when_matches_local_case_insensitive() {
        assert!(!TermWindow::host_looks_remote("MyMac", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_when_matches_local_with_domain_suffix() {
        assert!(!TermWindow::host_looks_remote("mymac.local", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_when_empty() {
        assert!(!TermWindow::host_looks_remote("", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_for_localhost() {
        assert!(!TermWindow::host_looks_remote("localhost", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_for_ipv4_loopback() {
        assert!(!TermWindow::host_looks_remote("127.0.0.1", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_false_for_ipv6_loopback() {
        assert!(!TermWindow::host_looks_remote("::1", Some("mymac")));
    }

    #[test]
    fn host_looks_remote_true_when_no_local_label() {
        assert!(TermWindow::host_looks_remote("build01", None));
    }

    #[test]
    fn windows_cli_bin_has_the_exe_suffix() {
        // The regression: probing for an extension-less `tgzterminal` never
        // matched on Windows, so the script silently got wezterm-gui.exe.
        // A backslash path cannot be exercised here — `Path` only splits on the
        // host's separator, so on a unix test host `C:\a\b` is a single
        // component. `shell_path` covers the separator rewrite instead.
        let exe = PathBuf::from("/programs/TGZTerminal/wezterm-gui.exe");
        let bin = cli_bin_for_script(Some(&exe), true, &|_| true);
        assert_eq!(bin, "/programs/TGZTerminal/tgzterminal.exe");
    }

    #[test]
    fn shell_path_rewrites_windows_separators() {
        // The worktree script is POSIX sh run through msys/git-bash, where a
        // backslash is an escape character.
        let path = PathBuf::from(r"C:\Program Files\TGZTerminal\tgzterminal.exe");
        assert_eq!(
            shell_path(&path, true),
            "C:/Program Files/TGZTerminal/tgzterminal.exe"
        );
        // Untouched off Windows, where a backslash is a legal filename byte.
        assert_eq!(
            shell_path(&PathBuf::from("/opt/a b/c"), false),
            "/opt/a b/c"
        );
    }

    #[test]
    fn unix_cli_bin_keeps_the_native_path() {
        let exe = PathBuf::from("/Applications/TGZTerminal.app/Contents/MacOS/wezterm-gui");
        let bin = cli_bin_for_script(Some(&exe), false, &|_| true);
        assert_eq!(
            bin,
            "/Applications/TGZTerminal.app/Contents/MacOS/tgzterminal"
        );
    }

    #[test]
    fn missing_cli_falls_back_to_the_bare_name_not_the_gui() {
        // Handing the script the GUI binary is worse than handing it a name that
        // PATH can resolve: the GUI is not a CLI and fails obscurely.
        let exe = PathBuf::from("/opt/tgz/wezterm-gui");
        assert_eq!(
            cli_bin_for_script(Some(&exe), false, &|_| false),
            "tgzterminal"
        );
        assert_eq!(cli_bin_for_script(None, false, &|_| true), "tgzterminal");
    }
}
