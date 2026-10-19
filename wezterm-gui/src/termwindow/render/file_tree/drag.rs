//! Dragging in the Worktree panel: rows onto a folder (they move there) or
//! onto a terminal pane (their paths are pasted into it), and the scroll
//! bar's thumb and track.
//!
//! A press on a row only arms a drag; it starts once the pointer has moved a
//! few pixels, and a release before that is an ordinary click.

use super::*;
use crate::file_tree::ops::{self, PasteSyntax};

/// Pixels the pointer moves before a press on a row becomes a drag.
const DRAG_THRESHOLD: isize = 4;
/// How long a dragged row hovers over a closed folder before it opens.
const HOVER_OPEN: Duration = Duration::from_millis(600);
/// Shortest the scroll bar's thumb gets, in pixels at 1x.
const MIN_THUMB: f32 = 16.;

/// Where a dragged row would land if released now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DropTarget {
    Nothing,
    /// Move into this folder (`""` is the root).
    Folder(String),
    /// Paste the paths into this pane.
    Pane(PaneId),
}

pub(super) struct RowDrag {
    tab_id: TabId,
    /// The row pressed, and what it showed.
    index: usize,
    rel: String,
    start: (isize, isize),
    /// The pointer moved far enough: this is a drag, not a click.
    active: bool,
    sources: Vec<(String, EntryKind)>,
    pointer: (f32, f32),
    target: DropTarget,
    /// A closed folder under the pointer, and since when.
    hover: Option<(String, Instant)>,
}

impl RowDrag {
    /// The folder a drop would move into, while the drag is over one.
    pub(super) fn folder_target(&self) -> Option<&str> {
        match &self.target {
            DropTarget::Folder(rel) if self.active => Some(rel.as_str()),
            _ => None,
        }
    }
}

/// The scroll bar as last painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Track {
    top: f32,
    height: f32,
    thumb_top: f32,
    thumb_h: f32,
}

impl Painted {
    /// The top and bottom of what the rows and the preview share.
    pub(super) fn area(&self) -> (f32, f32) {
        self.area
    }
}

/// What the last paint put where, for the drags that follow it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Painted {
    /// What the rows and the preview share.
    area: (f32, f32),
    body_top: f32,
    body_bottom: f32,
    row_h: f32,
    track: Option<Track>,
}

/// The panel's body as [`super`]'s paint laid it out.
pub(super) struct Body {
    pub card: RectF,
    pub area: (f32, f32),
    pub top: f32,
    pub bottom: f32,
    pub row_h: f32,
    pub total: usize,
    pub visible: usize,
    pub scroll: usize,
}

impl crate::TermWindow {
    /// Press, wheel and middle click on a row.
    pub fn mouse_event_file_tree_row(
        &mut self,
        item: UIItem,
        index: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        let row = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.rows.get(index))
            .cloned();
        match event.kind {
            WMEK::Press(MousePress::Right) => self.file_tree_open_menu(Some(index), &event),
            // The field a new row is named in is not a row to drag or click.
            WMEK::Press(MousePress::Left)
                if row.as_ref().is_some_and(|row| row.rel == menu::EDIT_ROW) => {}
            WMEK::Press(MousePress::Left) => {
                let Some(row) = row else {
                    return;
                };
                self.file_tree.drag = Some(RowDrag {
                    tab_id,
                    index,
                    rel: row.rel,
                    start: (event.coords.x, event.coords.y),
                    active: false,
                    sources: Vec::new(),
                    pointer: (event.coords.x as f32, event.coords.y as f32),
                    target: DropTarget::Nothing,
                    hover: None,
                });
                self.pressed_ui_item = Some(item.item_type.clone());
                self.dragging.replace((item, event));
            }
            // Middle click opens a file in a new tab.
            WMEK::Press(MousePress::Middle) => {
                if let Some(row) = row.filter(|row| row.kind == EntryKind::File) {
                    self.file_tree_open_file(tab_id, &row.rel, true);
                }
            }
            WMEK::Release(MousePress::Left) => {
                // A release without the press that arms a drag (the press
                // landed elsewhere): an ordinary click.
                self.pressed_ui_item = None;
                self.file_tree_click_row(tab_id, index, event.modifiers);
                context.invalidate();
            }
            _ => self.mouse_event_file_tree(None, event, context),
        }
    }

    pub fn drag_file_tree_row(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        self.dragging.replace((item, start_event));
        let Some(mut drag) = self.file_tree.drag.take() else {
            return;
        };
        let (x, y) = (event.coords.x, event.coords.y);
        if !drag.active {
            let moved = (x - drag.start.0).abs().max((y - drag.start.1).abs());
            if moved < DRAG_THRESHOLD {
                self.file_tree.drag = Some(drag);
                return;
            }
            drag.active = true;
            // Dragging a selected row takes the whole selection along;
            // dragging any other row takes just it, and selects it.
            if let Some(view) = self.file_tree.views.get_mut(&drag.tab_id) {
                if view.is_marked(&drag.rel) {
                    drag.sources = view.marked_rows();
                } else {
                    view.select_only(Some(drag.rel.clone()));
                    drag.sources = view.marked_rows();
                }
            }
            self.file_tree.focused = true;
        }
        drag.pointer = (x as f32, y as f32);
        let (target, under) =
            self.file_tree_drop_target(drag.tab_id, x as f32, y as f32, &drag.sources);
        drag.target = target;
        // A closed folder held under the pointer opens after a moment.
        drag.hover = match (under, drag.hover.take()) {
            (Some(rel), Some((held, since))) if held == rel => Some((held, since)),
            (Some(rel), _) => {
                let since = Instant::now();
                self.update_next_frame_time(Some(since + HOVER_OPEN));
                Some((rel, since))
            }
            (None, _) => None,
        };
        context.set_cursor(Some(match drag.target {
            DropTarget::Nothing => CursorIcon::NoDrop,
            DropTarget::Folder(_) => CursorIcon::Grabbing,
            DropTarget::Pane(_) => CursorIcon::Copy,
        }));
        self.file_tree_autoscroll(drag.tab_id, y as f32);
        self.file_tree.drag = Some(drag);
        context.invalidate();
    }

    /// Near the body's top or bottom edge a drag scrolls the tree, a row per
    /// pointer move.
    fn file_tree_autoscroll(&mut self, tab_id: TabId, y: f32) {
        let Some(painted) = self.file_tree.painted.get() else {
            return;
        };
        let visible = self.file_tree.visible_rows.get();
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        let max = view.rows.len().saturating_sub(visible);
        let edge = painted.row_h;
        if y < painted.body_top + edge && y >= painted.body_top - edge {
            view.scroll = view.scroll.saturating_sub(1);
        } else if y > painted.body_bottom - edge && y <= painted.body_bottom + edge {
            view.scroll = (view.scroll + 1).min(max);
        }
    }

    /// The release that ends a press on a row: a click when the pointer
    /// hardly moved, a drop otherwise.
    pub fn finish_file_tree_row_drag(&mut self, event: &MouseEvent, context: &dyn WindowOps) {
        let Some(drag) = self.file_tree.drag.take() else {
            return;
        };
        context.set_cursor(Some(CursorIcon::Default));
        if !drag.active {
            self.file_tree_click_row(drag.tab_id, drag.index, event.modifiers);
        } else {
            match drag.target {
                DropTarget::Folder(dest) => self.file_tree_move(drag.tab_id, drag.sources, dest),
                DropTarget::Pane(pane_id) => {
                    self.file_tree_paste_paths(drag.tab_id, &drag.sources, pane_id)
                }
                DropTarget::Nothing => {}
            }
        }
        context.invalidate();
    }

    /// Where a drop at `(x, y)` would go, and the closed folder under the
    /// pointer, if any.
    fn file_tree_drop_target(
        &self,
        tab_id: TabId,
        x: f32,
        y: f32,
        sources: &[(String, EntryKind)],
    ) -> (DropTarget, Option<String>) {
        let Some(view) = self.file_tree.views.get(&tab_id) else {
            return (DropTarget::Nothing, None);
        };
        let inside_panel = self
            .file_tree_geometry()
            .is_some_and(|panel| panel.contains(euclid::point2(x, y)));
        if !inside_panel {
            return match self.file_tree_pane_at(x, y) {
                Some((pane_id, _)) => (DropTarget::Pane(pane_id), None),
                None => (DropTarget::Nothing, None),
            };
        }
        let row = self.ui_items.iter().find_map(|item| match item.item_type {
            UIItemType::FileTreeRow { index }
                if (item.x as f32..(item.x + item.width) as f32).contains(&x)
                    && (item.y as f32..(item.y + item.height) as f32).contains(&y) =>
            {
                view.rows.get(index)
            }
            _ => None,
        });
        let (dest, under) = match row {
            Some(row) if row.kind == EntryKind::Dir => (
                row.rel.clone(),
                (row.state == RowState::Collapsed).then(|| row.rel.clone()),
            ),
            Some(row) => (parent_rel(&row.rel).unwrap_or("").to_string(), None),
            // Below the last row: the root.
            None => (String::new(), None),
        };
        let target = if ops::is_drop_target(sources, &dest) {
            DropTarget::Folder(dest)
        } else {
            DropTarget::Nothing
        };
        (target, under)
    }

    /// The terminal pane under `(x, y)`, with where it is on the window.
    fn file_tree_pane_at(&self, x: f32, y: f32) -> Option<(PaneId, RectF)> {
        self.file_tree_pane_rects()
            .into_iter()
            .find(|(_, rect)| rect.contains(euclid::point2(x, y)))
    }

    fn file_tree_pane_rects(&self) -> Vec<(PaneId, RectF)> {
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar && !self.sidebar_is_active() {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let top_bar_height = if self.config.tab_bar_at_bottom {
            0.
        } else {
            tab_bar_height
        };
        let border = self.get_os_border();
        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        self.get_panes_to_render()
            .iter()
            .map(|pos| {
                let x = border.left.get() as f32 + padding_left + pos.left as f32 * cell_w;
                let y = border.top.get() as f32
                    + top_bar_height
                    + padding_top
                    + pos.top as f32 * cell_h;
                (
                    pos.pane.pane_id(),
                    euclid::rect(x, y, pos.pixel_width as f32, pos.pixel_height as f32),
                )
            })
            .collect()
    }

    /// Move `sources` into the folder `dest`, asking first for more than one.
    fn file_tree_move(&mut self, tab_id: TabId, sources: Vec<(String, EntryKind)>, dest: String) {
        let Some(root) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| tree.root.clone())
        else {
            return;
        };
        let plan = match ops::plan_move(&sources, &dest) {
            Ok(plan) if plan.is_empty() => return,
            Ok(plan) => plan,
            Err(reason) => {
                file_tree_toast("Not moved", reason);
                return;
            }
        };
        let into = if dest.is_empty() {
            "the top folder".to_string()
        } else {
            dest.clone()
        };
        let run = move |term_window: &mut crate::TermWindow| {
            let Some(window) = term_window.window.clone() else {
                return;
            };
            std::thread::spawn(move || {
                let outcome = ops::apply_moves(&root, &plan);
                window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    term_window.file_tree_moved(tab_id, &dest, &plan, outcome);
                })));
            });
        };
        if sources.len() > 1 {
            self.file_tree_confirm(format!("Move {} items into {into}?", sources.len()), run);
        } else {
            run(self);
        }
    }

    fn file_tree_moved(
        &mut self,
        tab_id: TabId,
        dest: &str,
        plan: &[ops::Move],
        outcome: Result<usize, String>,
    ) {
        if let Err(reason) = &outcome {
            file_tree_toast("Not moved", reason.clone());
        }
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        // Land on what moved, in its new folder.
        if outcome.is_ok() {
            if !dest.is_empty() {
                view.expanded.insert(dest.to_string());
                view.reveal(dest);
                view.expanded_changed_at = Some(Instant::now());
            }
            view.marked = plan.iter().map(|planned| planned.to.clone()).collect();
            view.selected = plan.first().map(|planned| planned.to.clone());
            view.anchor = view.selected.clone();
        }
        view.scanned_at = None;
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Paste the paths of `sources` into the pane `pane_id`, spelled the way
    /// the program there reads them, and move the keyboard there.
    pub(super) fn file_tree_paste_paths(
        &mut self,
        tab_id: TabId,
        sources: &[(String, EntryKind)],
        pane_id: PaneId,
    ) {
        let mux = Mux::get();
        let Some(pane) = mux.get_pane(pane_id) else {
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
        let distro = mux
            .get_domain(pane.domain_id())
            .and_then(|domain| wsl_paths::distro_for_domain(domain.domain_name(), &self.config));
        let paths: Vec<String> = sources
            .iter()
            .map(|(rel, _)| {
                let native = tree.abs_path(rel).to_string_lossy().to_string();
                match distro.as_deref() {
                    Some(distro) => wsl_paths::windows_to_wsl(&native, distro).unwrap_or(native),
                    None => native,
                }
            })
            .collect();
        let syntax = if distro.is_none() && cfg!(windows) {
            PasteSyntax::Windows
        } else {
            PasteSyntax::Posix
        };
        if let Err(err) = pane.send_paste(&ops::paste_text(&paths, syntax)) {
            log::warn!("file tree: paste into pane {pane_id} failed: {err:#}");
            return;
        }
        self.file_tree.focused = false;
        if let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) {
            tab.set_active_pane(&pane);
        }
    }

    /// Ask `message` in an overlay over the tab; `on_yes` runs on the GUI
    /// thread if the answer is yes.
    pub(super) fn file_tree_confirm(
        &mut self,
        message: String,
        on_yes: impl FnOnce(&mut crate::TermWindow) + Send + Sync + 'static,
    ) {
        let Some(tab) = Mux::get().get_active_tab_for_window(self.mux_window_id) else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let (overlay, future) =
            crate::overlay::start_overlay(self, &tab, move |tab_id, mut term| {
                if crate::overlay::confirm::run_confirmation(&message, &mut term)? {
                    window.notify(TermWindowNotif::Apply(Box::new(on_yes)));
                }
                crate::TermWindow::schedule_cancel_overlay(window, tab_id, None);
                Ok(())
            });
        self.assign_overlay(tab.tab_id(), overlay);
        promise::spawn::spawn(future).detach();
    }

    /// The scroll bar: a track to click and a thumb to drag, drawn only when
    /// the rows do not all fit. Also records where the body was painted.
    pub(super) fn paint_file_tree_scroll_bar(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        body: Body,
        surface: LinearRgba,
        meta: LinearRgba,
        hovered: &Option<UIItemType>,
    ) -> anyhow::Result<()> {
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let track_h = body.bottom - body.top;
        let thumb = tree_view::thumb(
            track_h,
            body.total,
            body.visible,
            body.scroll,
            MIN_THUMB * dpi_scale,
        );
        let track = thumb.map(|(offset, thumb_h)| Track {
            top: body.top,
            height: track_h,
            thumb_top: body.top + offset,
            thumb_h,
        });
        self.file_tree.painted.set(Some(Painted {
            area: body.area,
            body_top: body.top,
            body_bottom: body.bottom,
            row_h: body.row_h,
            track,
        }));
        let Some(track) = track else {
            return Ok(());
        };
        let hit_w = 12. * dpi_scale;
        let hit_x = body.card.max_x() - hit_w - 2. * dpi_scale;
        let hot = matches!(
            hovered,
            Some(UIItemType::FileTreeScrollThumb | UIItemType::FileTreeScrollTrack)
        ) || matches!(
            self.dragging.as_ref().map(|(item, _)| &item.item_type),
            Some(UIItemType::FileTreeScrollThumb)
        );
        // Thin at rest, wider and stronger while pointed at or dragged, like
        // the terminal's own scroll bar.
        let width = if hot { 6. } else { 4. } * dpi_scale;
        self.sidebar_pill_fill(
            layers,
            2,
            euclid::rect(
                body.card.max_x() - 7. * dpi_scale - width * 0.5,
                track.thumb_top,
                width,
                track.thumb_h,
            ),
            width * 0.5,
            lerp_rgba(surface, meta, if hot { 0.75 } else { 0.5 }),
        )?;
        self.ui_items.push(UIItem {
            x: hit_x as usize,
            y: track.top as usize,
            width: hit_w as usize,
            height: track.height as usize,
            item_type: UIItemType::FileTreeScrollTrack,
        });
        self.ui_items.push(UIItem {
            x: hit_x as usize,
            y: track.thumb_top as usize,
            width: hit_w as usize,
            height: track.thumb_h.max(1.) as usize,
            item_type: UIItemType::FileTreeScrollThumb,
        });
        Ok(())
    }

    /// A hairline frame around `rect`.
    pub(super) fn paint_file_tree_outline(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        rect: RectF,
        color: LinearRgba,
        stroke: f32,
    ) -> anyhow::Result<()> {
        let w = stroke.max(1.) * 1.5;
        for edge in [
            euclid::rect(rect.origin.x, rect.origin.y, rect.size.width, w),
            euclid::rect(rect.origin.x, rect.max_y() - w, rect.size.width, w),
            euclid::rect(rect.origin.x, rect.origin.y, w, rect.size.height),
            euclid::rect(rect.max_x() - w, rect.origin.y, w, rect.size.height),
        ] {
            self.filled_rectangle(layers, 2, edge, color)?;
        }
        Ok(())
    }

    /// While a row is dragged: the pane it would paste into, outlined, and a
    /// label following the pointer. Also opens a closed folder the drag has
    /// rested on.
    pub(super) fn paint_file_tree_drag(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        surface: LinearRgba,
        sb: &crate::termwindow::render::sidebar::SidebarPalette,
    ) -> anyhow::Result<()> {
        let Some(drag) = self.file_tree.drag.as_ref().filter(|drag| drag.active) else {
            return Ok(());
        };
        let tab_id = drag.tab_id;
        let target = drag.target.clone();
        let pointer = drag.pointer;
        let label = match drag.sources.as_slice() {
            [(rel, kind)] => {
                let name = rel.rsplit('/').next().unwrap_or(rel);
                let icon = if *kind == EntryKind::Dir {
                    "\u{f07b}"
                } else {
                    "\u{f15b}"
                };
                format!("{icon}  {name}")
            }
            many => format!("\u{f0c5}  {} items", many.len()),
        };
        let rested = drag
            .hover
            .as_ref()
            .filter(|(_, since)| since.elapsed() >= HOVER_OPEN)
            .map(|(rel, _)| rel.clone());
        if let Some(rel) = rested {
            if let Some(drag) = self.file_tree.drag.as_mut() {
                drag.hover = None;
            }
            let closed = self
                .file_tree
                .views
                .get(&tab_id)
                .is_some_and(|view| !view.expanded.contains(&rel));
            if closed {
                self.file_tree_toggle_folder(tab_id, &rel);
                if let Some(window) = self.window.as_ref() {
                    window.invalidate();
                }
            }
        }

        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let accent = sb.accent();
        if let DropTarget::Pane(pane_id) = target {
            if let Some((_, rect)) = self
                .file_tree_pane_rects()
                .into_iter()
                .find(|(id, _)| *id == pane_id)
            {
                self.filled_rectangle(layers, 2, rect, accent.mul_alpha(0.08))?;
                self.paint_file_tree_outline(layers, rect, accent, dpi_scale)?;
            }
        }

        let cell_w = self.render_metrics.cell_size.width as f32;
        let cell_h = self.render_metrics.cell_size.height as f32;
        let pad = 8. * dpi_scale;
        let width = label.chars().count() as f32 * cell_w + pad * 2.;
        let height = cell_h + 8. * dpi_scale;
        let ghost = euclid::rect(
            pointer.0 + 14. * dpi_scale,
            pointer.1 + 10. * dpi_scale,
            width,
            height,
        );
        let fill = opaque(lerp_rgba(surface, sb.active_fill, 0.9));
        let edge = if target == DropTarget::Nothing {
            opaque(lerp_rgba(surface, sb.text_meta, 0.5))
        } else {
            accent
        };
        self.sidebar_rounded_fill(layers, 1, ghost, 6. * dpi_scale, edge)?;
        let stroke = dpi_scale.max(1.);
        let inner = euclid::rect(
            ghost.origin.x + stroke,
            ghost.origin.y + stroke,
            (ghost.size.width - stroke * 2.).max(1.),
            (ghost.size.height - stroke * 2.).max(1.),
        );
        self.sidebar_rounded_fill(layers, 1, inner, 5. * dpi_scale, fill)?;
        self.diff_panel_text(
            layers,
            &label,
            ghost.origin.x + pad,
            ghost.origin.y + (height - cell_h) * 0.5,
            width - pad * 2.,
            sb.text_active,
            fill,
            false,
        )?;
        Ok(())
    }

    /// Press and wheel on the scroll bar: a press on the track pages towards
    /// the pointer, one on the thumb starts dragging it.
    pub fn mouse_event_file_tree_scroll(
        &mut self,
        item: UIItem,
        on_thumb: bool,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        match event.kind {
            WMEK::Press(MousePress::Left) if on_thumb => {
                self.pressed_ui_item = Some(item.item_type.clone());
                self.dragging.replace((item, event));
            }
            WMEK::Press(MousePress::Left) => {
                let Some(track) = self.file_tree.painted.get().and_then(|p| p.track) else {
                    return;
                };
                let visible = self.file_tree.visible_rows.get();
                let Some(tab_id) = self.file_tree_active_tab() else {
                    return;
                };
                if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
                    let max = view.rows.len().saturating_sub(visible);
                    view.scroll = if (event.coords.y as f32) < track.thumb_top {
                        view.scroll.saturating_sub(visible)
                    } else {
                        (view.scroll + visible).min(max)
                    };
                }
                context.invalidate();
            }
            _ => self.mouse_event_file_tree(None, event, context),
        }
    }

    pub fn drag_file_tree_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        self.dragging.replace((item.clone(), start_event.clone()));
        let Some(track) = self.file_tree.painted.get().and_then(|p| p.track) else {
            return;
        };
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        let visible = self.file_tree.visible_rows.get();
        // The thumb's top as it was at the press, moved with the pointer.
        let delta = (event.coords.y - start_event.coords.y) as f32;
        let offset = item.y as f32 - track.top + delta;
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            let next = tree_view::scroll_for_thumb(
                track.height,
                track.thumb_h,
                view.rows.len(),
                visible,
                offset,
            );
            if next != view.scroll {
                view.scroll = next;
                context.invalidate();
            }
        }
    }
}

pub(super) fn file_tree_toast(title: &str, message: String) {
    wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
        title: title.to_string(),
        message,
        url: None,
        timeout: Some(Duration::from_millis(4000)),
    });
}
