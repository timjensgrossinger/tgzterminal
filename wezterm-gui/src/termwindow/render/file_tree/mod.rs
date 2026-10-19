//! The Worktree panel's window glue: per-tab state, where the panel sits, how
//! it is dragged, painted, clicked and typed into, and how a picked file is
//! opened.
//!
//! The model is the GUI-free [`crate::file_tree`]; this file only gathers its
//! inputs from the window and acts on its answers. Folders, git status, the
//! filter's file list and the agents' touched files are read on a worker
//! thread and applied through `TermWindowNotif::Apply`, the same way the
//! Changes panel scans.
//!
//! Dragging rows and the scroll bar lives in [`drag`].

mod drag;
mod menu;
mod preview;

use crate::diff_panel::watch::Watch;
use crate::file_tree::editor::{resolve_editor, EditorHost, EditorLaunch};
use crate::file_tree::list::{list_dir, list_open, path_index, ListOptions};
use crate::file_tree::status::{AgentMarks, Badge, GitStatus};
use crate::file_tree::view::{self as tree_view, Nav, Row, RowState};
use crate::file_tree::{filter, parent_rel, EntryKind, Tree};
use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::agent_launch::split_spawn;
use crate::termwindow::render::diff_panel::{opaque, settle_scan_target};
use crate::termwindow::render::sidebar::{
    contrast_label_color, herd_scan_is_due, lerp_rgba, sidebar_width_scale_for_dpi, FLOAT_GAP,
    PAD_X, RADIUS,
};
use crate::termwindow::{
    tgz_ui_state, wsl_paths, FileTreeAction, FileTreeMenuAction, TermWindowNotif, UIItem,
    UIItemType,
};
use ::window::{
    CursorIcon, Modifiers, MouseEvent, MouseEventKind as WMEK, MousePress, RectF, WindowOps,
};
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use config::TermConfig;
use mux::pane::{Pane, PaneId};
use mux::tab::{SplitDirection, SplitRequest, SplitSize as MuxSplitSize, TabId};
use mux::Mux;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use termwiz::input::KeyCode;
use window::color::LinearRgba;

/// Narrowest the panel may be dragged, in pixels at 2x.
const MIN_WIDTH_PX: f32 = 280.;
/// Widest share of the window the panel may take.
const MAX_WIDTH_SHARE: f32 = 0.5;
/// Thickness of the draggable edge, in pixels at 1x.
const GRIP: f32 = 6.;
/// How often the folders on show are read again while the panel is open.
const REFRESH: Duration = Duration::from_secs(2);
/// A read may use at most this share of the time between reads.
const SCAN_DUTY_DIVISOR: u32 = 4;
/// A read still running after this long is presumed lost.
const SCAN_WATCHDOG: Duration = Duration::from_secs(30);
/// How long after a watched change the tree is read: writes come in bursts.
const WATCH_SETTLE: Duration = Duration::from_millis(300);
/// Two clicks on the same row within this long open it.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// How long open folders wait before they are written to the state file, so
/// a burst of clicks is one write.
const SAVE_SETTLE: Duration = Duration::from_millis(800);

/// The filter: what was typed, the files it searches, and what it found.
#[derive(Default)]
struct FilterView {
    query: String,
    /// Every file under the root; `None` until the first read with the
    /// filter open comes back.
    index: Option<Vec<String>>,
    /// The index was cut off at [`crate::file_tree::list::MAX_INDEX`].
    cut: bool,
    /// The matches as a tree; `None` while the query is empty.
    tree: Option<Tree>,
    expanded: HashSet<String>,
    highlights: HashMap<String, Vec<usize>>,
    count: usize,
}

/// What the panel shows for one tab.
pub struct TreeView {
    /// The pane whose directory the tree follows, and beside which files open.
    target: PaneId,
    tree: Option<Tree>,
    /// Why there is no tree, when there is none.
    error: Option<String>,
    expanded: HashSet<String>,
    rows: Vec<Row>,
    /// The row the keyboard is on.
    selected: Option<String>,
    /// Every selected row, `selected` among them: more than one after a
    /// Cmd/Ctrl- or Shift-click.
    marked: HashSet<String>,
    /// Where a Shift-click's range starts.
    anchor: Option<String>,
    scroll: usize,
    scanned_at: Option<Instant>,
    scan_cost: Duration,
    /// Open folders changed since they were last saved.
    expanded_changed_at: Option<Instant>,
    status: Option<GitStatus>,
    marks: AgentMarks,
    /// Follows the root, so a written file is read at once instead of at the
    /// next poll. `None` where watching is not possible.
    watch: Option<Watch>,
    filter: Option<FilterView>,
    /// A row being named: renamed, or a new file or folder.
    edit: Option<menu::EditState>,
    /// The quick preview, while it is open.
    preview: Option<preview::PreviewView>,
}

impl TreeView {
    fn new(target: PaneId) -> Self {
        Self {
            target,
            tree: None,
            error: None,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            marked: HashSet::new(),
            anchor: None,
            scroll: 0,
            scanned_at: None,
            scan_cost: Duration::ZERO,
            expanded_changed_at: None,
            status: None,
            marks: AgentMarks::default(),
            watch: None,
            filter: None,
            edit: None,
            preview: None,
        }
    }

    fn rebuild(&mut self) {
        self.rows = match (&self.filter, &self.tree) {
            (Some(filter), _) if filter.tree.is_some() => {
                let tree = filter.tree.as_ref().expect("checked above");
                let mut rows = tree_view::flatten(tree, &filter.expanded);
                for row in &mut rows {
                    if let Some(chars) = filter.highlights.get(&row.rel) {
                        row.highlight = chars.clone();
                    }
                }
                rows
            }
            // A query and nothing to search yet: nothing to show either.
            (Some(filter), _) if !filter.query.trim().is_empty() => Vec::new(),
            (_, Some(tree)) => tree_view::flatten(tree, &self.expanded),
            _ => Vec::new(),
        };
        if let Some(edit) = &self.edit {
            menu::insert_edit_row(&mut self.rows, edit);
        }
    }

    /// Match the query again. `reselect`: the query changed, so the best
    /// match is selected and shown; otherwise (the file list was re-read)
    /// the selection stays where it is while it is still a match.
    fn refilter(&mut self, reselect: bool) {
        let Some(root) = self.tree.as_ref().map(|tree| tree.root.clone()) else {
            return;
        };
        let Some(filter) = self.filter.as_mut() else {
            return;
        };
        let best = match &filter.index {
            Some(index) if !filter.query.trim().is_empty() => {
                let found = filter::matches(index, &filter.query, filter::MAX_MATCHES);
                let (tree, open) = filter::match_tree(root, &found);
                filter.count = found.len();
                filter.highlights = found
                    .iter()
                    .map(|found| (found.rel.clone(), found.name_chars.clone()))
                    .collect();
                filter.tree = Some(tree);
                filter.expanded = open;
                found.first().map(|found| found.rel.clone())
            }
            _ => {
                filter.tree = None;
                filter.highlights.clear();
                filter.count = 0;
                None
            }
        };
        self.rebuild();
        if reselect || self.selected_index().is_none() {
            if best.is_some() {
                self.select_only(best);
            }
            self.scroll = 0;
        }
    }

    /// Select `rel` alone.
    fn select_only(&mut self, rel: Option<String>) {
        self.marked = rel.iter().cloned().collect();
        self.anchor = rel.clone();
        self.selected = rel;
    }

    /// Whether `rel` is part of the selection.
    fn is_marked(&self, rel: &str) -> bool {
        self.marked.contains(rel)
    }

    /// The selected rows that are on show, in row order, with their kinds.
    fn marked_rows(&self) -> Vec<(String, EntryKind)> {
        self.rows
            .iter()
            .filter(|row| self.is_marked(&row.rel))
            .map(|row| (row.rel.clone(), row.kind))
            .collect()
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected
            .as_deref()
            .and_then(|rel| tree_view::index_of(&self.rows, rel))
    }

    fn root_key(&self) -> Option<String> {
        self.tree
            .as_ref()
            .map(|tree| tree.root.to_string_lossy().to_string())
    }

    /// Open every folder leading to `rel` in the unfiltered tree.
    fn reveal(&mut self, rel: &str) {
        let mut current = rel;
        let mut opened = false;
        while let Some(parent) = parent_rel(current).filter(|parent| !parent.is_empty()) {
            opened |= self.expanded.insert(parent.to_string());
            current = parent;
        }
        if opened {
            self.expanded_changed_at = Some(Instant::now());
            self.scanned_at = None;
        }
    }
}

pub struct FileTreeState {
    /// The tabs the panel is open for, and what it shows in each.
    views: HashMap<TabId, TreeView>,
    /// Dragged width in physical pixels; `None` uses `file_browser.width_px`.
    pub width: Option<usize>,
    /// The reservation the last layout pass used; see the Changes panel's
    /// field of the same name.
    layout_width: Cell<usize>,
    relayout_queued: Cell<bool>,
    scan_started_at: Cell<Option<Instant>>,
    /// Body rows that fit, as of the last paint.
    visible_rows: Cell<usize>,
    /// The row last clicked and when, for double clicks.
    last_click: Option<(String, Instant)>,
    /// The panel has the keyboard.
    focused: bool,
    /// Hidden files shown or not, as toggled in the panel; `None` follows
    /// `file_browser.show_hidden`.
    show_hidden: Option<bool>,
    /// A row being dragged, from the press on it to the release.
    drag: Option<drag::RowDrag>,
    /// Where the last paint put the body and the scroll bar, for drags.
    painted: Cell<Option<drag::Painted>>,
    /// The right-click menu, while it is open.
    menu: Option<menu::MenuState>,
    /// The editor pane the panel last opened in each tab, for
    /// `reuse_editor_pane`.
    editors: HashMap<TabId, PaneId>,
    /// Dragged share of the body the preview takes; `None` until dragged.
    preview_share: Option<f32>,
    /// Preview lines that fit, as of the last paint.
    preview_rows: Cell<usize>,
}

impl FileTreeState {
    pub fn load() -> Self {
        Self {
            views: HashMap::new(),
            width: tgz_ui_state::load_file_tree_width(),
            layout_width: Cell::new(0),
            relayout_queued: Cell::new(false),
            scan_started_at: Cell::new(None),
            visible_rows: Cell::new(0),
            last_click: None,
            focused: false,
            show_hidden: None,
            drag: None,
            painted: Cell::new(None),
            menu: None,
            editors: HashMap::new(),
            preview_share: None,
            preview_rows: Cell::new(0),
        }
    }
}

/// What a read of the tree produced, carried back to the GUI thread.
struct ScanResult {
    tree: Result<Tree, String>,
    /// The open folders remembered for a root the view did not have yet.
    remembered: Option<HashSet<String>>,
    status: Option<GitStatus>,
    /// `None` when the sessions could not be asked this time.
    marks: Option<AgentMarks>,
    /// Every file, when the filter is open.
    index: Option<(Vec<String>, bool)>,
    /// A watch started for a root that had none.
    watch: Option<Watch>,
}

/// Right-pointing or downward chevron, as stacked rounded bars like the
/// sidebar's own.
fn chevron_bars(cx: f32, cy: f32, scale: f32, expanded: bool) -> Vec<RectF> {
    let bar = (1.5 * scale).max(1.5);
    let widest = 8. * scale;
    let mut bars = Vec::new();
    for step in 0..4 {
        let length = widest * (1. - step as f32 / 4.);
        if length < 1. {
            break;
        }
        let offset = -(widest * 0.25) + step as f32 * bar;
        bars.push(if expanded {
            euclid::rect(cx - length * 0.5, cy + offset, length, bar)
        } else {
            euclid::rect(cx + offset, cy - length * 0.5, bar, length)
        });
    }
    bars
}

/// Nerd Font glyph for a row: folders open or closed, files by kind. The
/// Symbols Nerd Font fallback the fork loads carries all of them.
fn row_icon(row: &Row) -> &'static str {
    match row.state {
        RowState::Expanded | RowState::Loading => "\u{f07c}",
        RowState::Collapsed | RowState::Failed => "\u{f07b}",
        RowState::File => {
            let name = row.name.to_ascii_lowercase();
            let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
            match (name.as_str(), ext) {
                (".gitignore" | ".gitattributes" | ".gitmodules", _) => "\u{e702}",
                ("dockerfile", _) => "\u{f308}",
                (_, "lock") => "\u{f023}",
                (_, "rs") => "\u{e7a8}",
                (_, "md" | "markdown") => "\u{e73e}",
                (_, "json") => "\u{e60b}",
                (_, "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf") => "\u{e615}",
                (_, "lua") => "\u{e620}",
                (_, "py") => "\u{e73c}",
                (_, "js" | "mjs" | "cjs") => "\u{e74e}",
                (_, "ts" | "tsx") => "\u{e628}",
                (_, "go") => "\u{e626}",
                (_, "c" | "h") => "\u{e61e}",
                (_, "cpp" | "cc" | "hpp") => "\u{e61d}",
                (_, "html" | "htm") => "\u{e736}",
                (_, "css" | "scss") => "\u{e749}",
                (_, "sh" | "bash" | "zsh" | "fish" | "ps1") => "\u{f489}",
                (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "icns") => "\u{f1c5}",
                _ => "\u{f15b}",
            }
        }
    }
}

/// A git badge's colour from the terminal's ANSI red, green and yellow, as
/// the Changes panel colours its counts.
fn badge_color(ansi: &[LinearRgba; 3], badge: Badge) -> LinearRgba {
    let [red, green, yellow] = *ansi;
    match badge {
        Badge::Untracked | Badge::Added => green,
        Badge::Modified | Badge::Renamed => yellow,
        Badge::Deleted | Badge::Conflict => red,
    }
}

/// Runs of `len` characters split by whether they are in `marked`:
/// `(start, end, marked)`, covering `0..len` in order.
fn highlight_runs(len: usize, marked: &[usize]) -> Vec<(usize, usize, bool)> {
    let mut runs: Vec<(usize, usize, bool)> = Vec::new();
    for at in 0..len {
        let on = marked.binary_search(&at).is_ok();
        match runs.last_mut() {
            Some(run) if run.2 == on => run.1 = at + 1,
            _ => runs.push((at, at + 1, on)),
        }
    }
    runs
}

/// What a printable key does while the panel has the keyboard and the
/// filter is not being typed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    First,
    Last,
    Activate,
    Toggle,
    StartFilter,
    Preview,
    CollapseAll,
    Refresh,
    ToggleHidden,
    /// A menu action on the selection.
    Run(crate::termwindow::FileTreeMenuAction),
    Leave,
    /// Taken so it does not reach the terminal, but does nothing.
    Swallow,
}

fn tree_key(key: &KeyCode) -> Option<TreeKey> {
    Some(match key {
        KeyCode::UpArrow | KeyCode::Char('k') => TreeKey::Up,
        KeyCode::DownArrow | KeyCode::Char('j') => TreeKey::Down,
        KeyCode::LeftArrow | KeyCode::Char('h') => TreeKey::Left,
        KeyCode::RightArrow | KeyCode::Char('l') => TreeKey::Right,
        KeyCode::PageUp => TreeKey::PageUp,
        KeyCode::PageDown => TreeKey::PageDown,
        KeyCode::Home | KeyCode::Char('g') => TreeKey::First,
        KeyCode::End | KeyCode::Char('G') => TreeKey::Last,
        KeyCode::Enter | KeyCode::Char('o') => TreeKey::Activate,
        KeyCode::Char(' ') => TreeKey::Toggle,
        KeyCode::Char('/') => TreeKey::StartFilter,
        KeyCode::Char('p') => TreeKey::Preview,
        KeyCode::Char('-') => TreeKey::CollapseAll,
        KeyCode::Char('R') => TreeKey::Refresh,
        KeyCode::Char('.') => TreeKey::ToggleHidden,
        KeyCode::Function(2) => TreeKey::Run(FileTreeMenuAction::Rename),
        KeyCode::Char('a') => TreeKey::Run(FileTreeMenuAction::NewFile),
        KeyCode::Char('A') => TreeKey::Run(FileTreeMenuAction::NewFolder),
        KeyCode::Delete => TreeKey::Run(FileTreeMenuAction::Trash),
        KeyCode::Char('y') => TreeKey::Run(FileTreeMenuAction::CopyPath),
        KeyCode::Char('Y') => TreeKey::Run(FileTreeMenuAction::CopyRelativePath),
        KeyCode::Escape => TreeKey::Leave,
        KeyCode::Char(c) if !c.is_control() => TreeKey::Swallow,
        _ => return None,
    })
}

impl crate::TermWindow {
    fn file_tree_active_tab(&self) -> Option<TabId> {
        Mux::get()
            .get_active_tab_for_window(self.mux_window_id)
            .map(|tab| tab.tab_id())
    }

    /// Whether the panel is open for the tab on show.
    pub fn file_tree_shown(&self) -> bool {
        !self.file_tree.views.is_empty()
            && self
                .file_tree_active_tab()
                .is_some_and(|tab_id| self.file_tree.views.contains_key(&tab_id))
    }

    /// Whether keys go to the panel rather than the terminal.
    pub fn file_tree_focused(&self) -> bool {
        self.file_tree.focused && self.file_tree_shown()
    }

    /// A click landed on the panel (`true`) or anywhere else.
    pub fn set_file_tree_focus(&mut self, focused: bool) -> bool {
        let focused = focused && self.file_tree_shown();
        let changed = self.file_tree.focused != focused;
        self.file_tree.focused = focused;
        // A name half typed is dropped when the keyboard goes elsewhere.
        if changed && !focused {
            if let Some(tab_id) = self.file_tree_active_tab() {
                self.file_tree_cancel_edit(tab_id);
            }
        }
        changed
    }

    fn file_tree_scale(&self) -> f32 {
        sidebar_width_scale_for_dpi(self.dimensions.dpi as f64)
    }

    fn file_tree_list_options(&self) -> ListOptions {
        ListOptions {
            show_hidden: self
                .file_tree
                .show_hidden
                .unwrap_or(self.config.file_browser.show_hidden),
            respect_gitignore: self.config.file_browser.respect_gitignore,
        }
    }

    /// Where the panel is right now, or `None` when it is hidden or the
    /// window is too narrow for it.
    pub fn file_tree_geometry(&self) -> Option<RectF> {
        if !self.file_tree_shown() {
            return None;
        }
        let area = self.side_panel_area();
        let scale = self.file_tree_scale();
        let area_w = area.right - area.left;
        let area_h = area.bottom - area.top;
        let min = MIN_WIDTH_PX * scale;
        let max = (area_w * MAX_WIDTH_SHARE).floor();
        if !(area_w.is_finite() && area_h.is_finite()) || max < min || area_h <= 0. {
            return None;
        }
        let width = self
            .file_tree
            .width
            .map(|w| w as f32)
            .unwrap_or(self.config.file_browser.width_px as f32 * scale)
            .clamp(min, max)
            .floor();
        Some(euclid::rect(area.left, area.top, width, area_h))
    }

    /// Pixels of width the terminal gives up for the panel, frozen while its
    /// edge is dragged so the pane reflows once, on release.
    pub fn file_tree_reserved_width(&self) -> usize {
        if let Some((item, _)) = self.dragging.as_ref() {
            if let UIItemType::FileTreeResize { start_reserved } = item.item_type {
                return start_reserved;
            }
        }
        self.file_tree_geometry()
            .map(|g| g.size.width as usize)
            .unwrap_or(0)
    }

    /// [`Self::file_tree_reserved_width`] for the layout pass, which also
    /// records what it used.
    pub fn file_tree_layout_width(&self) -> usize {
        let width = self.file_tree_reserved_width();
        self.file_tree.layout_width.set(width);
        width
    }

    /// Queue a layout pass when the reservation changed without one, e.g.
    /// after switching to a tab where the panel is in the other state.
    fn file_tree_sync_layout(&self) {
        if self.dragging.is_some() || self.file_tree.relayout_queued.get() {
            return;
        }
        if self.file_tree_reserved_width() == self.file_tree.layout_width.get() {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        self.file_tree.relayout_queued.set(true);
        window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
            term_window.file_tree.relayout_queued.set(false);
            term_window.relayout_for_diff_panel();
            let width = term_window.file_tree_reserved_width();
            term_window.file_tree.layout_width.set(width);
        })));
    }

    /// Whether `pane` is somewhere the panel cannot list yet: a pane on
    /// another machine. Those keep the fzf picker.
    fn file_tree_needs_picker(&self, pane: &Arc<dyn Pane>) -> bool {
        let local = Mux::get()
            .get_domain(pane.domain_id())
            .is_some_and(|domain| domain.downcast_ref::<mux::domain::LocalDomain>().is_some());
        if !local {
            return true;
        }
        let (remote, looks_remote) = self.file_browser_remote_context(pane);
        remote.is_some() || looks_remote
    }

    fn file_tree_close(&mut self, tab_id: TabId) {
        self.file_tree.views.remove(&tab_id);
        self.file_tree.focused = false;
        self.file_tree.menu = None;
        self.relayout_for_diff_panel();
    }

    /// The Worktree button and `OpenFileBrowser`: show or hide the panel for
    /// the tab `pane` is in, or open the picker where the panel cannot go.
    pub fn toggle_file_tree(&mut self, pane: &Arc<dyn Pane>) {
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        if self.file_tree.views.contains_key(&tab_id) {
            self.file_tree_close(tab_id);
            return;
        }
        // A picker already open in this tab: the button closes it, as before.
        if self.config.file_browser.ui == config::FileBrowserUi::Picker
            || self.find_worktree_pane().is_some()
        {
            self.open_file_browser(pane);
            return;
        }
        let target = self.file_browser_target_pane(pane);
        if self.file_tree_needs_picker(&target) {
            self.open_file_browser(pane);
            return;
        }
        let mux = Mux::get();
        self.file_tree
            .views
            .retain(|tab, _| mux.get_tab(*tab).is_some());
        self.file_tree
            .views
            .insert(tab_id, TreeView::new(target.pane_id()));
        self.relayout_for_diff_panel();
    }

    /// `FocusFileTree`: open the panel when it is closed, then give it the
    /// keyboard.
    pub fn focus_file_tree(&mut self, pane: &Arc<dyn Pane>) {
        if !self.file_tree_shown() {
            self.toggle_file_tree(pane);
        }
        if self.file_tree_shown() {
            self.file_tree.focused = true;
            if let Some(tab_id) = self.file_tree_active_tab() {
                if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                    if view.selected.is_none() {
                        let first = view.rows.first().map(|row| row.rel.clone());
                        view.select_only(first);
                    }
                }
            }
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    /// The pane the tree follows, re-picked when the one it had is gone.
    fn file_tree_target(&mut self, tab_id: TabId) -> Option<Arc<dyn Pane>> {
        let mux = Mux::get();
        let view = self.file_tree.views.get(&tab_id)?;
        if let Some(pane) = mux.get_pane(view.target) {
            return Some(pane);
        }
        let active = self.get_active_pane_or_overlay()?;
        let pane = self.file_browser_target_pane(&active);
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            view.target = pane.pane_id();
        }
        Some(pane)
    }

    /// Read the tree again when the one on show has gone stale, and save
    /// open folders that have settled. Reading runs on its own thread.
    fn file_tree_refresh(&mut self, tab_id: TabId) {
        let now = Instant::now();
        self.file_tree_save_expanded(tab_id, now);
        let Some(view) = self.file_tree.views.get(&tab_id) else {
            return;
        };
        let changed = view.watch.as_ref().is_some_and(|watch| watch.is_dirty());
        let ttl = if changed {
            WATCH_SETTLE.max(view.scan_cost * 2)
        } else {
            REFRESH.max(view.scan_cost * SCAN_DUTY_DIVISOR)
        };
        // Come back when the next read is due even if nothing else repaints.
        self.update_next_frame_time(Some(now + ttl));
        if !herd_scan_is_due(
            self.file_tree.scan_started_at.get(),
            view.scanned_at,
            ttl,
            SCAN_WATCHDOG,
            now,
        ) {
            return;
        }
        let Some(pane) = self.file_tree_target(tab_id) else {
            return;
        };
        let target = self.diff_panel_target(&pane);
        // `None` while the herd is busy: the marks on show are kept.
        let sessions = self.diff_panel_sessions(tab_id);
        let Some(view) = self.file_tree.views.get(&tab_id) else {
            return;
        };
        let current_root = view.tree.as_ref().map(|tree| tree.root.clone());
        let expanded = view.expanded.clone();
        let want_index = view.filter.is_some();
        let watched = view.watch.as_ref().map(|watch| {
            watch.clear();
            watch.root().to_path_buf()
        });
        let Some(window) = self.window.clone() else {
            return;
        };
        let other_distro = Self::default_wsl_distro();
        let options = self.file_tree_list_options();
        self.file_tree.scan_started_at.set(Some(now));
        let spawned = std::thread::Builder::new()
            .name("file-tree-scan".into())
            .spawn(move || {
                let settled = target.and_then(|target| settle_scan_target(target, other_distro));
                let (root, runner) = match settled {
                    Ok((dir, runner)) => {
                        let root = crate::diff_panel::detect_vcs(&dir)
                            .map(|(_, root)| root)
                            .unwrap_or(dir);
                        (root, runner)
                    }
                    Err(reason) => {
                        let result = ScanResult {
                            tree: Err(reason),
                            remembered: None,
                            status: None,
                            marks: None,
                            index: None,
                            watch: None,
                        };
                        window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                            term_window.file_tree_publish(tab_id, now, result);
                        })));
                        return;
                    }
                };
                let same_root = current_root.as_ref() == Some(&root);
                let remembered = (!same_root).then(|| {
                    tgz_ui_state::load_file_tree_expanded(&root.to_string_lossy())
                        .into_iter()
                        .collect::<HashSet<String>>()
                });
                let open = remembered.as_ref().unwrap_or(&expanded);
                let tree = list_open(root.clone(), open, options, list_dir);
                let is_git = root.join(".git").exists();
                let status = is_git
                    .then(|| {
                        runner
                            .run("git", &["status", "--porcelain=v1", "-z"], &root)
                            .ok()
                    })
                    .flatten()
                    .filter(|out| out.success && !out.truncated)
                    .map(|out| GitStatus::parse(&out.stdout));
                let marks = sessions.map(|sessions| {
                    let mut marks = AgentMarks::default();
                    for session in &sessions {
                        session.mark_tree(&root, &mut marks);
                    }
                    marks
                });
                let index =
                    want_index.then(|| path_index(&root, options, is_git.then_some(&runner)));
                // Watched where notifications arrive: not across the WSL
                // filesystem bridge, where only the poll is reliable.
                let watch = (watched.as_deref() != Some(root.as_path())
                    && wsl_paths::distro_of_unc(&root.to_string_lossy()).is_none())
                .then(|| {
                    let window = window.clone();
                    Watch::start(&root, move || {
                        window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                            if let Some(window) = term_window.window.as_ref() {
                                window.invalidate();
                            }
                        })));
                    })
                    .map_err(|err| log::debug!("file tree: not watching: {err:#}"))
                    .ok()
                })
                .flatten();
                let result = ScanResult {
                    tree: Ok(tree),
                    remembered,
                    status,
                    marks,
                    index,
                    watch,
                };
                window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    term_window.file_tree_publish(tab_id, now, result);
                })));
            });
        if let Err(err) = spawned {
            log::warn!("file tree: could not start a read: {err:#}");
            self.file_tree.scan_started_at.set(None);
        }
    }

    fn file_tree_publish(&mut self, tab_id: TabId, started: Instant, result: ScanResult) {
        self.file_tree.scan_started_at.set(None);
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        view.scanned_at = Some(started);
        view.scan_cost = started.elapsed();
        let before = (
            view.tree.clone(),
            view.error.clone(),
            view.status.clone(),
            view.marks.clone(),
        );
        match result.tree {
            Ok(tree) => {
                if let Some(remembered) = result.remembered {
                    // A new root: its own open folders, nothing selected,
                    // and a filter's file list that no longer applies.
                    view.expanded = remembered;
                    view.select_only(None);
                    view.scroll = 0;
                    if let Some(filter) = view.filter.as_mut() {
                        filter.index = None;
                    }
                }
                tree_view::prune_expanded(&tree, &mut view.expanded);
                view.tree = Some(tree);
                view.error = None;
            }
            Err(reason) => {
                view.tree = None;
                view.error = Some(reason);
            }
        }
        view.status = result.status;
        if let Some(marks) = result.marks {
            view.marks = marks;
        }
        if result.watch.is_some() {
            view.watch = result.watch;
        }
        let mut changed = before
            != (
                view.tree.clone(),
                view.error.clone(),
                view.status.clone(),
                view.marks.clone(),
            );
        match (view.filter.as_mut(), result.index) {
            (Some(filter), Some((index, cut))) if filter.index.as_ref() != Some(&index) => {
                filter.index = Some(index);
                filter.cut = cut;
                view.refilter(false);
                changed = true;
            }
            _ => view.rebuild(),
        }
        // A folder opened while this read ran is read at once.
        if view.filter.is_none()
            && view
                .tree
                .as_ref()
                .is_some_and(|tree| !tree_view::unlisted(tree, &view.rows).is_empty())
        {
            view.scanned_at = None;
        }
        if changed || view.scanned_at.is_none() {
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    fn file_tree_save_expanded(&mut self, tab_id: TabId, now: Instant) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        let Some(changed_at) = view.expanded_changed_at else {
            return;
        };
        if now.duration_since(changed_at) < SAVE_SETTLE {
            self.update_next_frame_time(Some(changed_at + SAVE_SETTLE));
            return;
        }
        view.expanded_changed_at = None;
        let Some(root) = view.root_key() else {
            return;
        };
        let mut folders: Vec<String> = view.expanded.iter().cloned().collect();
        folders.sort();
        // The state file is read and rewritten; keep that off the GUI thread.
        std::thread::spawn(move || {
            static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _guard = WRITE.lock();
            tgz_ui_state::save_file_tree_expanded(&root, folders);
        });
    }

    /// Open or close the folder `rel`: in the filter's tree while it shows
    /// one, else in the tree itself.
    fn file_tree_toggle_folder(&mut self, tab_id: TabId, rel: &str) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        if let Some(filter) = view.filter.as_mut().filter(|filter| filter.tree.is_some()) {
            if !filter.expanded.remove(rel) {
                filter.expanded.insert(rel.to_string());
            }
        } else {
            if !view.expanded.remove(rel) {
                view.expanded.insert(rel.to_string());
                // Read it now rather than at the next poll.
                view.scanned_at = None;
            }
            view.expanded_changed_at = Some(Instant::now());
        }
        view.rebuild();
    }

    fn file_tree_collapse_all(&mut self, tab_id: TabId) {
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            match view.filter.as_mut().filter(|filter| filter.tree.is_some()) {
                Some(filter) => filter.expanded.clear(),
                None => {
                    view.expanded.clear();
                    view.expanded_changed_at = Some(Instant::now());
                }
            }
            view.scroll = 0;
            view.rebuild();
        }
    }

    /// Enter on a row: a folder opens or closes, a file opens in the editor.
    fn file_tree_activate(&mut self, tab_id: TabId, rel: &str) {
        let kind = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.rows.iter().find(|row| row.rel == rel))
            .map(|row| row.kind);
        match kind {
            Some(EntryKind::Dir) => self.file_tree_toggle_folder(tab_id, rel),
            Some(EntryKind::File) => self.file_tree_open_file(tab_id, rel, false),
            None => {}
        }
    }

    /// Where an editor started for the tree's files runs.
    fn file_tree_editor_host(&self, root: &std::path::Path, target: &Arc<dyn Pane>) -> EditorHost {
        if let Some(distro) = wsl_paths::distro_of_unc(&root.to_string_lossy()) {
            let own_domain = Mux::get()
                .get_domain(target.domain_id())
                .and_then(|domain| wsl_paths::distro_for_domain(domain.domain_name(), &self.config))
                .is_some_and(|pane_distro| pane_distro.eq_ignore_ascii_case(&distro));
            return EditorHost::Wsl { distro, own_domain };
        }
        if cfg!(windows) {
            EditorHost::Windows
        } else {
            EditorHost::Unix {
                shell: std::env::var("SHELL").ok(),
            }
        }
    }

    /// Open the file `rel` in the configured editor: beside the target pane,
    /// or in a new tab with `in_tab`.
    ///
    /// A `file-tree-open-file` handler in the Lua config sees the pick first,
    /// as `(window, pane, path)` with the pane the tree follows; returning
    /// `false` from it skips the default.
    fn file_tree_open_file(&mut self, tab_id: TabId, rel: &str, in_tab: bool) {
        let Some(target) = self.file_tree_target(tab_id) else {
            return;
        };
        let Some(path) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| tree.abs_path(rel))
        else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let in_tab = in_tab
            || self.config.file_browser.editor_placement == config::FileBrowserEditorPlacement::Tab;
        // The editor takes the keyboard from here.
        self.file_tree.focused = false;
        let gui_win = crate::scripting::guiwin::GuiWin::new(self);
        let pane = mux_lua::MuxPane(target.pane_id());

        async fn ask_lua(
            lua: Option<std::rc::Rc<mlua::Lua>>,
            gui_win: crate::scripting::guiwin::GuiWin,
            pane: mux_lua::MuxPane,
            path: PathBuf,
        ) -> anyhow::Result<bool> {
            let Some(lua) = lua else {
                return Ok(true);
            };
            let args = lua.pack_multi((gui_win, pane, path.to_string_lossy().to_string()))?;
            config::lua::emit_event(&lua, ("file-tree-open-file".to_string(), args))
                .await
                .map_err(|err| {
                    log::error!("while processing file-tree-open-file event: {err:#}");
                    anyhow::anyhow!("{err:#}")
                })
        }

        promise::spawn::spawn(config::with_lua_config_on_main_thread(
            move |lua| async move {
                if ask_lua(lua, gui_win, pane, path.clone())
                    .await
                    .unwrap_or(true)
                {
                    window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        term_window.file_tree_open_path(tab_id, path, in_tab);
                    })));
                }
                Ok::<(), anyhow::Error>(())
            },
        ))
        .detach();
    }

    /// Open `path` in the configured editor: [`Self::file_tree_open_file`]
    /// once Lua has let it.
    fn file_tree_open_path(&mut self, tab_id: TabId, path: PathBuf, in_tab: bool) {
        let Some(target) = self.file_tree_target(tab_id) else {
            return;
        };
        let Some(tree) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
        else {
            return;
        };
        let host = self.file_tree_editor_host(&tree.root, &target);
        let config = &self.config.file_browser;
        let launch = resolve_editor(
            config.editor_command.as_deref(),
            config.editor_is_gui,
            &host,
            &path,
        );
        match launch {
            EditorLaunch::OpenWithSystem(path) => wezterm_open_url::open_url(&path),
            EditorLaunch::Detached { argv } => {
                let dir = path.parent().map(|dir| dir.to_path_buf());
                std::thread::spawn(move || {
                    let mut command = std::process::Command::new(&argv[0]);
                    command.args(&argv[1..]);
                    if let Some(dir) = dir {
                        command.current_dir(dir);
                    }
                    if let Err(err) = command.spawn() {
                        log::warn!("file tree: could not start the editor: {err:#}");
                        wezterm_toast_notification::show(
                            wezterm_toast_notification::ToastNotification {
                                title: "Editor unavailable".to_string(),
                                message: format!("{}: {err}", argv[0]),
                                url: None,
                                timeout: Some(Duration::from_millis(4000)),
                            },
                        );
                    }
                });
            }
            EditorLaunch::Split { argv, cwd } => {
                let through_wsl_exe = wsl_paths::wsl_exec_distro(&argv).is_some();
                let spawn = SpawnCommand {
                    label: path
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string()),
                    args: Some(argv),
                    cwd: cwd.map(PathBuf::from),
                    domain: if through_wsl_exe {
                        SpawnTabDomain::DomainName("local".to_string())
                    } else {
                        SpawnTabDomain::CurrentPaneDomain
                    },
                    ..Default::default()
                };
                if in_tab {
                    self.spawn_command(&spawn, crate::spawn::SpawnWhere::NewTab);
                    return;
                }
                if self.file_tree_reuse_editor(tab_id, &host, &path) {
                    return;
                }
                let pane_id = target.pane_id();
                let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
                let window = self.window.clone();
                promise::spawn::spawn(async move {
                    let request = SplitRequest {
                        direction: SplitDirection::Horizontal,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    };
                    match split_spawn(spawn, pane_id, request, term_config).await {
                        Ok((tab, pane)) => {
                            tab.set_active_pane(&pane);
                            // Remembered, so the next file can go to it.
                            let editor = pane.pane_id();
                            if let Some(window) = window {
                                window.notify(TermWindowNotif::Apply(Box::new(
                                    move |term_window| {
                                        term_window.file_tree.editors.insert(tab_id, editor);
                                    },
                                )));
                            }
                        }
                        Err(err) => log::error!("file tree: could not open the editor: {err:#}"),
                    }
                })
                .detach();
            }
        }
    }

    /// The Changes panel, scrolled to the file on show in the preview.
    fn file_tree_show_changes(&mut self, tab_id: TabId) {
        let Some(path) = self.file_tree.views.get(&tab_id).and_then(|view| {
            let rel = view.preview.as_ref()?.shown()?;
            Some(view.tree.as_ref()?.abs_path(rel))
        }) else {
            return;
        };
        let Some(target) = self.file_tree_target(tab_id) else {
            return;
        };
        self.diff_panel_focus_path(target.pane_id(), path);
    }

    /// With `reuse_editor_pane`, show `path` in the vim this panel opened
    /// earlier in this tab, if it is still running there, instead of opening
    /// another split. `false` when there is none to reuse.
    fn file_tree_reuse_editor(
        &mut self,
        tab_id: TabId,
        host: &EditorHost,
        path: &std::path::Path,
    ) -> bool {
        use crate::file_tree::editor::{editor_program, is_vim, vim_drop_keys};
        let config = &self.config.file_browser;
        if !config.reuse_editor_pane || !is_vim(&editor_program(config.editor_command.as_deref())) {
            return false;
        }
        let mux = Mux::get();
        let Some(editor) = self
            .file_tree
            .editors
            .get(&tab_id)
            .and_then(|pane_id| mux.get_pane(*pane_id))
        else {
            return false;
        };
        let in_tab = mux
            .resolve_pane_id(editor.pane_id())
            .is_some_and(|(_, _, tab)| tab == tab_id);
        // Only into a vim: the editor may have quit, leaving a shell (or
        // nothing) in the pane, and keys meant for vim must not reach that.
        let running_vim = self
            .pane_command_basename(&editor)
            .is_some_and(|name| is_vim(&name));
        if !in_tab || !running_vim {
            self.file_tree.editors.remove(&tab_id);
            return false;
        }
        let native = path.to_string_lossy().to_string();
        let shown = match host {
            EditorHost::Wsl { distro, .. } => {
                wsl_paths::windows_to_wsl(&native, distro).unwrap_or(native)
            }
            _ => native,
        };
        if let Err(err) = editor.writer().write_all(vim_drop_keys(&shown).as_bytes()) {
            log::warn!("file tree: could not reach the editor: {err:#}");
            return false;
        }
        if let Some(tab) = mux.get_tab(tab_id) {
            tab.set_active_pane(&editor);
        }
        true
    }

    /// Select row `index` and scroll it into view.
    fn file_tree_select(&mut self, tab_id: TabId, index: usize) {
        let visible = self.file_tree.visible_rows.get();
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            if let Some(row) = view.rows.get(index) {
                let rel = row.rel.clone();
                view.select_only(Some(rel));
                view.scroll = tree_view::scroll_to_show(view.scroll, index, visible);
            }
        }
    }

    fn file_tree_open_filter(&mut self, tab_id: TabId) {
        self.file_tree_cancel_edit(tab_id);
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            if view.filter.is_none() {
                view.filter = Some(FilterView::default());
                // Read the file list now.
                view.scanned_at = None;
            }
        }
    }

    /// Close the filter, landing on the selected file in the full tree.
    fn file_tree_close_filter(&mut self, tab_id: TabId) {
        let visible = self.file_tree.visible_rows.get();
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        if view.filter.take().is_none() {
            return;
        }
        if let Some(rel) = view.selected.clone() {
            view.reveal(&rel);
        }
        view.rebuild();
        if let Some(index) = view.selected_index() {
            view.scroll = tree_view::scroll_to_show(view.scroll, index, visible);
        }
    }

    /// Keys while the panel has the keyboard. `false` lets the key go on to
    /// the terminal: anything held with Ctrl, Alt or Super, so bindings keep
    /// working.
    pub fn file_tree_key_input(
        &mut self,
        key: KeyCode,
        modifiers: Modifiers,
        context: &dyn WindowOps,
    ) -> bool {
        if !self.file_tree_focused()
            || modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
        {
            return false;
        }
        let Some(tab_id) = self.file_tree_active_tab() else {
            return false;
        };
        // A key closes the menu; Escape does nothing else.
        if self.close_file_tree_menu() {
            context.invalidate();
            if key == KeyCode::Escape {
                return true;
            }
        }
        let (editing, typing) = self
            .file_tree
            .views
            .get(&tab_id)
            .map_or((false, false), |view| {
                (view.edit.is_some(), view.filter.is_some())
            });
        let handled = if editing {
            self.file_tree_edit_key(tab_id, &key)
        } else if typing {
            self.file_tree_filter_key(tab_id, &key)
        } else {
            match tree_key(&key) {
                Some(action) => {
                    self.file_tree_tree_key(tab_id, action);
                    true
                }
                None => false,
            }
        };
        if handled {
            context.invalidate();
        }
        handled
    }

    /// Composed (IME, dead-key) text while the panel has the keyboard: typed
    /// into the filter when it is open, dropped otherwise.
    pub fn file_tree_insert_str(&mut self, text: &str) {
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        if self.file_tree_edit_insert(tab_id, text) {
            return;
        }
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            if let Some(filter) = view.filter.as_mut() {
                filter
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                view.refilter(true);
            }
        }
    }

    /// A key while the filter is being typed into: text edits the query,
    /// arrows and Enter still move through and open the matches.
    fn file_tree_filter_key(&mut self, tab_id: TabId, key: &KeyCode) -> bool {
        let action = match key {
            KeyCode::UpArrow => Some(TreeKey::Up),
            KeyCode::DownArrow => Some(TreeKey::Down),
            KeyCode::LeftArrow => Some(TreeKey::Left),
            KeyCode::RightArrow => Some(TreeKey::Right),
            KeyCode::PageUp => Some(TreeKey::PageUp),
            KeyCode::PageDown => Some(TreeKey::PageDown),
            KeyCode::Enter => Some(TreeKey::Activate),
            _ => None,
        };
        if let Some(action) = action {
            self.file_tree_tree_key(tab_id, action);
            return true;
        }
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return false;
        };
        let Some(filter) = view.filter.as_mut() else {
            return false;
        };
        match key {
            KeyCode::Escape => {
                self.file_tree_close_filter(tab_id);
                return true;
            }
            KeyCode::Backspace => {
                if filter.query.pop().is_none() {
                    self.file_tree_close_filter(tab_id);
                    return true;
                }
            }
            KeyCode::Char(c) if !c.is_control() => filter.query.push(*c),
            _ => return false,
        }
        view.refilter(true);
        true
    }

    fn file_tree_tree_key(&mut self, tab_id: TabId, action: TreeKey) {
        let visible = self.file_tree.visible_rows.get().max(1);
        let Some(view) = self.file_tree.views.get(&tab_id) else {
            return;
        };
        let len = view.rows.len();
        let selected = view.selected_index();
        let selected_rel = view.selected.clone();
        let step = |delta: isize| tree_view::moved(selected, delta, len);
        let target = match action {
            TreeKey::Up => step(-1).map(Nav::Select),
            TreeKey::Down => step(1).map(Nav::Select),
            TreeKey::PageUp => step(-(visible as isize)).map(Nav::Select),
            TreeKey::PageDown => step(visible as isize).map(Nav::Select),
            TreeKey::First => (len > 0).then_some(Nav::Select(0)),
            TreeKey::Last => len.checked_sub(1).map(Nav::Select),
            TreeKey::Left => selected.map(|at| tree_view::left(&view.rows, at)),
            TreeKey::Right => selected.map(|at| tree_view::right(&view.rows, at)),
            TreeKey::Toggle => match selected.and_then(|at| view.rows.get(at)) {
                Some(row) if row.kind == EntryKind::Dir => Some(Nav::Toggle(row.rel.clone())),
                // Space on a file: the quick preview.
                _ => {
                    self.file_tree_toggle_preview(tab_id);
                    None
                }
            },
            TreeKey::Preview => {
                self.file_tree_toggle_preview(tab_id);
                None
            }
            TreeKey::Activate => {
                if let Some(rel) = selected_rel {
                    self.file_tree_activate(tab_id, &rel);
                }
                None
            }
            TreeKey::StartFilter => {
                self.file_tree_open_filter(tab_id);
                None
            }
            TreeKey::CollapseAll => {
                self.file_tree_collapse_all(tab_id);
                None
            }
            TreeKey::Refresh => {
                if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                    view.scanned_at = None;
                }
                None
            }
            TreeKey::ToggleHidden => {
                let shown = self
                    .file_tree
                    .show_hidden
                    .unwrap_or(self.config.file_browser.show_hidden);
                self.file_tree.show_hidden = Some(!shown);
                for view in self.file_tree.views.values_mut() {
                    view.scanned_at = None;
                }
                None
            }
            TreeKey::Run(action) => {
                let targets = self
                    .file_tree
                    .views
                    .get(&tab_id)
                    .map(|view| view.marked_rows())
                    .unwrap_or_default();
                // Rename and trash need something selected; the rest act on
                // the root without.
                let needs_target = matches!(
                    action,
                    FileTreeMenuAction::Rename
                        | FileTreeMenuAction::Trash
                        | FileTreeMenuAction::CopyRelativePath
                );
                if !(needs_target && targets.is_empty()) {
                    self.file_tree_run(tab_id, action, targets);
                }
                None
            }
            TreeKey::Leave => {
                self.file_tree.focused = false;
                None
            }
            TreeKey::Swallow => None,
        };
        match target {
            Some(Nav::Select(index)) => self.file_tree_select(tab_id, index),
            Some(Nav::Toggle(rel)) => self.file_tree_toggle_folder(tab_id, &rel),
            Some(Nav::Nothing) | None => {}
        }
    }

    pub fn mouse_event_file_tree_resize(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::EwResize));
        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    pub fn drag_file_tree_resize(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::EwResize));
        let area = self.side_panel_area();
        let scale = self.file_tree_scale();
        let width = (event.coords.x as f32 - area.left).max(MIN_WIDTH_PX * scale) as usize;
        if self.file_tree.width != Some(width) {
            self.file_tree.width = Some(width);
            self.quad_generation += 1;
            context.invalidate();
        }
        self.dragging.replace((item, start_event));
    }

    /// The drag is over: settle the width, remember it, and let the terminal
    /// take up whatever the panel now leaves it.
    pub fn finish_file_tree_resize(&mut self) {
        if let Some(geometry) = self.file_tree_geometry() {
            self.file_tree.width = Some(geometry.size.width as usize);
        }
        let width = self.file_tree.width;
        std::thread::spawn(move || tgz_ui_state::save_file_tree_width(width));
        self.relayout_for_diff_panel();
    }

    /// Clicks and wheel on the panel. `target` is the header button or the
    /// row under the pointer, if any.
    pub fn mouse_event_file_tree(
        &mut self,
        target: Option<Result<FileTreeAction, usize>>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        if let WMEK::VertWheel(amount) = event.kind {
            let visible = self.file_tree.visible_rows.get();
            if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                let next = crate::diff_panel::view::scrolled(
                    view.scroll,
                    amount as isize,
                    view.rows.len(),
                    visible,
                );
                if next != view.scroll {
                    view.scroll = next;
                    context.invalidate();
                }
            }
            return;
        }
        if event.kind == WMEK::Press(MousePress::Right) && target.is_none() {
            self.file_tree_open_menu(None, &event);
            context.invalidate();
            return;
        }
        if event.kind != WMEK::Release(MousePress::Left) {
            return;
        }
        self.pressed_ui_item = None;
        match target {
            None => {}
            Some(Ok(FileTreeAction::Close)) => self.file_tree_close(tab_id),
            Some(Ok(FileTreeAction::Refresh)) => {
                if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                    view.scanned_at = None;
                }
            }
            Some(Ok(FileTreeAction::CollapseAll)) => self.file_tree_collapse_all(tab_id),
            Some(Ok(FileTreeAction::Preview)) => self.file_tree_toggle_preview(tab_id),
            Some(Ok(FileTreeAction::ShowChanges)) => self.file_tree_show_changes(tab_id),
            Some(Ok(FileTreeAction::Filter)) => {
                let open = self
                    .file_tree
                    .views
                    .get(&tab_id)
                    .is_some_and(|view| view.filter.is_some());
                if open {
                    self.file_tree_close_filter(tab_id);
                } else {
                    self.file_tree_open_filter(tab_id);
                }
            }
            Some(Err(index)) => self.file_tree_click_row(tab_id, index, event.modifiers),
        }
        context.invalidate();
    }

    /// A click on a folder opens or closes it; a click on a file selects it,
    /// and a second click on the same file soon after opens it. Cmd-click
    /// (Ctrl-click off macOS) adds a row to the selection or takes it out,
    /// Shift-click selects the run of rows from the last one clicked.
    fn file_tree_click_row(&mut self, tab_id: TabId, index: usize, modifiers: Modifiers) {
        let Some(row) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.rows.get(index))
            .cloned()
        else {
            return;
        };
        let toggle = if cfg!(target_os = "macos") {
            Modifiers::SUPER
        } else {
            Modifiers::CTRL
        };
        if modifiers.intersects(toggle | Modifiers::SHIFT) {
            self.file_tree.last_click = None;
            if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                if modifiers.contains(Modifiers::SHIFT) {
                    let anchor = view
                        .anchor
                        .clone()
                        .or_else(|| view.selected.clone())
                        .unwrap_or_else(|| row.rel.clone());
                    view.marked = tree_view::range(&view.rows, &anchor, &row.rel)
                        .into_iter()
                        .collect();
                    view.anchor = Some(anchor);
                    view.selected = Some(row.rel.clone());
                } else {
                    if view.marked.remove(&row.rel) {
                        // The keyboard moves to another selected row, if any.
                        if view.selected.as_deref() == Some(row.rel.as_str()) {
                            view.selected = view.marked_rows().first().map(|(rel, _)| rel.clone());
                        }
                    } else {
                        view.marked.insert(row.rel.clone());
                        view.selected = Some(row.rel.clone());
                    }
                    view.anchor = Some(row.rel.clone());
                }
            }
            return;
        }
        let now = Instant::now();
        let double = self
            .file_tree
            .last_click
            .as_ref()
            .is_some_and(|(rel, at)| *rel == row.rel && now.duration_since(*at) < DOUBLE_CLICK);
        self.file_tree.last_click = (!double).then(|| (row.rel.clone(), now));
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            view.select_only(Some(row.rel.clone()));
        }
        match row.kind {
            EntryKind::Dir => self.file_tree_toggle_folder(tab_id, &row.rel),
            EntryKind::File if double => self.file_tree_open_file(tab_id, &row.rel, false),
            EntryKind::File => {}
        }
    }

    pub fn paint_file_tree(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        self.file_tree_sync_layout();
        let Some(geometry) = self.file_tree_geometry() else {
            return Ok(());
        };
        let Some(tab_id) = self.file_tree_active_tab() else {
            return Ok(());
        };
        self.file_tree_refresh(tab_id);
        let focused = self.file_tree_focused();

        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        let gap = FLOAT_GAP * dpi_scale;
        let pad_x = PAD_X * dpi_scale;
        let radius = RADIUS * dpi_scale;
        let stroke = dpi_scale.max(1.);
        let grip = GRIP * dpi_scale;

        let card: RectF = euclid::rect(
            geometry.origin.x + gap,
            geometry.origin.y + gap,
            (geometry.size.width - gap * 2.).max(1.),
            (geometry.size.height - gap * 2.).max(1.),
        );
        let sb = self.sidebar_palette();
        let accent = sb.accent();
        let surface = opaque(sb.surface);
        // With the keyboard, the card's edge takes on the accent.
        let border = opaque(if focused {
            lerp_rgba(sb.surface, accent, 0.7)
        } else {
            lerp_rgba(sb.surface, sb.text_meta, 0.35)
        });
        self.sidebar_rounded_fill(layers, 1, card, radius, border)?;
        self.sidebar_rounded_fill(
            layers,
            1,
            euclid::rect(
                card.origin.x + stroke,
                card.origin.y + stroke,
                (card.size.width - stroke * 2.).max(1.),
                (card.size.height - stroke * 2.).max(1.),
            ),
            (radius - stroke).max(0.),
            surface,
        )?;

        // Registered first, so everything pushed after it wins the hit test.
        self.ui_items.push(UIItem {
            x: geometry.origin.x.max(0.) as usize,
            y: geometry.origin.y.max(0.) as usize,
            width: geometry.size.width as usize,
            height: geometry.size.height as usize,
            item_type: UIItemType::FileTreeBody,
        });

        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());

        // The draggable right edge, drawn as a pill while it is pointed at.
        let resize = UIItemType::FileTreeResize {
            start_reserved: self.file_tree_reserved_width(),
        };
        let resize_hot = matches!(hovered, Some(UIItemType::FileTreeResize { .. }))
            || matches!(
                self.dragging.as_ref().map(|(item, _)| &item.item_type),
                Some(UIItemType::FileTreeResize { .. })
            );
        if resize_hot {
            let pill_h = (card.size.height * 0.12).clamp(24. * dpi_scale, 64. * dpi_scale);
            self.sidebar_pill_fill(
                layers,
                2,
                euclid::rect(
                    card.max_x() - stroke * 0.5 - 1.5 * dpi_scale,
                    card.origin.y + (card.size.height - pill_h) * 0.5,
                    3. * dpi_scale,
                    pill_h,
                ),
                1.5 * dpi_scale,
                accent,
            )?;
        }
        self.ui_items.push(UIItem {
            x: (geometry.max_x() - grip).max(0.) as usize,
            y: geometry.origin.y.max(0.) as usize,
            width: grip as usize,
            height: geometry.size.height as usize,
            item_type: resize,
        });

        // Header: the root's name on the left, buttons on the right.
        let header_h = (cell_h + 12. * dpi_scale).ceil();
        let header_y = card.origin.y + stroke;
        let text_y = header_y + (header_h - cell_h) * 0.5;
        let button_h = (cell_h + 4. * dpi_scale).min(header_h - 4. * dpi_scale);
        let button_y = header_y + (header_h - button_h) * 0.5;
        let (filter_open, preview_open) = self
            .file_tree
            .views
            .get(&tab_id)
            .map_or((false, false), |view| {
                (view.filter.is_some(), view.preview.is_some())
            });
        let mut right = card.max_x() - pad_x * 0.6;
        for (label, action) in [
            ("\u{00d7}", FileTreeAction::Close),
            ("\u{f021}", FileTreeAction::Refresh),
            ("\u{f066}", FileTreeAction::CollapseAll),
            ("\u{f002}", FileTreeAction::Filter),
            ("\u{f06e}", FileTreeAction::Preview),
        ] {
            let label_w = cell_w;
            let button_w = (label_w + 10. * dpi_scale).max(button_h);
            let button_x = right - button_w;
            if button_x < card.origin.x + pad_x + 6. * cell_w {
                break;
            }
            let item_type = UIItemType::FileTreeButton(action);
            let on = (action == FileTreeAction::Filter && filter_open)
                || (action == FileTreeAction::Preview && preview_open);
            let button_bg = if hovered.as_ref() == Some(&item_type) {
                opaque(sb.pressed_fill)
            } else if on {
                opaque(lerp_rgba(surface, sb.active_fill, 0.9))
            } else {
                surface
            };
            if button_bg != surface {
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(button_x, button_y, button_w, button_h),
                    5. * dpi_scale,
                    button_bg,
                )?;
            }
            self.diff_panel_text(
                layers,
                label,
                button_x + (button_w - label_w) * 0.5,
                button_y + (button_h - cell_h) * 0.5,
                label_w,
                if on {
                    accent
                } else {
                    contrast_label_color(button_bg, sb.text_meta)
                },
                button_bg,
                false,
            )?;
            self.ui_items.push(UIItem {
                x: button_x as usize,
                y: button_y as usize,
                width: button_w as usize,
                height: button_h as usize,
                item_type,
            });
            right = button_x - gap * 0.5;
        }
        let title = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| {
                tree.root
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| tree.root.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "Worktree".to_string());
        let title_x = card.origin.x + pad_x;
        self.diff_panel_text(
            layers,
            &title,
            title_x,
            text_y,
            (right - title_x).max(0.),
            sb.text_active,
            surface,
            true,
        )?;
        let mut divider_y = header_y + header_h;

        let row_h = (cell_h + 6. * dpi_scale).ceil();

        // The filter field, under the header while the filter is open.
        let filter_field = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.filter.as_ref())
            .map(|filter| {
                let count = match (&filter.index, filter.query.trim().is_empty()) {
                    (None, _) => "\u{2026}".to_string(),
                    (Some(_), true) => String::new(),
                    (Some(_), false) if filter.cut || filter.count >= filter::MAX_MATCHES => {
                        format!("{}+", filter.count)
                    }
                    (Some(_), false) => filter.count.to_string(),
                };
                (filter.query.clone(), count)
            });
        if let Some((query, count)) = filter_field {
            let field = euclid::rect(
                card.origin.x + pad_x * 0.6,
                divider_y + 2. * dpi_scale,
                (card.size.width - pad_x * 1.2).max(1.),
                row_h,
            );
            let field_bg = opaque(lerp_rgba(surface, sb.text_meta, 0.12));
            self.sidebar_rounded_fill(layers, 1, field, 6. * dpi_scale, field_bg)?;
            let field_text_y = field.origin.y + (row_h - cell_h) * 0.5;
            let inner_x = field.origin.x + 8. * dpi_scale;
            self.diff_panel_text(
                layers,
                "\u{f002}",
                inner_x,
                field_text_y,
                cell_w,
                sb.text_meta,
                field_bg,
                false,
            )?;
            let count_w = count.chars().count() as f32 * cell_w;
            let query_x = inner_x + cell_w * 2.;
            let query_room = (field.max_x() - 8. * dpi_scale - count_w - cell_w - query_x).max(0.);
            let shown = if query.is_empty() {
                "Filter files".to_string()
            } else {
                crate::diff_panel::view::elide_start(
                    &query,
                    (query_room / cell_w).floor().max(0.) as usize,
                )
            };
            self.diff_panel_text(
                layers,
                &shown,
                query_x,
                field_text_y,
                query_room,
                if query.is_empty() {
                    sb.text_meta
                } else {
                    sb.text_active
                },
                field_bg,
                false,
            )?;
            if focused {
                let caret_x = if query.is_empty() {
                    query_x
                } else {
                    query_x + shown.chars().count() as f32 * cell_w
                };
                self.filled_rectangle(
                    layers,
                    2,
                    euclid::rect(caret_x, field_text_y, (1.5 * dpi_scale).max(1.), cell_h),
                    accent,
                )?;
            }
            if count_w > 0. {
                self.diff_panel_text(
                    layers,
                    &count,
                    field.max_x() - 8. * dpi_scale - count_w,
                    field_text_y,
                    count_w,
                    sb.text_meta,
                    field_bg,
                    false,
                )?;
            }
            // A click on the field lands on the panel's body, which gives
            // the panel the keyboard.
            divider_y = field.max_y() + 4. * dpi_scale;
        }

        self.filled_rectangle(
            layers,
            1,
            euclid::rect(
                card.origin.x + pad_x * 0.6,
                divider_y,
                (card.size.width - pad_x * 1.2).max(0.),
                stroke,
            ),
            lerp_rgba(surface, sb.divider, 0.8),
        )?;

        // Body: the rows that fit, above the preview when it is open.
        let area_top = divider_y + stroke + 4. * dpi_scale;
        let area_bottom = card.max_y() - stroke - 4. * dpi_scale;
        let preview_top = self.file_tree_preview_top(tab_id, area_top, area_bottom, row_h);
        let body_top = area_top;
        let body_bottom = preview_top.map_or(area_bottom, |top| top - 4. * dpi_scale);
        let visible = ((body_bottom - body_top) / row_h).floor().max(0.) as usize;
        self.file_tree.visible_rows.set(visible);

        struct Mark {
            badge: Option<Badge>,
            touched: Option<crate::agent_herd::touched::Touch>,
            /// Part of the selection.
            marked: bool,
        }
        let (status, rows, selected, scroll, total) = {
            let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
                return Ok(());
            };
            view.scroll = view.scroll.min(view.rows.len().saturating_sub(visible));
            let status = match (&view.tree, &view.error, &view.filter) {
                (_, Some(error), _) => Some(error.clone()),
                (None, None, _) => Some("Reading\u{2026}".to_string()),
                (Some(_), None, Some(filter))
                    if filter.index.is_none() && !filter.query.trim().is_empty() =>
                {
                    Some("Listing files\u{2026}".to_string())
                }
                (Some(_), None, Some(filter)) if filter.tree.is_some() && view.rows.is_empty() => {
                    Some("No matches".to_string())
                }
                (Some(_), None, _) if view.rows.is_empty() => Some("Empty folder".to_string()),
                _ => None,
            };
            let rows: Vec<(usize, Row, Mark)> = view
                .rows
                .iter()
                .enumerate()
                .skip(view.scroll)
                .take(visible)
                .map(|(index, row)| {
                    let dir = row.kind == EntryKind::Dir;
                    let badge = view.status.as_ref().and_then(|status| {
                        if dir {
                            status.dir(&row.rel)
                        } else {
                            status.file(&row.rel)
                        }
                    });
                    let touched = view.marks.of(&row.rel, dir);
                    let marked = view.is_marked(&row.rel);
                    (
                        index,
                        row.clone(),
                        Mark {
                            badge,
                            touched,
                            marked,
                        },
                    )
                })
                .collect();
            (
                status,
                rows,
                view.selected.clone(),
                view.scroll,
                view.rows.len(),
            )
        };

        if let Some(status) = status {
            self.diff_panel_text(
                layers,
                &status,
                card.origin.x + pad_x,
                body_top + (row_h - cell_h) * 0.5,
                (card.size.width - pad_x * 2.).max(0.),
                sb.text_meta,
                surface,
                false,
            )?;
        }

        let row_x = card.origin.x + 4. * dpi_scale;
        let row_w =
            (card.size.width - 8. * dpi_scale - if total > visible { 8. * dpi_scale } else { 0. })
                .max(1.);
        let indent = (12. * dpi_scale).max(cell_w);
        let chevron_w = (14. * dpi_scale).max(cell_w);
        let folder_color = lerp_rgba(sb.text_meta, accent, 0.45);
        let ansi = {
            let palette = self.palette();
            [
                palette.colors.0[1].to_linear(),
                palette.colors.0[2].to_linear(),
                palette.colors.0[3].to_linear(),
            ]
        };
        let dot = (5. * dpi_scale).round();
        // The row being named, and what has been typed.
        let editing = self.file_tree.views.get(&tab_id).and_then(|view| {
            view.edit.as_ref().map(|edit| {
                (
                    edit.row_rel().to_string(),
                    edit.text.clone(),
                    edit.cursor,
                    edit.busy,
                )
            })
        });
        // The folder a dragged row would land in, while it is over one.
        let drop_folder = self
            .file_tree
            .drag
            .as_ref()
            .and_then(|drag| drag.folder_target().map(str::to_string));
        for (slot, (index, row, mark)) in rows.iter().enumerate() {
            let y = body_top + slot as f32 * row_h;
            let item_type = UIItemType::FileTreeRow { index: *index };
            let is_hovered = hovered.as_ref() == Some(&item_type);
            // The keyboard's row carries the accent bar; every selected row
            // is filled.
            let is_cursor = selected.as_deref() == Some(row.rel.as_str());
            let is_selected = mark.marked;
            let row_rect = euclid::rect(row_x, y, row_w, row_h);
            // Hover and selection only tint: the row's geometry never moves.
            // Without the keyboard the selection stays, more quietly.
            let fill = if drop_folder.as_deref() == Some(row.rel.as_str()) {
                Some(opaque(lerp_rgba(surface, accent, 0.28)))
            } else if is_selected {
                Some(opaque(lerp_rgba(
                    surface,
                    sb.active_fill,
                    if focused { 0.9 } else { 0.55 },
                )))
            } else if is_hovered {
                Some(opaque(lerp_rgba(surface, sb.hover_fill, 0.9)))
            } else {
                None
            };
            let behind = fill.unwrap_or(surface);
            if let Some(fill) = fill {
                self.sidebar_rounded_fill(layers, 1, row_rect, 5. * dpi_scale, fill)?;
            }
            if is_selected && is_cursor {
                self.sidebar_pill_fill(
                    layers,
                    2,
                    euclid::rect(row_x, y + row_h * 0.25, 3. * dpi_scale, row_h * 0.5),
                    1.5 * dpi_scale,
                    if focused {
                        accent
                    } else {
                        lerp_rgba(behind, accent, 0.6)
                    },
                )?;
            }
            let base_x = row_x + pad_x * 0.6;
            // Indent guides: one hairline per level, under each parent's
            // chevron.
            for level in 0..row.depth {
                let guide_x = base_x + level as f32 * indent + chevron_w * 0.5;
                self.filled_rectangle(
                    layers,
                    1,
                    euclid::rect(guide_x, y, stroke, row_h),
                    lerp_rgba(behind, sb.divider, 0.7),
                )?;
            }
            let x = base_x + row.depth as f32 * indent;
            if row.kind == EntryKind::Dir {
                let expanded = matches!(row.state, RowState::Expanded | RowState::Loading);
                let color = if is_hovered || is_selected {
                    sb.text_active
                } else {
                    sb.text_meta
                };
                for bar in chevron_bars(x + chevron_w * 0.5, y + row_h * 0.5, dpi_scale, expanded) {
                    self.sidebar_rounded_fill(
                        layers,
                        2,
                        bar,
                        bar.size.width.min(bar.size.height) * 0.5,
                        color,
                    )?;
                }
            }
            let icon_x = x + chevron_w;
            let text_y = y + (row_h - cell_h) * 0.5;
            let icon_color = if row.kind == EntryKind::Dir {
                folder_color
            } else {
                sb.text_meta
            };
            self.diff_panel_text(
                layers,
                row_icon(row),
                icon_x,
                text_y,
                cell_w,
                icon_color,
                behind,
                false,
            )?;

            // Marks on the right: the agents' dot, then git's letter (a dot
            // for a folder).
            let mut marks_left = row_x + row_w - 6. * dpi_scale;
            if let Some(badge) = mark.badge {
                let color = badge_color(&ansi, badge);
                if row.kind == EntryKind::Dir {
                    marks_left -= dot;
                    self.sidebar_pill_fill(
                        layers,
                        2,
                        euclid::rect(marks_left, y + (row_h - dot) * 0.5, dot, dot),
                        dot * 0.5,
                        lerp_rgba(color, behind, 0.25),
                    )?;
                } else {
                    marks_left -= cell_w;
                    self.diff_panel_text(
                        layers,
                        badge.letter(),
                        marks_left,
                        text_y,
                        cell_w,
                        color,
                        behind,
                        true,
                    )?;
                }
                marks_left -= 6. * dpi_scale;
            }
            if let Some(touch) = mark.touched {
                let edited = touch == crate::agent_herd::touched::Touch::Edited;
                marks_left -= dot;
                self.sidebar_pill_fill(
                    layers,
                    2,
                    euclid::rect(marks_left, y + (row_h - dot) * 0.5, dot, dot),
                    dot * 0.5,
                    if edited {
                        accent
                    } else {
                        lerp_rgba(behind, accent, 0.45)
                    },
                )?;
                marks_left -= 6. * dpi_scale;
            }

            let name_x = icon_x + cell_w * 2.;
            let mut name = row.name.clone();
            match row.state {
                RowState::Loading => name.push_str(" \u{2026}"),
                RowState::Failed => name.push_str("  (unreadable)"),
                _ => {}
            }
            if row.symlink {
                name.push_str(" \u{2192}");
            }
            let base_fg = if is_selected || is_hovered {
                sb.text_active
            } else if row.state == RowState::Failed {
                sb.text_meta
            } else {
                sb.text_idle
            };
            // A changed file's name leans towards its badge's colour.
            let fg = match mark.badge.filter(|_| row.kind == EntryKind::File) {
                Some(badge) => lerp_rgba(base_fg, badge_color(&ansi, badge), 0.45),
                None => base_fg,
            };
            let name_room = (marks_left - name_x - 2. * dpi_scale).max(0.);
            let cols = (name_room / cell_w).floor().max(0.) as usize;
            let chars: Vec<char> = name.chars().collect();
            let field = editing
                .as_ref()
                .filter(|(rel, ..)| *rel == row.rel)
                .map(|(_, text, cursor, busy)| (text.clone(), *cursor, *busy));
            if let Some((text, cursor, busy)) = field {
                // The name as a text field, inset into the row.
                let field_x = name_x - 4. * dpi_scale;
                let field_w = (row_x + row_w - field_x - 4. * dpi_scale).max(1.);
                let field_rect =
                    euclid::rect(field_x, y + 2. * dpi_scale, field_w, row_h - 4. * dpi_scale);
                self.sidebar_rounded_fill(layers, 1, field_rect, 4. * dpi_scale, accent)?;
                let field_bg = opaque(lerp_rgba(surface, sb.text_meta, 0.10));
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(
                        field_rect.origin.x + stroke,
                        field_rect.origin.y + stroke,
                        (field_rect.size.width - stroke * 2.).max(1.),
                        (field_rect.size.height - stroke * 2.).max(1.),
                    ),
                    (4. * dpi_scale - stroke).max(0.),
                    field_bg,
                )?;
                let room = (field_w - 8. * dpi_scale).max(0.);
                let fit = (room / cell_w).floor().max(1.) as usize;
                // Keep the caret in view: show the end of a long name.
                let skip = cursor.saturating_sub(fit.saturating_sub(1));
                let shown: String = text.chars().skip(skip).take(fit).collect();
                let placeholder = text.is_empty();
                self.diff_panel_text(
                    layers,
                    if placeholder {
                        if row.kind == EntryKind::Dir {
                            "folder name"
                        } else {
                            "file name"
                        }
                    } else {
                        &shown
                    },
                    name_x,
                    text_y,
                    room,
                    if placeholder || busy {
                        sb.text_meta
                    } else {
                        sb.text_active
                    },
                    field_bg,
                    false,
                )?;
                if focused && !busy {
                    let caret_x = name_x + (cursor - skip) as f32 * cell_w;
                    self.filled_rectangle(
                        layers,
                        2,
                        euclid::rect(caret_x, text_y, (1.5 * dpi_scale).max(1.), cell_h),
                        accent,
                    )?;
                }
            } else if row.highlight.is_empty() || chars.len() > cols {
                self.diff_panel_text(layers, &name, name_x, text_y, name_room, fg, behind, false)?;
            } else {
                for (start, end, marked) in highlight_runs(chars.len(), &row.highlight) {
                    let piece: String = chars[start..end].iter().collect();
                    let width = (end - start) as f32 * cell_w;
                    self.diff_panel_text(
                        layers,
                        &piece,
                        name_x + start as f32 * cell_w,
                        text_y,
                        width,
                        if marked { accent } else { fg },
                        behind,
                        marked,
                    )?;
                }
            }
            self.ui_items.push(UIItem {
                x: row_x as usize,
                y: y as usize,
                width: row_w as usize,
                height: row_h as usize,
                item_type,
            });
        }

        self.paint_file_tree_scroll_bar(
            layers,
            drag::Body {
                card,
                area: (area_top, area_bottom),
                top: body_top,
                bottom: body_bottom,
                row_h,
                total,
                visible,
                scroll,
            },
            surface,
            sb.text_meta,
            &hovered,
        )?;
        if drop_folder.as_deref() == Some("") {
            // Dropping on the root: the whole body is the target.
            self.paint_file_tree_outline(
                layers,
                euclid::rect(row_x, body_top, row_w, (body_bottom - body_top).max(1.)),
                accent,
                stroke,
            )?;
        }
        if let Some(top) = preview_top {
            // The preview's own file's badge: it offers that file's changes.
            let badge = self.file_tree.views.get(&tab_id).and_then(|view| {
                let rel = view.preview.as_ref()?.shown()?;
                view.status.as_ref()?.file(rel)
            });
            self.paint_file_tree_preview(
                layers,
                tab_id,
                euclid::rect(
                    card.origin.x,
                    top,
                    card.size.width,
                    (area_bottom - top).max(0.),
                ),
                badge,
            )?;
        }
        self.paint_file_tree_drag(layers, surface, &sb)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chevrons_point_right_when_closed_and_down_when_open() {
        let closed = chevron_bars(10., 10., 1., false);
        assert!(closed.iter().all(|bar| bar.size.height >= bar.size.width));
        assert!(closed.windows(2).all(|w| w[1].origin.x > w[0].origin.x));
        let open = chevron_bars(10., 10., 1., true);
        assert!(open.iter().all(|bar| bar.size.width >= bar.size.height));
        assert!(open.windows(2).all(|w| w[1].origin.y > w[0].origin.y));
    }

    #[test]
    fn icons_follow_the_kind_of_file() {
        let row = |name: &str, kind, state| Row {
            rel: name.into(),
            highlight: Vec::new(),
            name: name.into(),
            depth: 0,
            kind,
            symlink: false,
            state,
        };
        assert_eq!(
            row_icon(&row("src", EntryKind::Dir, RowState::Collapsed)),
            "\u{f07b}"
        );
        assert_eq!(
            row_icon(&row("src", EntryKind::Dir, RowState::Expanded)),
            "\u{f07c}"
        );
        assert_eq!(
            row_icon(&row("main.RS", EntryKind::File, RowState::File)),
            "\u{e7a8}"
        );
        assert_eq!(
            row_icon(&row(".gitignore", EntryKind::File, RowState::File)),
            "\u{e702}"
        );
        assert_eq!(
            row_icon(&row("Makefile", EntryKind::File, RowState::File)),
            "\u{f15b}"
        );
    }

    #[test]
    fn highlight_runs_cover_the_name_in_order() {
        assert_eq!(
            highlight_runs(6, &[0, 1, 4]),
            [(0, 2, true), (2, 4, false), (4, 5, true), (5, 6, false)]
        );
        assert_eq!(highlight_runs(3, &[]), [(0, 3, false)]);
        assert!(highlight_runs(0, &[0]).is_empty());
    }

    #[test]
    fn a_reread_file_list_keeps_the_selection_a_new_query_moves_it() {
        let mut view = TreeView::new(0);
        view.tree = Some(Tree::new(PathBuf::from("/r")));
        view.filter = Some(FilterView {
            query: "rs".into(),
            index: Some(vec!["a/main.rs".into(), "b/lib.rs".into(), "c.md".into()]),
            ..FilterView::default()
        });
        view.refilter(true);
        assert_eq!(view.filter.as_ref().unwrap().count, 2);
        let best = view.selected.clone().unwrap();
        let other = if best == "a/main.rs" {
            "b/lib.rs"
        } else {
            "a/main.rs"
        };
        view.selected = Some(other.into());
        view.refilter(false);
        assert_eq!(view.selected.as_deref(), Some(other));
        view.refilter(true);
        assert_eq!(view.selected.as_deref(), Some(best.as_str()));
        // Every folder on the way to a match is open.
        assert!(view.rows.iter().any(|row| row.rel == "b/lib.rs"));
    }

    #[test]
    fn the_selection_is_listed_in_row_order() {
        let mut view = TreeView::new(0);
        let mut tree = Tree::new(PathBuf::from("/r"));
        tree.set_listing(
            String::new(),
            crate::file_tree::Listing::Loaded(vec![
                crate::file_tree::Entry {
                    name: "a".into(),
                    kind: EntryKind::Dir,
                    symlink: false,
                },
                crate::file_tree::Entry {
                    name: "b.rs".into(),
                    kind: EntryKind::File,
                    symlink: false,
                },
                crate::file_tree::Entry {
                    name: "c.rs".into(),
                    kind: EntryKind::File,
                    symlink: false,
                },
            ]),
        );
        view.tree = Some(tree);
        view.rebuild();
        view.select_only(Some("c.rs".into()));
        view.marked.insert("a".into());
        assert_eq!(
            view.marked_rows(),
            [
                ("a".to_string(), EntryKind::Dir),
                ("c.rs".to_string(), EntryKind::File)
            ]
        );
        assert!(!view.is_marked("b.rs"));
        view.select_only(None);
        assert!(view.marked_rows().is_empty());
        assert_eq!(view.anchor, None);
    }

    #[test]
    fn keys_map_to_tree_actions() {
        assert_eq!(tree_key(&KeyCode::Char('j')), Some(TreeKey::Down));
        assert_eq!(tree_key(&KeyCode::UpArrow), Some(TreeKey::Up));
        assert_eq!(tree_key(&KeyCode::Enter), Some(TreeKey::Activate));
        assert_eq!(tree_key(&KeyCode::Char('/')), Some(TreeKey::StartFilter));
        assert_eq!(tree_key(&KeyCode::Char('x')), Some(TreeKey::Swallow));
        assert_eq!(tree_key(&KeyCode::Function(1)), None);
    }
}
