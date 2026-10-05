//! The Changes panel's window glue: per-window state, where it sits, how it
//! is dragged, and how it is painted.
//!
//! The geometry itself is the pure [`crate::diff_panel::geometry`]; this file
//! only gathers its inputs from the window and acts on its answer.

use crate::diff_panel::exec::{Env, Runner};
use crate::diff_panel::geometry::{
    panel_geometry, PanelArea, PanelGeometry, PanelRequest, PanelSnap,
};
use crate::diff_panel::view::{self, Row};
use crate::diff_panel::{self, ChangeSource, FileStatus, Limits, LineKind, Scan};
use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::sidebar::{
    contrast_label_color, herd_scan_is_due, lerp_rgba, sidebar_text_cols,
    sidebar_width_scale_for_dpi, truncate_to_cols, FLOAT_GAP, PAD_X, RADIUS,
};
use crate::termwindow::render::RenderScreenLineParams;
use crate::termwindow::{
    tgz_ui_state, DiffPanelAction, DiffPanelEdge, TermWindowNotif, UIItem, UIItemType,
};
use ::window::{CursorIcon, MouseEvent, MouseEventKind as WMEK, MousePress, RectF, WindowOps};
use config::{DiffPanelPosition, SidebarPosition};
use mux::pane::{CachePolicy, Pane, PaneId};
use mux::renderable::RenderableDimensions;
use mux::Mux;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::color::ColorAttribute;
use termwiz::surface::{Line, SEQ_ZERO};
use window::color::LinearRgba;

/// Narrowest and shortest the panel may be dragged, in pixels at 2x.
const MIN_WIDTH_PX: f32 = 360.;
const MIN_HEIGHT_PX: f32 = 240.;
/// Share of the available height a never-dragged panel takes.
const DEFAULT_HEIGHT_SHARE: f32 = 0.62;
/// Thickness of the draggable edges, in pixels at 1x.
const GRIP: f32 = 8.;

/// A scan still running after this long is presumed lost, so it cannot hold
/// up every later one.
const SCAN_WATCHDOG: Duration = Duration::from_secs(60);
/// A scan may use at most this share of the time between scans: a tree that
/// takes a second to diff is re-read every few seconds, not every two.
const SCAN_DUTY_DIVISOR: u32 = 4;
/// How long after a watched change the working copy is re-read: writes come
/// in bursts (a save, a build, a checkout), and one read should see them all.
const WATCH_SETTLE: Duration = Duration::from_millis(300);
/// A tree that keeps changing is re-read at most this share of the time.
const WATCH_DUTY_DIVISOR: u32 = 2;
/// Most files given a chip in the index, and most rows the chips may wrap
/// onto; the rest are counted in a "+N more" marker.
const MAX_CHIPS: usize = 40;
const MAX_CHIPS_EXPANDED: usize = 400;
const MAX_CHIP_ROWS: usize = 3;
const REMOTE_PANE: &str = "Changes are not available for remote panes yet";

/// What the panel shows for one pane.
#[derive(Default)]
pub struct PaneView {
    scan: Option<Scan>,
    /// When the scan on show was started.
    scanned_at: Option<Instant>,
    /// How long that scan took.
    scan_cost: Duration,
    /// Paths of files folded down to their header row.
    collapsed: HashSet<String>,
    /// Whether the file index shows every chip rather than its first rows.
    chips_expanded: bool,
    scroll: usize,
    rows: Vec<Row>,
    digits: usize,
}

impl PaneView {
    fn rebuild(&mut self) {
        match &self.scan {
            Some(Scan::Changes(set)) => {
                self.rows = view::flatten(set, &self.collapsed);
                self.digits = view::line_number_digits(set);
            }
            _ => {
                self.rows.clear();
                self.digits = 1;
            }
        }
    }
}

/// One visible row, copied out of the view so painting can borrow the window.
enum PaintRow {
    File {
        index: usize,
        status: FileStatus,
        path: String,
        added: usize,
        removed: usize,
        collapsed: bool,
    },
    Gap,
    Line {
        kind: LineKind,
        old_no: Option<u32>,
        new_no: Option<u32>,
        text: String,
    },
    Note(String),
}

fn snapshot_base() -> PathBuf {
    config::DATA_DIR.join("diff-snapshots")
}

/// Where a pane's changes are to be looked for. Decided on the GUI thread
/// from what the pane says about itself, and settled into a directory and a
/// [`Runner`] on the scan thread, which may have to ask a WSL distro.
enum ScanTarget {
    /// A directory this process can open.
    Dir(PathBuf),
    /// A pane whose program runs inside a WSL distro, where only the distro
    /// knows the directory it is in.
    WslPane {
        /// `(distro, user)` to ask, likeliest first. One for a pane of a WSL
        /// domain; every running distro for a `wsl.exe` typed into an
        /// ordinary pane, which does not say where it went.
        distros: Vec<(String, Option<String>)>,
        pane_id: PaneId,
        /// The directory the shell reported through OSC 7, as a Linux path.
        reported: Option<String>,
        /// The directory to settle for when the distro has no answer.
        fallback: Option<PathBuf>,
    },
}

/// The directory to scan and the environments its client may live in, the
/// likeliest first.
///
/// The client follows the files, not the pane: a working copy on a Windows
/// drive is read by the Windows client even from a WSL pane, and one inside a
/// distro by that distro's. The other side is the fallback for a client that
/// is installed on one side only. `other_distro` is the default WSL distro,
/// which stands in when the pane itself is not in one.
fn settle_scan_target(
    target: ScanTarget,
    other_distro: Option<String>,
) -> Result<(PathBuf, Runner), String> {
    use crate::termwindow::wsl_paths;
    // The pane's own distro, when it is in one.
    let mut pane_env = None;
    let dir = match target {
        ScanTarget::Dir(dir) => dir,
        ScanTarget::WslPane {
            distros,
            pane_id,
            reported,
            fallback,
        } => {
            let found = distros.iter().find_map(|(distro, user)| {
                let linux = wsl_paths::pane_cwd_in_distro(distro, user.as_deref(), pane_id)?;
                Some((distro, user, linux))
            });
            // A reported directory is only as good as the guess at the distro
            // it is in, so it counts when there is a single candidate.
            let found = found.or_else(|| match (distros.as_slice(), reported) {
                ([(distro, user)], Some(linux)) => Some((distro, user, linux)),
                _ => None,
            });
            let settled = found.and_then(|(distro, user, linux)| {
                let dir = wsl_paths::wsl_to_windows(&linux, distro)?;
                Some((dir, distro.clone(), user.clone()))
            });
            match (settled, fallback) {
                (Some((dir, distro, user)), _) => {
                    pane_env = Some((distro, user));
                    dir
                }
                (None, Some(dir)) => dir,
                (None, None) => {
                    return Err("WSL did not say which directory this pane is in".to_string())
                }
            }
        }
    };
    let envs = match wsl_paths::distro_of_unc(&dir.to_string_lossy()) {
        // Inside a distro's own filesystem, whoever is looking at it.
        Some(distro) => {
            let user = pane_env
                .filter(|(pane_distro, _)| pane_distro.eq_ignore_ascii_case(&distro))
                .and_then(|(_, user)| user);
            vec![Env::Wsl { distro, user }, Env::Host]
        }
        None => std::iter::once(Env::Host)
            .chain(
                pane_env
                    .or(other_distro.map(|distro| (distro, None)))
                    .map(|(distro, user)| Env::Wsl { distro, user }),
            )
            .collect(),
    };
    Ok((dir, Runner::new(envs)))
}

/// True when a Windows process image is one of the shims a WSL session runs
/// behind; the Linux program itself is invisible from this side.
fn is_wsl_shim(executable: &str) -> bool {
    let name = executable
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase();
    matches!(
        name.trim_end_matches(".exe"),
        "wsl" | "wslhost" | "wslrelay"
    )
}

/// How tall the user wants the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelHeight {
    /// Never dragged: [`DEFAULT_HEIGHT_SHARE`] of the available height.
    Default,
    /// Snapped to the bottom.
    Bottom,
    /// Dragged to this many physical pixels.
    Px(usize),
}

pub struct DiffPanelState {
    /// Panes the panel is switched on for.
    pub enabled_panes: HashSet<PaneId>,
    /// Dragged width in physical pixels; `None` uses `diff_panel.width_px`.
    pub width: Option<usize>,
    pub height: PanelHeight,
    /// The reservation the last layout pass used. Painting compares it with
    /// the current one to notice a change layout has not caught up with: the
    /// panel is per pane, so switching tabs changes it without any resize.
    layout_width: Cell<usize>,
    relayout_queued: Cell<bool>,
    views: HashMap<PaneId, PaneView>,
    scan_started_at: Cell<Option<Instant>>,
    /// Watches the working copy on show, so a written file is re-read at
    /// once instead of at the next poll. `None` where watching is not
    /// possible; the poll still runs either way.
    watch: Option<diff_panel::watch::Watch>,
    /// Body rows that fit, as of the last paint; the wheel scrolls within it.
    visible_rows: Cell<usize>,
}

impl DiffPanelState {
    pub fn load() -> Self {
        let (width, height) = tgz_ui_state::load_diff_panel_size();
        Self {
            enabled_panes: HashSet::new(),
            width,
            height: match height {
                None => PanelHeight::Default,
                Some(0) => PanelHeight::Bottom,
                Some(px) => PanelHeight::Px(px),
            },
            layout_width: Cell::new(0),
            relayout_queued: Cell::new(false),
            views: HashMap::new(),
            scan_started_at: Cell::new(None),
            watch: None,
            visible_rows: Cell::new(0),
        }
    }

    fn save(&self) {
        tgz_ui_state::save_diff_panel_size(
            self.width,
            match self.height {
                PanelHeight::Default => None,
                PanelHeight::Bottom => Some(0),
                PanelHeight::Px(px) => Some(px),
            },
        );
    }
}

fn opaque(color: LinearRgba) -> LinearRgba {
    LinearRgba(color.0, color.1, color.2, 1.0)
}

fn resize_cursor(edge: DiffPanelEdge, position: DiffPanelPosition) -> CursorIcon {
    match (edge, position) {
        (DiffPanelEdge::Width, _) => CursorIcon::EwResize,
        (DiffPanelEdge::Height, _) => CursorIcon::NsResize,
        (DiffPanelEdge::Corner, DiffPanelPosition::Right) => CursorIcon::NeswResize,
        (DiffPanelEdge::Corner, DiffPanelPosition::Left) => CursorIcon::NwseResize,
    }
}

impl crate::TermWindow {
    /// Whether the panel is switched on for the pane being shown.
    pub fn diff_panel_shown(&self) -> bool {
        if !self.config.diff_panel.enabled || self.diff_panel.enabled_panes.is_empty() {
            return false;
        }
        match self.get_active_pane_or_overlay() {
            Some(pane) => self.diff_panel.enabled_panes.contains(&pane.pane_id()),
            None => false,
        }
    }

    fn diff_panel_scale(&self) -> f32 {
        sidebar_width_scale_for_dpi(self.dimensions.dpi as f64)
    }

    /// The window region the terminal and the panel share.
    fn diff_panel_area(&self) -> PanelArea {
        let border = self.get_os_border();
        let tab_bar_height = if self.show_tab_bar && !self.sidebar_is_active() {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (top_bar, bottom_bar) = if self.config.tab_bar_at_bottom {
            (0., tab_bar_height)
        } else {
            (tab_bar_height, 0.)
        };
        let sidebar = self.sidebar_reserved_width() as f32;
        let (sidebar_left, sidebar_right) = match self.config.sidebar_position {
            SidebarPosition::Left => (sidebar, 0.),
            SidebarPosition::Right => (0., sidebar),
        };
        PanelArea {
            left: border.left.get() as f32 + sidebar_left,
            right: self.dimensions.pixel_width as f32 - border.right.get() as f32 - sidebar_right,
            top: border.top.get() as f32 + top_bar,
            bottom: self.dimensions.pixel_height as f32 - border.bottom.get() as f32 - bottom_bar,
            strip_height: self.docked_input_pixel_height(),
        }
    }

    /// Where the panel is right now, or `None` when it is hidden or the
    /// window is too small for it.
    pub fn diff_panel_geometry(&self) -> Option<PanelGeometry> {
        if !self.diff_panel_shown() {
            return None;
        }
        let area = self.diff_panel_area();
        let scale = self.diff_panel_scale();
        let config = &self.config.diff_panel;
        let height = match self.diff_panel.height {
            PanelHeight::Default => Some((area.bottom - area.top) * DEFAULT_HEIGHT_SHARE),
            PanelHeight::Bottom => None,
            PanelHeight::Px(px) => Some(px as f32),
        };
        panel_geometry(
            area,
            PanelRequest {
                position: config.position,
                raised_mode: config.raised_mode,
                width: self
                    .diff_panel
                    .width
                    .map(|w| w as f32)
                    .unwrap_or(config.width_px as f32 * scale),
                height,
                snap: config.snap_px as f32 * scale,
                min_width: MIN_WIDTH_PX * scale,
                min_height: MIN_HEIGHT_PX * scale,
            },
        )
    }

    /// Pixels of width the terminal gives up for the panel.
    ///
    /// Frozen at its starting value while an edge is being dragged, so the
    /// pane reflows once on release instead of on every pixel of the drag.
    pub fn diff_panel_reserved_width(&self) -> usize {
        if let Some((item, _)) = self.dragging.as_ref() {
            if let UIItemType::DiffPanelResize { start_reserved, .. } = item.item_type {
                return start_reserved;
            }
        }
        self.diff_panel_geometry()
            .map(|g| g.reserved_width as usize)
            .unwrap_or(0)
    }

    /// [`Self::diff_panel_reserved_width`] for the layout pass, which also
    /// records what it used.
    pub fn diff_panel_layout_width(&self) -> usize {
        let width = self.diff_panel_reserved_width();
        self.diff_panel.layout_width.set(width);
        width
    }

    pub fn diff_panel_left_reserved(&self) -> usize {
        match self.config.diff_panel.position {
            DiffPanelPosition::Left => self.diff_panel_reserved_width(),
            DiffPanelPosition::Right => 0,
        }
    }

    pub fn diff_panel_right_reserved(&self) -> usize {
        match self.config.diff_panel.position {
            DiffPanelPosition::Left => 0,
            DiffPanelPosition::Right => self.diff_panel_reserved_width(),
        }
    }

    /// `(shift_left, extra_width)` for the docked input strip: a panel that
    /// stops above the strip hands the width it took from the terminal to it.
    pub fn diff_panel_strip_extra(&self) -> (f32, f32) {
        let extra = self
            .diff_panel_geometry()
            .map(|g| g.strip_extra_width)
            .unwrap_or(0.);
        match self.config.diff_panel.position {
            DiffPanelPosition::Left => (extra, extra),
            DiffPanelPosition::Right => (0., extra),
        }
    }

    /// The part of a pane's width, starting at `pane_x`, that a floating
    /// panel docked on the right leaves uncovered.
    pub fn diff_panel_clear_width(&self, pane_x: f32, pane_w: f32) -> f32 {
        match self.diff_panel_geometry() {
            Some(g)
                if g.floats()
                    && self.config.diff_panel.position == DiffPanelPosition::Right
                    && pane_x + pane_w > g.x =>
            {
                (g.x - pane_x).max(0.)
            }
            _ => pane_w,
        }
    }

    fn relayout_for_diff_panel(&mut self) {
        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            let dims = self.dimensions;
            self.apply_dimensions(&dims, None, &window);
            window.invalidate();
        }
    }

    /// Show or hide the panel for one pane.
    pub fn toggle_diff_panel_pane(&mut self, pane_id: PaneId) {
        if !self.config.diff_panel.enabled {
            return;
        }
        if self.diff_panel.enabled_panes.remove(&pane_id) {
            self.diff_panel.views.remove(&pane_id);
            if self.diff_panel.enabled_panes.is_empty() {
                // Nothing on show to keep current.
                self.diff_panel.watch = None;
            }
        } else {
            self.diff_panel.enabled_panes.insert(pane_id);
            // Housekeeping rides on the first use rather than on startup.
            static PRUNED: std::sync::Once = std::sync::Once::new();
            PRUNED.call_once(|| {
                std::thread::spawn(|| {
                    diff_panel::snapshot::prune(&snapshot_base(), std::time::SystemTime::now())
                });
            });
        }
        self.relayout_for_diff_panel();
    }

    /// Where the changes `pane` should show are to be looked for.
    fn diff_panel_target(&self, pane: &Arc<dyn Pane>) -> Result<ScanTarget, String> {
        let domain = Mux::get()
            .get_domain(pane.domain_id())
            .filter(|domain| domain.downcast_ref::<mux::domain::LocalDomain>().is_some())
            .ok_or_else(|| REMOTE_PANE.to_string())?;
        let cwd = pane
            .get_current_working_dir(CachePolicy::AllowStale)
            .map(|url| url.to_file_path());

        if cfg!(windows) {
            use crate::termwindow::wsl_paths;
            let domain_name = domain.domain_name();
            // What OSC 7 from inside a distro resolves to: a UNC naming this
            // machine. Anything else is `wsl.exe`'s own directory, which says
            // nothing about where the shell inside it is.
            let reported = cwd
                .as_ref()
                .and_then(|cwd| cwd.as_ref().ok())
                .and_then(|cwd| wsl_paths::unc_host_path_to_linux(&cwd.to_string_lossy()));
            if let Some(distro) = wsl_paths::distro_for_domain(domain_name, &self.config) {
                let user = wsl_paths::wsl_domains(&self.config)
                    .into_iter()
                    .find(|wsl| wsl.name == domain_name)
                    .and_then(|wsl| wsl.username);
                return Ok(ScanTarget::WslPane {
                    distros: vec![(distro, user)],
                    pane_id: pane.pane_id(),
                    reported,
                    fallback: None,
                });
            }
            // `wsl.exe` run by hand in an ordinary pane. Which distro it
            // entered is not visible from here, so each running one is asked;
            // a distro the pane is not in simply has no such pane.
            let shim = pane
                .get_foreground_process_name(CachePolicy::AllowStale)
                .is_some_and(|name| is_wsl_shim(&name));
            let distros = if shim {
                Self::running_wsl_distros()
            } else {
                vec![]
            };
            if !distros.is_empty() {
                return Ok(ScanTarget::WslPane {
                    distros: distros.into_iter().map(|distro| (distro, None)).collect(),
                    pane_id: pane.pane_id(),
                    reported,
                    fallback: cwd.and_then(|cwd| cwd.ok()),
                });
            }
        }

        // A shell on another host reports a `file://host/...` this machine
        // cannot open.
        cwd.ok_or_else(|| "This pane has not reported its directory".to_string())?
            .map(ScanTarget::Dir)
            .map_err(|_| REMOTE_PANE.to_string())
    }

    /// Running distros, the default first; just the default while it is not
    /// yet known which are running. Asking a stopped one would boot it.
    fn running_wsl_distros() -> Vec<String> {
        if !cfg!(windows) {
            return vec![];
        }
        let distros = crate::termwindow::wsl_paths::cached_distros();
        let running: Vec<String> = distros
            .iter()
            .filter(|distro| distro.state == config::WSL_STATE_RUNNING)
            .map(|distro| distro.name.clone())
            .collect();
        if running.is_empty() && distros.iter().all(|distro| distro.state.is_empty()) {
            return Self::default_wsl_distro().into_iter().collect();
        }
        running
    }

    fn default_wsl_distro() -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        crate::termwindow::wsl_paths::cached_distros()
            .iter()
            .find(|distro| distro.is_default)
            .map(|distro| distro.name.clone())
    }

    fn diff_panel_publish(&mut self, pane_id: PaneId, started: Instant, result: Scan) {
        if !self.diff_panel.enabled_panes.contains(&pane_id) {
            return;
        }
        let visible = self.diff_panel.visible_rows.get();
        let view = self.diff_panel.views.entry(pane_id).or_default();
        view.scanned_at = Some(started);
        view.scan_cost = started.elapsed();
        if view.scan.as_ref() == Some(&result) {
            return;
        }
        // Files reorder as they change. Someone reading further down keeps
        // their place: the same file, the same distance below its header.
        let anchor = match (&view.scan, view.scroll) {
            (Some(Scan::Changes(set)), scroll) if scroll > 0 => {
                view::file_at_row(&view.rows, scroll)
                    .and_then(|(file, delta)| Some((set.files.get(file)?.path.clone(), delta)))
            }
            _ => None,
        };
        view.scan = Some(result);
        view.rebuild();
        if let (Some((path, delta)), Some(Scan::Changes(set))) = (anchor, &view.scan) {
            let row = set
                .files
                .iter()
                .position(|file| file.path == path)
                .and_then(|file| view::row_of_file(&view.rows, file));
            if let Some(row) = row {
                view.scroll = row + delta;
            }
        }
        view.scroll = view.scroll.min(view.rows.len().saturating_sub(visible));
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Start a scan for `pane` when the one on show has gone stale. The scan
    /// itself runs on its own thread: it spawns processes and walks trees.
    fn diff_panel_refresh(&mut self, pane: &Arc<dyn Pane>) {
        let pane_id = pane.pane_id();
        let now = Instant::now();
        let (scanned_at, cost) = self
            .diff_panel
            .views
            .get(&pane_id)
            .map(|view| (view.scanned_at, view.scan_cost))
            .unwrap_or_default();
        // A change the watch saw is read as soon as the burst it belongs to
        // has had a moment to finish; with nothing seen, the poll decides.
        let changed = self
            .diff_panel
            .watch
            .as_ref()
            .filter(|watch| watch.is_dirty())
            .is_some_and(|watch| {
                let view = self.diff_panel.views.get(&pane_id);
                matches!(
                    view.and_then(|view| view.scan.as_ref()),
                    Some(Scan::Changes(set)) if set.root == watch.root()
                )
            });
        let ttl = if changed {
            WATCH_SETTLE.max(cost * WATCH_DUTY_DIVISOR)
        } else {
            Duration::from_millis(self.config.diff_panel.refresh_ms.max(250))
                .max(cost * SCAN_DUTY_DIVISOR)
        };
        // Come back when the next scan is due even if nothing else repaints.
        self.update_next_frame_time(Some(now + ttl));
        if !herd_scan_is_due(
            self.diff_panel.scan_started_at.get(),
            scanned_at,
            ttl,
            SCAN_WATCHDOG,
            now,
        ) {
            return;
        }
        let target = match self.diff_panel_target(pane) {
            Ok(target) => target,
            Err(reason) => {
                self.diff_panel_publish(pane_id, now, Scan::Unavailable(reason));
                return;
            }
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let limits = Limits {
            max_file_bytes: self.config.diff_panel.max_file_bytes,
            snapshot_max_files: self.config.diff_panel.snapshot_max_files,
        };
        let other_distro = Self::default_wsl_distro();
        let watched = self.diff_panel.watch.as_ref().map(|watch| {
            watch.clear();
            watch.root().to_path_buf()
        });
        self.diff_panel.scan_started_at.set(Some(now));
        let spawned = std::thread::Builder::new()
            .name("diff-panel-scan".into())
            .spawn(move || {
                let settled = settle_scan_target(target, other_distro);
                let settled_in = now.elapsed();
                let result = match settled {
                    Ok((dir, runner)) => {
                        let result = diff_panel::scan(&dir, &runner, &snapshot_base(), limits);
                        // Like the herd scan: a slow one is worth a line, so
                        // "the panel is slow" can be told apart from "the
                        // client is slow" after the fact.
                        if now.elapsed() > Duration::from_secs(2) {
                            log::info!(
                                "diff panel scan of {} took {:?} ({:?} finding the directory)",
                                dir.display(),
                                now.elapsed(),
                                settled_in
                            );
                        }
                        result
                    }
                    Err(reason) => Scan::Unavailable(reason),
                };
                // Follow the working copy on show. Started here because
                // registering a large tree takes a while on some platforms.
                let watch = match &result {
                    Scan::Changes(set) if watched.as_deref() != Some(set.root.as_path()) => {
                        let window = window.clone();
                        diff_panel::watch::Watch::start(&set.root, move || {
                            window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                                if let Some(window) = term_window.window.as_ref() {
                                    window.invalidate();
                                }
                            })));
                        })
                        .map_err(|err| {
                            log::debug!("diff panel: cannot watch {}: {err:#}", set.root.display())
                        })
                        .ok()
                    }
                    _ => None,
                };
                window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    term_window.diff_panel.scan_started_at.set(None);
                    if watch.is_some() {
                        term_window.diff_panel.watch = watch;
                    }
                    term_window.diff_panel_publish(pane_id, now, result);
                })));
            });
        if let Err(err) = spawned {
            log::warn!("diff panel: could not start a scan thread: {err:#}");
            self.diff_panel.scan_started_at.set(None);
        }
    }

    /// Scroll the panel so file `index`'s diff starts at the top, unfolding
    /// it if it was folded.
    pub fn diff_panel_jump_to_file(&mut self, pane_id: PaneId, index: usize) {
        let visible = self.diff_panel.visible_rows.get();
        let Some(view) = self.diff_panel.views.get_mut(&pane_id) else {
            return;
        };
        let path = match &view.scan {
            Some(Scan::Changes(set)) => set.files.get(index).map(|file| file.path.clone()),
            _ => None,
        };
        if let Some(path) = path {
            if view.collapsed.remove(&path) {
                view.rebuild();
            }
        }
        if let Some(row) = view::row_of_file(&view.rows, index) {
            view.scroll = row.min(view.rows.len().saturating_sub(visible));
        }
    }

    /// Clicks and wheel on the panel. `target` is the header button or the
    /// file row under the pointer, if any.
    pub fn mouse_event_diff_panel(
        &mut self,
        target: Option<Result<DiffPanelAction, usize>>,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        let pane_id = pane.pane_id();
        if let WMEK::VertWheel(amount) = event.kind {
            let visible = self.diff_panel.visible_rows.get();
            if let Some(view) = self.diff_panel.views.get_mut(&pane_id) {
                let next = view::scrolled(view.scroll, amount as isize, view.rows.len(), visible);
                if next != view.scroll {
                    view.scroll = next;
                    context.invalidate();
                }
            }
            return;
        }
        if event.kind != WMEK::Release(MousePress::Left) {
            return;
        }
        self.pressed_ui_item = None;
        match target {
            None => {}
            Some(Ok(DiffPanelAction::Close)) => self.toggle_diff_panel_pane(pane_id),
            Some(Ok(DiffPanelAction::ToggleChips)) => {
                if let Some(view) = self.diff_panel.views.get_mut(&pane_id) {
                    view.chips_expanded = !view.chips_expanded;
                }
                context.invalidate();
            }
            Some(Ok(DiffPanelAction::Refresh)) => {
                if let Some(view) = self.diff_panel.views.get_mut(&pane_id) {
                    view.scanned_at = None;
                    view.scan_cost = Duration::ZERO;
                }
                context.invalidate();
            }
            Some(Ok(DiffPanelAction::ResetBaseline)) => {
                if let Some(view) = self.diff_panel.views.get_mut(&pane_id) {
                    if let Some(Scan::Changes(set)) = &view.scan {
                        if set.source == ChangeSource::Snapshot {
                            diff_panel::snapshot::reset(&snapshot_base(), &set.root);
                        }
                    }
                    view.scanned_at = None;
                }
                context.invalidate();
            }
            Some(Err(index)) => {
                if let Some(view) = self.diff_panel.views.get_mut(&pane_id) {
                    let path = match &view.scan {
                        Some(Scan::Changes(set)) => set.files.get(index).map(|f| f.path.clone()),
                        _ => None,
                    };
                    if let Some(path) = path {
                        if !view.collapsed.remove(&path) {
                            view.collapsed.insert(path);
                        }
                        view.rebuild();
                        let visible = self.diff_panel.visible_rows.get();
                        view.scroll = view.scroll.min(view.rows.len().saturating_sub(visible));
                    }
                }
                context.invalidate();
            }
        }
    }

    /// Queue a layout pass when the reservation changed without one, e.g.
    /// after switching to a tab whose pane has the panel in another state.
    fn diff_panel_sync_layout(&self) {
        if self.dragging.is_some() || self.diff_panel.relayout_queued.get() {
            return;
        }
        if self.diff_panel_reserved_width() == self.diff_panel.layout_width.get() {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        self.diff_panel.relayout_queued.set(true);
        window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
            term_window.diff_panel.relayout_queued.set(false);
            term_window.relayout_for_diff_panel();
            // Whatever the pass did, do not ask for another one for the same
            // reservation: a window too small to honour it would loop.
            let width = term_window.diff_panel_reserved_width();
            term_window.diff_panel.layout_width.set(width);
        })));
    }

    pub fn mouse_event_diff_panel_resize(
        &mut self,
        edge: DiffPanelEdge,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(resize_cursor(edge, self.config.diff_panel.position)));
        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    pub fn drag_diff_panel_resize(
        &mut self,
        edge: DiffPanelEdge,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let position = self.config.diff_panel.position;
        context.set_cursor(Some(resize_cursor(edge, position)));
        let area = self.diff_panel_area();
        let scale = self.diff_panel_scale();
        let (x, y) = (event.coords.x as f32, event.coords.y as f32);

        let before = (self.diff_panel.width, self.diff_panel.height);
        if edge != DiffPanelEdge::Height {
            let width = match position {
                DiffPanelPosition::Left => x - area.left,
                DiffPanelPosition::Right => area.right - x,
            };
            self.diff_panel.width = Some(width.max(MIN_WIDTH_PX * scale) as usize);
        }
        if edge != DiffPanelEdge::Width {
            // Kept as dragged even inside a snap zone: the geometry snaps what
            // is drawn, and dragging back out lifts the panel off again.
            let height = (y - area.top).max(MIN_HEIGHT_PX * scale);
            self.diff_panel.height = PanelHeight::Px(height as usize);
        }
        if before != (self.diff_panel.width, self.diff_panel.height) {
            self.quad_generation += 1;
            context.invalidate();
        }
        self.dragging.replace((item, start_event));
    }

    /// The drag is over: settle the size, remember it, and let the terminal
    /// take up whatever the panel now leaves it.
    pub fn finish_diff_panel_resize(&mut self) {
        if let Some(geometry) = self.diff_panel_geometry() {
            self.diff_panel.width = Some(geometry.width as usize);
            self.diff_panel.height = match geometry.snap {
                PanelSnap::Bottom => PanelHeight::Bottom,
                PanelSnap::Strip | PanelSnap::Raised => PanelHeight::Px(geometry.height as usize),
            };
        }
        self.diff_panel.save();
        self.relayout_for_diff_panel();
    }

    fn diff_panel_text(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        text: &str,
        x: f32,
        y: f32,
        pixel_width: f32,
        fg: LinearRgba,
        bg: LinearRgba,
        bold: bool,
    ) -> anyhow::Result<()> {
        let cell_width = self.render_metrics.cell_size.width as usize;
        let cell_height = self.render_metrics.cell_size.height as usize;
        let cols = sidebar_text_cols(pixel_width, cell_width);
        if cols == 0 {
            return Ok(());
        }
        let text = truncate_to_cols(text, cols);
        let mut attrs = CellAttributes::default();
        attrs.set_foreground(ColorAttribute::TrueColorWithDefaultFallback(fg.to_srgb()));
        if bold {
            attrs.set_intensity(Intensity::Bold);
        }
        let mut line = Line::from_text(text, &attrs, 1, None);
        line.resize(cols, SEQ_ZERO);

        let palette = self.palette().clone();
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y: y,
                left_pixel_x: x,
                pixel_width,
                stable_line_idx: None,
                line: &line,
                selection: 0..0,
                cursor: &Default::default(),
                palette: &palette,
                dims: &RenderableDimensions {
                    cols,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: self.terminal_size.dpi,
                    pixel_height: cell_height,
                    pixel_width: pixel_width as usize,
                    reverse_video: false,
                },
                config: &self.config,
                cursor_border_color: LinearRgba::default(),
                foreground: fg,
                pane: None,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                white_space,
                filled_box,
                window_is_transparent: true,
                default_bg: bg,
                style: None,
                font: None,
                use_pixel_positioning: self.config.experimental_pixel_positioning,
                render_metrics: self.render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )
        .map(|_| ())
    }

    pub fn paint_diff_panel(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        self.diff_panel_sync_layout();
        let Some(geometry) = self.diff_panel_geometry() else {
            return Ok(());
        };
        let position = self.config.diff_panel.position;
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        let gap = FLOAT_GAP * dpi_scale;
        let pad_x = PAD_X * dpi_scale;
        let radius = RADIUS * dpi_scale;
        let stroke = dpi_scale.max(1.);
        let grip = GRIP * dpi_scale;

        // The card floats inside the panel's box, clear of the window edge,
        // the terminal beside it and whatever lies below.
        let card: RectF = euclid::rect(
            geometry.x + gap,
            geometry.y + gap,
            (geometry.width - gap * 2.).max(1.),
            (geometry.height - gap * 2.).max(1.),
        );

        let sb = self.sidebar_palette();
        let surface = opaque(sb.surface);
        let border = opaque(lerp_rgba(sb.surface, sb.text_meta, 0.35));
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
            x: geometry.x.max(0.) as usize,
            y: geometry.y.max(0.) as usize,
            width: geometry.width as usize,
            height: geometry.height as usize,
            item_type: UIItemType::DiffPanelBody,
        });

        let Some(pane) = self.get_active_pane_or_overlay() else {
            return Ok(());
        };
        let pane_id = pane.pane_id();
        self.diff_panel_refresh(&pane);

        let (green, red, amber) = {
            let palette = self.palette();
            (
                palette.colors.0[2].to_linear(),
                palette.colors.0[1].to_linear(),
                palette.colors.0[3].to_linear(),
            )
        };
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());

        // Header: title and source on the left, totals and buttons on the right.
        let header_h = (cell_h + 12. * dpi_scale).ceil();
        let header_y = card.origin.y + stroke;
        let text_y = header_y + (header_h - cell_h) * 0.5;
        let chips_expanded = self
            .diff_panel
            .views
            .get(&pane_id)
            .is_some_and(|view| view.chips_expanded);
        // The file index: one chip per file, most recently written first.
        let chips: Vec<(FileStatus, String, usize, usize, bool)> = match self
            .diff_panel
            .views
            .get(&pane_id)
            .and_then(|view| view.scan.as_ref())
        {
            Some(Scan::Changes(set)) if set.files.len() > 1 => set
                .files
                .iter()
                .take(if chips_expanded {
                    MAX_CHIPS_EXPANDED
                } else {
                    MAX_CHIPS
                })
                .map(|file| {
                    (
                        file.status,
                        view::file_name(&file.path).to_string(),
                        file.added,
                        file.removed,
                        file.fresh,
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let (source, totals, is_snapshot, summary) = match self
            .diff_panel
            .views
            .get(&pane_id)
            .and_then(|view| view.scan.as_ref())
        {
            Some(Scan::Changes(set)) => (
                set.source.label(),
                Some(set.totals()),
                set.source == ChangeSource::Snapshot,
                view::summary(set),
            ),
            _ => (String::new(), None, false, String::new()),
        };
        let total_files = totals.map_or(0, |_| {
            match self
                .diff_panel
                .views
                .get(&pane_id)
                .and_then(|v| v.scan.as_ref())
            {
                Some(Scan::Changes(set)) => set.files.len(),
                _ => 0,
            }
        });

        let button_h = (cell_h + 4. * dpi_scale).min(header_h - 4. * dpi_scale);
        let button_y = header_y + (header_h - button_h) * 0.5;
        let mut right = card.max_x() - pad_x;
        let mut buttons = vec![
            ("\u{00d7}", DiffPanelAction::Close),
            ("\u{f021}", DiffPanelAction::Refresh),
        ];
        if is_snapshot {
            buttons.push(("Reset", DiffPanelAction::ResetBaseline));
        }
        for (label, action) in buttons {
            let label_w = label.chars().count() as f32 * cell_w;
            let button_w = (label_w + 10. * dpi_scale).max(button_h);
            let button_x = right - button_w;
            if button_x < card.origin.x + pad_x {
                break;
            }
            let item_type = UIItemType::DiffPanelButton(action);
            let button_bg = if hovered.as_ref() == Some(&item_type) {
                opaque(sb.pressed_fill)
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
                contrast_label_color(button_bg, sb.text_active),
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
            right = button_x - gap;
        }
        if let Some((added, removed)) = totals.filter(|totals| *totals != (0, 0)) {
            for (text, color) in [(format!("-{removed}"), red), (format!("+{added}"), green)] {
                let width = text.chars().count() as f32 * cell_w;
                if right - width > card.origin.x + pad_x + 8. * cell_w {
                    self.diff_panel_text(
                        layers,
                        &text,
                        right - width,
                        text_y,
                        width,
                        lerp_rgba(color, sb.text_active, 0.25),
                        surface,
                        false,
                    )?;
                    right -= width + cell_w;
                }
            }
        }
        let title = "Changes";
        let title_x = card.origin.x + pad_x;
        let title_w = title.len() as f32 * cell_w;
        self.diff_panel_text(
            layers,
            title,
            title_x,
            text_y,
            (right - title_x).max(0.),
            sb.text_active,
            surface,
            true,
        )?;
        let source_x = title_x + title_w + cell_w;
        if !source.is_empty() && right - source_x >= 3. * cell_w {
            self.diff_panel_text(
                layers,
                &source,
                source_x,
                text_y,
                right - source_x,
                sb.text_meta,
                surface,
                false,
            )?;
        }

        let divider_y = header_y + header_h;
        self.filled_rectangle(
            layers,
            1,
            euclid::rect(
                card.origin.x + stroke,
                divider_y,
                card.size.width - stroke * 2.,
                stroke,
            ),
            opaque(sb.divider),
        )?;

        // Body: one scrolling column of file headers and their diff lines.
        let scroll_gutter = 8. * dpi_scale;
        let body_x = card.origin.x + stroke;
        // The set in one line, above the list it sums up.
        let mut body_y = divider_y + stroke;
        if !summary.is_empty() {
            let summary_h = (cell_h + 8. * dpi_scale).ceil();
            self.diff_panel_text(
                layers,
                &summary,
                body_x + pad_x - stroke,
                body_y + (summary_h - cell_h) * 0.5,
                (card.size.width - pad_x * 2.).max(0.),
                sb.text_idle,
                surface,
                false,
            )?;
            body_y += summary_h;
        }
        if !chips.is_empty() {
            let chip_h = (cell_h + 6. * dpi_scale).ceil();
            let chip_gap = 6. * dpi_scale;
            let chip_pad = 8. * dpi_scale;
            let strip_x = body_x + pad_x - stroke;
            let strip_w = (card.size.width - pad_x * 2.).max(0.);
            // letter, space, name, then " +a" and " -r" when non-zero.
            let counts = |added: usize, removed: usize| {
                let mut parts = Vec::new();
                if added > 0 {
                    parts.push((format!("+{added}"), green));
                }
                if removed > 0 {
                    parts.push((format!("-{removed}"), red));
                }
                parts
            };
            let widths: Vec<f32> = chips
                .iter()
                .map(|(_, name, added, removed, fresh)| {
                    let count_cols: usize = counts(*added, *removed)
                        .iter()
                        .map(|(text, _)| text.chars().count() + 1)
                        .sum();
                    let cols = 2 + name.chars().count() + count_cols + usize::from(*fresh) * 2;
                    cols as f32 * cell_w + chip_pad * 2.
                })
                .collect();
            // Room for "+999 · less", the widest the marker gets.
            let more_w = 11. * cell_w + chip_pad * 2.;
            // Opened, the index shows every file, short only of leaving the
            // diffs a few rows to stand on.
            let max_rows = if chips_expanded {
                let room = card.max_y() - body_y - 6. * cell_h;
                ((room / (chip_h + chip_gap)) as usize).max(MAX_CHIP_ROWS)
            } else {
                MAX_CHIP_ROWS
            };
            let flow = view::flow_chips(&widths, strip_w, chip_gap, max_rows, more_w);
            let chip_bg = opaque(lerp_rgba(sb.surface, sb.text_active, 0.09));
            let chip_hover_bg = opaque(lerp_rgba(sb.surface, sb.text_active, 0.2));
            for (index, ((row, x), (status, name, added, removed, fresh))) in
                flow.placed.iter().zip(&chips).enumerate()
            {
                let item_type = UIItemType::DiffPanelChip { index };
                let bg = if hovered.as_ref() == Some(&item_type) {
                    chip_hover_bg
                } else {
                    chip_bg
                };
                let chip_x = strip_x + x;
                let chip_y = body_y + *row as f32 * (chip_h + chip_gap);
                let chip_w = widths[index].min(strip_w);
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(chip_x, chip_y, chip_w, chip_h),
                    chip_h * 0.5,
                    bg,
                )?;
                let ty = chip_y + (chip_h - cell_h) * 0.5;
                let right_limit = chip_x + chip_w - chip_pad;
                let mut tx = chip_x + chip_pad;
                if *fresh {
                    self.diff_panel_text(layers, "\u{2022}", tx, ty, cell_w, amber, bg, true)?;
                    tx += cell_w * 2.;
                }
                let status_color = match status {
                    FileStatus::Added => green,
                    FileStatus::Deleted | FileStatus::Conflicted => red,
                    FileStatus::Modified | FileStatus::Renamed => amber,
                };
                self.diff_panel_text(
                    layers,
                    status.letter(),
                    tx,
                    ty,
                    cell_w,
                    lerp_rgba(status_color, sb.text_active, 0.25),
                    bg,
                    true,
                )?;
                tx += cell_w * 2.;
                let count_parts = counts(*added, *removed);
                let count_w: f32 = count_parts
                    .iter()
                    .map(|(text, _)| (text.chars().count() + 1) as f32 * cell_w)
                    .sum();
                let name_w = (right_limit - count_w - tx).max(0.);
                let name_cols = sidebar_text_cols(name_w, cell_w as usize);
                self.diff_panel_text(
                    layers,
                    &view::elide_start(name, name_cols),
                    tx,
                    ty,
                    name_w,
                    sb.text_active,
                    bg,
                    false,
                )?;
                tx = right_limit - count_w + cell_w;
                for (text, color) in count_parts {
                    let width = text.chars().count() as f32 * cell_w;
                    if tx + width <= right_limit + 0.5 {
                        self.diff_panel_text(
                            layers,
                            &text,
                            tx,
                            ty,
                            width,
                            lerp_rgba(color, sb.text_active, 0.25),
                            bg,
                            false,
                        )?;
                    }
                    tx += width + cell_w;
                }
                self.ui_items.push(UIItem {
                    x: chip_x as usize,
                    y: chip_y as usize,
                    width: chip_w as usize,
                    height: chip_h as usize,
                    item_type,
                });
            }
            // The marker ending the strip: "+N more" opens the whole index,
            // "less" closes it again.
            let hidden = flow.hidden + total_files.saturating_sub(chips.len());
            let marker = match flow.more_at {
                // Open and still not all visible: the click closes it, so say so.
                Some((row, x)) if chips_expanded => {
                    Some((row, x, format!("+{hidden} \u{00b7} less")))
                }
                Some((row, x)) => Some((row, x, format!("+{hidden} more"))),
                None if chips_expanded => {
                    let (row, x) = match flow.placed.last() {
                        Some((row, x)) => {
                            let end = x + widths[flow.placed.len() - 1].min(strip_w) + chip_gap;
                            if end + more_w > strip_w {
                                (row + 1, 0.)
                            } else {
                                (*row, end)
                            }
                        }
                        None => (0, 0.),
                    };
                    Some((row, x, "less".to_string()))
                }
                None => None,
            };
            let mut rows_used = flow.rows;
            if let Some((row, x, label)) = marker {
                rows_used = rows_used.max(row + 1);
                let item_type = UIItemType::DiffPanelButton(DiffPanelAction::ToggleChips);
                let bg = if hovered.as_ref() == Some(&item_type) {
                    chip_hover_bg
                } else {
                    surface
                };
                let marker_x = strip_x + x;
                let marker_y = body_y + row as f32 * (chip_h + chip_gap);
                let label_w = label.chars().count() as f32 * cell_w;
                let marker_w = (label_w + chip_pad * 2.).min((strip_w - x).max(0.));
                // Outlined rather than filled: it is a control, not a file.
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(marker_x, marker_y, marker_w, chip_h),
                    chip_h * 0.5,
                    opaque(lerp_rgba(sb.surface, sb.text_meta, 0.5)),
                )?;
                self.sidebar_rounded_fill(
                    layers,
                    1,
                    euclid::rect(
                        marker_x + stroke,
                        marker_y + stroke,
                        (marker_w - stroke * 2.).max(1.),
                        (chip_h - stroke * 2.).max(1.),
                    ),
                    (chip_h * 0.5 - stroke).max(0.),
                    bg,
                )?;
                self.diff_panel_text(
                    layers,
                    &label,
                    marker_x + chip_pad,
                    marker_y + (chip_h - cell_h) * 0.5,
                    (marker_w - chip_pad * 2.).max(0.),
                    sb.text_idle,
                    bg,
                    false,
                )?;
                self.ui_items.push(UIItem {
                    x: marker_x as usize,
                    y: marker_y as usize,
                    width: marker_w as usize,
                    height: chip_h as usize,
                    item_type,
                });
            }
            body_y += rows_used as f32 * (chip_h + chip_gap) + chip_gap * 0.5;
        }
        let body_w = (card.size.width - stroke * 2. - scroll_gutter).max(0.);
        let body_h = (card.max_y() - radius * 0.5 - body_y).max(0.);
        let row_h = cell_h + (2. * dpi_scale).round();
        let visible = (body_h / row_h) as usize;
        self.diff_panel.visible_rows.set(visible);

        let mut message: Vec<String> = Vec::new();
        let mut rows: Vec<PaintRow> = Vec::new();
        let (mut total_rows, mut scroll, mut digits) = (0, 0, 1);
        match self.diff_panel.views.get_mut(&pane_id) {
            None | Some(PaneView { scan: None, .. }) => {
                message.push("Reading changes\u{2026}".into())
            }
            Some(PaneView {
                scan: Some(Scan::Unavailable(reason)),
                ..
            }) => message.push(reason.clone()),
            Some(view) => {
                view.scroll = view.scroll.min(view.rows.len().saturating_sub(visible));
                total_rows = view.rows.len();
                scroll = view.scroll;
                digits = view.digits;
                let Some(Scan::Changes(set)) = &view.scan else {
                    unreachable!("the other scan states were matched above");
                };
                if set.files.is_empty() {
                    message.push("No changes".into());
                }
                message.extend(set.note.clone());
                for row in view.rows.iter().skip(scroll).take(visible) {
                    rows.push(match *row {
                        Row::File(index) => {
                            let file = &set.files[index];
                            PaintRow::File {
                                index,
                                status: file.status,
                                path: match &file.old_path {
                                    Some(old) => format!("{old} \u{2192} {}", file.path),
                                    None => file.path.clone(),
                                },
                                added: file.added,
                                removed: file.removed,
                                collapsed: view.collapsed.contains(&file.path),
                            }
                        }
                        Row::Gap => PaintRow::Gap,
                        Row::Line { file, hunk, line } => {
                            let line = &set.files[file].hunks[hunk].lines[line];
                            PaintRow::Line {
                                kind: line.kind,
                                old_no: line.old_no,
                                new_no: line.new_no,
                                text: view::display_text(&line.text),
                            }
                        }
                        Row::Note(index) => {
                            PaintRow::Note(set.files[index].note.clone().unwrap_or_default())
                        }
                    });
                }
            }
        }

        let mut y = body_y;
        if rows.is_empty() {
            y += gap;
            for text in &message {
                if y + cell_h > body_y + body_h {
                    break;
                }
                self.diff_panel_text(
                    layers,
                    text,
                    body_x + pad_x,
                    y,
                    (body_w - pad_x * 2.).max(0.),
                    sb.text_meta,
                    surface,
                    false,
                )?;
                y += row_h;
            }
        }

        let file_bg = opaque(lerp_rgba(sb.surface, sb.text_active, 0.07));
        let file_hover_bg = opaque(lerp_rgba(sb.surface, sb.text_active, 0.14));
        let added_bg = opaque(lerp_rgba(sb.surface, green, 0.16));
        let removed_bg = opaque(lerp_rgba(sb.surface, red, 0.16));
        let text_dy = (row_h - cell_h) * 0.5;
        let inset = 6. * dpi_scale;
        let gutter_w = (digits * 2 + 1) as f32 * cell_w;
        for row in rows {
            match row {
                PaintRow::File {
                    index,
                    status,
                    path,
                    added,
                    removed,
                    collapsed,
                } => {
                    let item_type = UIItemType::DiffPanelFile { index };
                    let bg = if hovered.as_ref() == Some(&item_type) {
                        file_hover_bg
                    } else {
                        file_bg
                    };
                    self.filled_rectangle(layers, 1, euclid::rect(body_x, y, body_w, row_h), bg)?;
                    let mut x = body_x + inset;
                    self.diff_panel_text(
                        layers,
                        if collapsed { "\u{25b8}" } else { "\u{25be}" },
                        x,
                        y + text_dy,
                        cell_w,
                        sb.text_meta,
                        bg,
                        false,
                    )?;
                    x += cell_w * 2.;
                    let status_color = match status {
                        FileStatus::Added => green,
                        FileStatus::Deleted | FileStatus::Conflicted => red,
                        FileStatus::Modified | FileStatus::Renamed => amber,
                    };
                    self.diff_panel_text(
                        layers,
                        status.letter(),
                        x,
                        y + text_dy,
                        cell_w,
                        lerp_rgba(status_color, sb.text_active, 0.25),
                        bg,
                        true,
                    )?;
                    x += cell_w * 2.;
                    let mut row_right = body_x + body_w - inset;
                    for (count, sign, color) in [(removed, '-', red), (added, '+', green)] {
                        if count == 0 {
                            continue;
                        }
                        let text = format!("{sign}{count}");
                        let width = text.chars().count() as f32 * cell_w;
                        if row_right - width < x + 6. * cell_w {
                            continue;
                        }
                        self.diff_panel_text(
                            layers,
                            &text,
                            row_right - width,
                            y + text_dy,
                            width,
                            lerp_rgba(color, sb.text_active, 0.25),
                            bg,
                            false,
                        )?;
                        row_right -= width + cell_w;
                    }
                    let path_w = (row_right - x).max(0.);
                    let path_cols = sidebar_text_cols(path_w, cell_w as usize);
                    self.diff_panel_text(
                        layers,
                        &view::elide_start(&path, path_cols),
                        x,
                        y + text_dy,
                        path_w,
                        sb.text_active,
                        bg,
                        false,
                    )?;
                    self.ui_items.push(UIItem {
                        x: body_x as usize,
                        y: y as usize,
                        width: body_w as usize,
                        height: row_h as usize,
                        item_type,
                    });
                }
                PaintRow::Gap => {
                    self.diff_panel_text(
                        layers,
                        "\u{22ef}",
                        body_x + inset + gutter_w + cell_w,
                        y + text_dy,
                        cell_w,
                        sb.text_meta,
                        surface,
                        false,
                    )?;
                }
                PaintRow::Note(note) => {
                    self.diff_panel_text(
                        layers,
                        &note,
                        body_x + inset + cell_w * 4.,
                        y + text_dy,
                        (body_w - inset * 2. - cell_w * 4.).max(0.),
                        sb.text_meta,
                        surface,
                        false,
                    )?;
                }
                PaintRow::Line {
                    kind,
                    old_no,
                    new_no,
                    text,
                } => {
                    let (bg, marker, marker_color) = match kind {
                        LineKind::Added => (added_bg, "+", green),
                        LineKind::Removed => (removed_bg, "-", red),
                        LineKind::Context => (surface, " ", sb.text_meta),
                    };
                    if kind != LineKind::Context {
                        self.filled_rectangle(
                            layers,
                            1,
                            euclid::rect(body_x, y, body_w, row_h),
                            bg,
                        )?;
                    }
                    let number = |no: Option<u32>| match no {
                        Some(no) => format!("{no:>digits$}"),
                        None => " ".repeat(digits),
                    };
                    let mut x = body_x + inset;
                    self.diff_panel_text(
                        layers,
                        &format!("{} {}", number(old_no), number(new_no)),
                        x,
                        y + text_dy,
                        gutter_w,
                        sb.text_meta,
                        bg,
                        false,
                    )?;
                    x += gutter_w + cell_w;
                    self.diff_panel_text(
                        layers,
                        marker,
                        x,
                        y + text_dy,
                        cell_w,
                        lerp_rgba(marker_color, sb.text_active, 0.25),
                        bg,
                        true,
                    )?;
                    x += cell_w * 2.;
                    self.diff_panel_text(
                        layers,
                        &text,
                        x,
                        y + text_dy,
                        (body_x + body_w - inset - x).max(0.),
                        if kind == LineKind::Context {
                            sb.text_idle
                        } else {
                            sb.text_active
                        },
                        bg,
                        false,
                    )?;
                }
            }
            y += row_h;
        }

        // Where in the list the visible rows are.
        if total_rows > visible && visible > 0 {
            let thumb_w = 3. * dpi_scale;
            let thumb_h = (body_h * visible as f32 / total_rows as f32).max(24. * dpi_scale);
            let travel = (body_h - thumb_h).max(0.);
            let thumb_y = body_y + travel * scroll as f32 / (total_rows - visible) as f32;
            self.sidebar_pill_fill(
                layers,
                1,
                euclid::rect(
                    body_x + body_w + (scroll_gutter - thumb_w) * 0.5,
                    thumb_y,
                    thumb_w,
                    thumb_h.min(body_h),
                ),
                thumb_w * 0.5,
                opaque(lerp_rgba(sb.surface, sb.text_active, 0.3)),
            )?;
        }

        // Draggable edges: the one facing the terminal, the bottom, and the
        // corner between them. A pill marks whichever the pointer is on.
        let start_reserved = self.diff_panel_reserved_width();
        let inner_x = match position {
            DiffPanelPosition::Left => geometry.right() - grip,
            DiffPanelPosition::Right => geometry.x,
        };
        let bottom_y = geometry.bottom() - grip;
        let active_edge = self
            .dragging
            .as_ref()
            .map(|(item, _)| &item.item_type)
            .or(self.last_ui_item.as_ref().map(|item| &item.item_type))
            .and_then(|item_type| match item_type {
                UIItemType::DiffPanelResize { edge, .. } => Some(*edge),
                _ => None,
            });
        let pill = opaque(lerp_rgba(sb.surface, sb.text_active, 0.55));
        let pill_thickness = (3. * dpi_scale).min(grip);
        let pill_length = 36. * dpi_scale;
        if matches!(
            active_edge,
            Some(DiffPanelEdge::Width | DiffPanelEdge::Corner)
        ) {
            self.sidebar_pill_fill(
                layers,
                1,
                euclid::rect(
                    inner_x + (grip - pill_thickness) * 0.5,
                    geometry.y + (geometry.height - pill_length) * 0.5,
                    pill_thickness,
                    pill_length.min(geometry.height),
                ),
                pill_thickness * 0.5,
                pill,
            )?;
        }
        if matches!(
            active_edge,
            Some(DiffPanelEdge::Height | DiffPanelEdge::Corner)
        ) {
            self.sidebar_pill_fill(
                layers,
                1,
                euclid::rect(
                    geometry.x + (geometry.width - pill_length) * 0.5,
                    bottom_y + (grip - pill_thickness) * 0.5,
                    pill_length.min(geometry.width),
                    pill_thickness,
                ),
                pill_thickness * 0.5,
                pill,
            )?;
        }

        let mut push_grip = |x: f32, y: f32, width: f32, height: f32, edge: DiffPanelEdge| {
            self.ui_items.push(UIItem {
                x: x.max(0.) as usize,
                y: y.max(0.) as usize,
                width: width.max(1.) as usize,
                height: height.max(1.) as usize,
                item_type: UIItemType::DiffPanelResize {
                    edge,
                    start_reserved,
                },
            });
        };
        push_grip(
            inner_x,
            geometry.y,
            grip,
            geometry.height,
            DiffPanelEdge::Width,
        );
        push_grip(
            geometry.x,
            bottom_y,
            geometry.width,
            grip,
            DiffPanelEdge::Height,
        );
        // Last, so the corner wins where the two edges cross.
        let corner = grip * 2.;
        let corner_x = match position {
            DiffPanelPosition::Left => geometry.right() - corner,
            DiffPanelPosition::Right => geometry.x,
        };
        push_grip(
            corner_x,
            geometry.bottom() - corner,
            corner,
            corner,
            DiffPanelEdge::Corner,
        );

        Ok(())
    }
}
