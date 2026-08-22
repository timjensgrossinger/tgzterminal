use crate::agent_herd::AgentKey;
use crate::tabbar::TabBarItem;
use crate::termwindow::{
    AgentCopyAction, AgentCopyMenuState, AgentLaunchMenuState, AgentRowAction, AgentToolbeltAction,
    CloseTabMenuAction, CloseTabMenuState, CloseTabSource, ExpandedMenuRow, GuiWin, MouseCapture,
    PositionedSplit, ScrollHit, SshLaunchMenuState, TermWindowNotif, UIItem, UIItemType, TMB,
};
use ::window::{
    MouseButtons as WMB, MouseCursor, MouseEvent, MouseEventKind as WMEK, MousePress,
    WindowDecorations, WindowOps, WindowState,
};
use config::keyassignment::{
    ClipboardCopyDestination, KeyAssignment, MouseEventTrigger, SpawnTabDomain,
};
use config::MouseEventAltScreen;
use mux::pane::{Pane, PaneId, WithPaneLines};
use mux::tab::SplitDirection;
use mux::Mux;
use mux_lua::MuxPane;
use std::convert::TryInto;
use std::ops::Sub;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::Line;
use wezterm_dynamic::ToDynamic;
use wezterm_open_url::open_url;
use wezterm_term::input::{MouseButton, MouseEventKind as TMEK};
use wezterm_term::{ClickPosition, KeyCode, KeyModifiers, LastMouseClick, StableRowIndex};

impl super::TermWindow {
    fn resolve_ui_item(&self, event: &MouseEvent) -> Option<UIItem> {
        let x = event.coords.x;
        let y = event.coords.y;
        self.ui_items
            .iter()
            .rev()
            .find(|item| item.hit_test(x, y))
            .cloned()
    }

    fn leave_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {
                self.update_title_post_status();
            }
            UIItemType::SidebarTab { .. }
            | UIItemType::SidebarTabExpand { .. }
            | UIItemType::SidebarPaneRow { .. }
            | UIItemType::SidebarPaneClose { .. }
            | UIItemType::SidebarTabList
            | UIItemType::SidebarScrollTrack
            | UIItemType::SidebarScrollThumb
            | UIItemType::CloseTab(_)
            | UIItemType::SidebarCloseTab(_)
            | UIItemType::CloseTabMenuItem { .. }
            | UIItemType::SidebarResize { .. }
            | UIItemType::SidebarSearch
            | UIItemType::SidebarAutoHideToggle
            | UIItemType::SidebarWorktreeButton
            | UIItemType::SidebarAgentLaunchButton
            | UIItemType::SidebarAgentMenuItem { .. }
            | UIItemType::SidebarAgentMenuProjectRootToggle
            | UIItemType::SidebarAgentMenuHerd
            | UIItemType::SidebarAgentMenuTarget { .. }
            | UIItemType::SidebarAgentMenuResume
            | UIItemType::SidebarAgentMenuResumeSession { .. }
            | UIItemType::SidebarAgentMenuRestoreLastWindow
            | UIItemType::SidebarAgentSectionHeader
            | UIItemType::SidebarAgentRow { .. }
            | UIItemType::SidebarAgentRowChevron { .. }
            | UIItemType::SidebarAgentAction { .. }
            | UIItemType::SidebarNewTabMenuButton
            | UIItemType::SidebarNewTabMenuItem { .. }
            | UIItemType::SidebarSshLaunchButton
            | UIItemType::SidebarSshMenuItem { .. }
            | UIItemType::AgentToolbeltButton { .. }
            | UIItemType::AgentCopyMenuItem { .. }
            | UIItemType::SidebarWaitingCounter
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_) => {}
        }
    }

    fn enter_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {}
            UIItemType::SidebarTab { .. }
            | UIItemType::SidebarTabExpand { .. }
            | UIItemType::SidebarPaneRow { .. }
            | UIItemType::SidebarPaneClose { .. }
            | UIItemType::SidebarTabList
            | UIItemType::SidebarScrollTrack
            | UIItemType::SidebarScrollThumb
            | UIItemType::CloseTab(_)
            | UIItemType::SidebarCloseTab(_)
            | UIItemType::CloseTabMenuItem { .. }
            | UIItemType::SidebarResize { .. }
            | UIItemType::SidebarSearch
            | UIItemType::SidebarAutoHideToggle
            | UIItemType::SidebarWorktreeButton
            | UIItemType::SidebarAgentLaunchButton
            | UIItemType::SidebarAgentMenuItem { .. }
            | UIItemType::SidebarAgentMenuProjectRootToggle
            | UIItemType::SidebarAgentMenuHerd
            | UIItemType::SidebarAgentMenuTarget { .. }
            | UIItemType::SidebarAgentMenuResume
            | UIItemType::SidebarAgentMenuResumeSession { .. }
            | UIItemType::SidebarAgentMenuRestoreLastWindow
            | UIItemType::SidebarAgentSectionHeader
            | UIItemType::SidebarAgentRow { .. }
            | UIItemType::SidebarAgentRowChevron { .. }
            | UIItemType::SidebarAgentAction { .. }
            | UIItemType::SidebarNewTabMenuButton
            | UIItemType::SidebarNewTabMenuItem { .. }
            | UIItemType::SidebarSshLaunchButton
            | UIItemType::SidebarSshMenuItem { .. }
            | UIItemType::AgentToolbeltButton { .. }
            | UIItemType::AgentCopyMenuItem { .. }
            | UIItemType::SidebarWaitingCounter
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_) => {}
        }
    }

    pub fn mouse_event_impl(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        log::trace!("{:?}", event);
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        self.current_mouse_event.replace(event.clone());
        if self.update_sidebar_auto_hide_state() {
            context.invalidate();
        }

        let border = self.get_os_border();

        let first_line_offset =
            if self.show_tab_bar && !self.sidebar_is_active() && !self.config.tab_bar_at_bottom {
                self.tab_bar_pixel_height().unwrap_or(0.) as isize
            } else {
                0
            } + border.top.get() as isize;

        let (padding_left, padding_top) = self.padding_left_top();

        let y = (event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset)
            .max(0)
            / self.render_metrics.cell_size.height) as i64;

        let x = (event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize)
            .max(0) as f32)
            / self.render_metrics.cell_size.width as f32;
        let x = if !pane.is_mouse_grabbed() {
            // Round the x coordinate so that we're a bit more forgiving of
            // the horizontal position when selecting cells
            x.round()
        } else {
            x
        }
        .trunc() as usize;

        let mut y_pixel_offset = event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset);
        if y > 0 {
            y_pixel_offset = y_pixel_offset.max(0) % self.render_metrics.cell_size.height;
        }

        let mut x_pixel_offset = event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize);
        if x > 0 {
            x_pixel_offset = x_pixel_offset.max(0) % self.render_metrics.cell_size.width;
        }

        self.last_mouse_coords = (x, y);

        let mut capture_mouse = false;

        match event.kind {
            WMEK::Release(ref press) => {
                self.current_mouse_capture = None;
                self.current_mouse_buttons.retain(|p| p != press);
                if press == &MousePress::Left && self.window_drag_position.take().is_some() {
                    self.pressed_ui_item = None;
                    // Completed a window drag
                    return;
                }
                if press == &MousePress::Left {
                    if let Some((item, _)) = self.dragging.take() {
                        let dropped_tab = match &item.item_type {
                            UIItemType::SidebarTab { tab_idx, .. } => Some(*tab_idx),
                            _ => None,
                        };
                        self.pressed_ui_item = None;
                        if matches!(item.item_type, UIItemType::SidebarResize { .. }) {
                            self.finish_sidebar_resize();
                        }
                        if let Some(tab_idx) = dropped_tab {
                            self.sidebar_drop_flash = Some((tab_idx, Instant::now()));
                            *self.has_animation.borrow_mut() = Some(Instant::now());
                            context.invalidate();
                        }
                        if self.update_sidebar_auto_hide_state() {
                            context.invalidate();
                        }
                        // Completed a drag
                        return;
                    }
                }
            }

            WMEK::Press(ref press) => {
                capture_mouse = true;

                // Perform click counting
                let button = mouse_press_to_tmb(press);

                let click_position = ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                };

                let click = match self.last_mouse_click.take() {
                    None => LastMouseClick::new(button, click_position),
                    Some(click) => click.add(button, click_position),
                };
                self.last_mouse_click = Some(click);
                self.current_mouse_buttons.retain(|p| p != press);
                self.current_mouse_buttons.push(*press);
            }

            WMEK::Move => {
                if let Some(start) = self.window_drag_position.as_ref() {
                    // Dragging the window
                    // Compute the distance since the initial event
                    let delta_x = start.screen_coords.x - event.screen_coords.x;
                    let delta_y = start.screen_coords.y - event.screen_coords.y;

                    // Now compute a new window position.
                    // We don't have a direct way to get the position,
                    // but we can infer it by comparing the mouse coords
                    // with the screen coords in the initial event.
                    // This computes the original top_left position,
                    // and applies the total drag delta to it.
                    let top_left = ::window::ScreenPoint::new(
                        (start.screen_coords.x - start.coords.x) - delta_x,
                        (start.screen_coords.y - start.coords.y) - delta_y,
                    );
                    // and now tell the window to go there
                    context.set_window_position(top_left);
                    return;
                }

                if let Some((item, start_event)) = self.dragging.take() {
                    self.drag_ui_item(item, start_event, x, y, event, context);
                    return;
                }
            }
            _ => {}
        }

        let prior_ui_item = self.last_ui_item.clone();

        let ui_item = if matches!(self.current_mouse_capture, None | Some(MouseCapture::UI)) {
            let ui_item = self.resolve_ui_item(&event);

            match (self.last_ui_item.take(), &ui_item) {
                (Some(prior), Some(item)) => {
                    if prior != *item || !self.config.use_fancy_tab_bar {
                        self.leave_ui_item(&prior);
                        self.enter_ui_item(item);
                        context.invalidate();
                    }
                }
                (Some(prior), None) => {
                    self.leave_ui_item(&prior);
                    context.invalidate();
                }
                (None, Some(item)) => {
                    self.enter_ui_item(item);
                    context.invalidate();
                }
                (None, None) => {}
            }

            ui_item
        } else {
            None
        };
        let is_left_release = event.kind == WMEK::Release(MousePress::Left);

        if matches!(&event.kind, WMEK::Press(_)) && self.sidebar_search.is_some() {
            let on_search = matches!(
                &ui_item,
                Some(item) if item.item_type == UIItemType::SidebarSearch
            );
            if !on_search {
                self.sidebar_search = None;
                context.invalidate();
            }
        }

        // Clicking inside the docked input band focuses the strip and consumes
        // the click; clicking above it releases focus back to the terminal but
        // lets the terminal handle the click.
        if matches!(&event.kind, WMEK::Press(MousePress::Left)) {
            if let Some(band_top) = self.docked_input_band_top() {
                let y = event.coords.y as f32;
                if y >= band_top {
                    if !self.docked_input.focused {
                        self.docked_input.focused = true;
                        context.invalidate();
                    }
                    return;
                } else if self.docked_input.focused {
                    self.docked_input.focused = false;
                    context.invalidate();
                }
            }
        }

        if matches!(&event.kind, WMEK::Press(_)) && self.agent_copy_menu.is_some() {
            let on_copy_menu = matches!(
                &ui_item,
                Some(item)
                    if matches!(
                        item.item_type,
                        UIItemType::AgentToolbeltButton {
                            action: AgentToolbeltAction::CopyMenu,
                            ..
                        } | UIItemType::AgentCopyMenuItem { .. }
                    )
            );
            if !on_copy_menu {
                self.agent_copy_menu = None;
                context.invalidate();
            }
        }

        if matches!(&event.kind, WMEK::Press(_)) && self.agent_launch_menu.is_some() {
            let on_launch_menu = matches!(
                &ui_item,
                Some(item)
                    if matches!(
                        item.item_type,
                        UIItemType::SidebarAgentLaunchButton
                            | UIItemType::SidebarAgentMenuItem { .. }
                            | UIItemType::SidebarAgentMenuProjectRootToggle
                            | UIItemType::SidebarAgentMenuHerd
                            | UIItemType::SidebarAgentMenuTarget { .. }
                            | UIItemType::SidebarAgentMenuResume
                            | UIItemType::SidebarAgentMenuResumeSession { .. }
                            | UIItemType::SidebarAgentMenuRestoreLastWindow
                            | UIItemType::SidebarAgentSectionHeader
                            | UIItemType::SidebarAgentRow { .. }
                            | UIItemType::SidebarAgentRowChevron { .. }
                            | UIItemType::SidebarAgentAction { .. }
                    )
            );
            if !on_launch_menu {
                self.agent_launch_menu = None;
                context.invalidate();
            }
        }

        if matches!(&event.kind, WMEK::Press(_)) && self.new_tab_menu.is_some() {
            let on_new_tab_menu = matches!(
                &ui_item,
                Some(item)
                    if matches!(
                        item.item_type,
                        UIItemType::SidebarNewTabMenuButton
                            | UIItemType::SidebarNewTabMenuItem { .. }
                    )
            );
            if !on_new_tab_menu {
                self.new_tab_menu = None;
                context.invalidate();
            }
        }

        if matches!(&event.kind, WMEK::Press(_)) && self.close_tab_menu.is_some() {
            let on_close_tab_menu = matches!(
                &ui_item,
                Some(item)
                    if matches!(
                        item.item_type,
                        UIItemType::CloseTab(_)
                            | UIItemType::SidebarCloseTab(_)
                            | UIItemType::CloseTabMenuItem { .. }
                    )
            );
            if !on_close_tab_menu {
                self.close_tab_menu = None;
                context.invalidate();
            }
        }

        if matches!(&event.kind, WMEK::Press(_)) && self.ssh_launch_menu.is_some() {
            let on_ssh_menu = matches!(
                &ui_item,
                Some(item)
                    if matches!(
                        item.item_type,
                        UIItemType::SidebarSshLaunchButton
                            | UIItemType::SidebarSshMenuItem { .. }
                    )
            );
            if !on_ssh_menu {
                self.ssh_launch_menu = None;
                context.invalidate();
            }
        }

        if let Some(item) = ui_item.clone() {
            if capture_mouse {
                self.current_mouse_capture = Some(MouseCapture::UI);
            }
            self.mouse_event_ui_item(item, pane, y, event, context);
        } else if matches!(
            self.current_mouse_capture,
            None | Some(MouseCapture::TerminalPane(_))
        ) {
            self.mouse_event_terminal(
                pane,
                ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                event,
                context,
                capture_mouse,
            );
        }

        if prior_ui_item != ui_item {
            self.update_title_post_status();
        }
        if is_left_release {
            self.pressed_ui_item = None;
            context.invalidate();
        }
    }

    pub fn mouse_leave_impl(&mut self, context: &dyn WindowOps) {
        self.current_mouse_event = None;
        if self.sidebar_auto_hide_open && self.schedule_sidebar_auto_hide_close() {
            context.invalidate();
        }
        self.update_title();
        context.set_cursor(Some(MouseCursor::Arrow));
        context.invalidate();
    }

    fn drag_split(
        &mut self,
        mut item: UIItem,
        split: PositionedSplit,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        context: &dyn WindowOps,
    ) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let delta = match split.direction {
            SplitDirection::Horizontal => (x as isize).saturating_sub(split.left as isize),
            SplitDirection::Vertical => (y as isize).saturating_sub(split.top as isize),
        };

        if delta != 0 {
            tab.resize_split_by(split.index, delta);
            if let Some(split) = tab.iter_splits().into_iter().nth(split.index) {
                item.item_type = UIItemType::Split(split);
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    fn drag_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        _context: &dyn WindowOps,
    ) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let dims = pane.get_dimensions();
        let current_viewport = self.get_viewport(pane.pane_id());

        let tab_bar_height = if self.show_tab_bar && !self.sidebar_is_active() {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let y_offset = top_bar_height + border.top.get() as f32;

        let from_top = start_event.coords.y.saturating_sub(item.y as isize);
        let effective_thumb_top = event
            .coords
            .y
            .saturating_sub(y_offset as isize + from_top)
            .max(0) as usize;

        // Convert thumb top into a row index by reversing the math
        // in ScrollHit::thumb
        let row = ScrollHit::thumb_top_to_scroll_top(
            effective_thumb_top,
            &*pane,
            current_viewport,
            self.dimensions.pixel_height.saturating_sub(
                y_offset as usize + border.bottom.get() + bottom_bar_height as usize,
            ),
            self.min_scroll_bar_height() as usize,
        );
        self.set_viewport(pane.pane_id(), Some(row), dims);
        self.dragging.replace((item, start_event));
    }

    fn drag_ui_item(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match item.item_type {
            UIItemType::Split(split) => {
                self.drag_split(item, split, start_event, x, y, context);
            }
            UIItemType::ScrollThumb => {
                self.drag_scroll_thumb(item, start_event, event, context);
            }
            UIItemType::SidebarResize { start_width } => {
                self.drag_sidebar_resize(start_width, start_event, event, context);
            }
            UIItemType::SidebarScrollThumb => {
                self.drag_sidebar_scroll_thumb(item, start_event, event, context);
            }
            UIItemType::SidebarTab { tab_idx, .. } => {
                self.drag_sidebar_tab(item, tab_idx, start_event, event, context);
            }
            _ => {
                log::error!("drag not implemented for {:?}", item);
            }
        }
    }

    fn mouse_event_ui_item(
        &mut self,
        item: UIItem,
        pane: Arc<dyn Pane>,
        _y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let is_left_press = event.kind == WMEK::Press(MousePress::Left);
        self.last_ui_item.replace(item.clone());
        if matches!(event.kind, WMEK::VertWheel(_) | WMEK::HorzWheel(_))
            && matches!(
                item.item_type,
                UIItemType::SidebarAgentSectionHeader
                    | UIItemType::SidebarAgentRow { .. }
                    | UIItemType::SidebarAgentRowChevron { .. }
                    | UIItemType::SidebarAgentAction { .. }
            )
        {
            self.mouse_event_sidebar_agent_section_wheel(event, context);
            return;
        }
        // A press anywhere that is not an agent action closes an open row menu.
        if matches!(event.kind, WMEK::Press(_))
            && !matches!(item.item_type, UIItemType::SidebarAgentAction { .. })
        {
            self.agent_herd_state.borrow_mut().context_menu = None;
        }
        if is_left_press {
            self.pressed_ui_item.replace(item.item_type.clone());
            context.invalidate();
        }
        match item.item_type {
            UIItemType::TabBar(item) => {
                self.mouse_event_tab_bar(item, event, context);
            }
            UIItemType::AboveScrollThumb => {
                self.mouse_event_above_scroll_thumb(item, pane, event, context);
            }
            UIItemType::ScrollThumb => {
                self.mouse_event_scroll_thumb(item, pane, event, context);
            }
            UIItemType::BelowScrollThumb => {
                self.mouse_event_below_scroll_thumb(item, pane, event, context);
            }
            UIItemType::Split(split) => {
                self.mouse_event_split(item, split, event, context);
            }
            UIItemType::CloseTab(idx) => {
                self.mouse_event_close_tab(idx, CloseTabSource::TabBar, &item, event, context);
            }
            UIItemType::SidebarCloseTab(idx) => {
                self.mouse_event_close_tab(idx, CloseTabSource::Sidebar, &item, event, context);
            }
            UIItemType::CloseTabMenuItem { source, action } => {
                self.mouse_event_close_tab_menu_item(source, action, event, context);
            }
            UIItemType::SidebarTab { tab_idx, .. } => {
                self.mouse_event_sidebar_tab(item, tab_idx, event, context);
            }
            UIItemType::SidebarTabExpand { tab_idx } => {
                self.mouse_event_sidebar_tab_expand(tab_idx, event, context);
            }
            UIItemType::SidebarPaneRow { pane_id } => {
                self.mouse_event_sidebar_pane_row(pane_id, event, context);
            }
            UIItemType::SidebarPaneClose { pane_id } => {
                self.mouse_event_sidebar_pane_close(pane_id, event, context);
            }
            UIItemType::SidebarTabList => {
                self.mouse_event_sidebar_tab_list(event, context);
            }
            UIItemType::SidebarScrollTrack => {
                self.mouse_event_sidebar_scroll_track(item, event, context);
            }
            UIItemType::SidebarScrollThumb => {
                self.mouse_event_sidebar_scroll_thumb(item, event, context);
            }
            UIItemType::SidebarResize { .. } => {
                self.mouse_event_sidebar_resize(item, event, context);
            }
            UIItemType::SidebarSearch => {
                self.mouse_event_sidebar_search(event, context);
            }
            UIItemType::SidebarAutoHideToggle => {
                self.mouse_event_sidebar_autohide_toggle(event, context);
            }
            UIItemType::SidebarWorktreeButton => {
                self.mouse_event_sidebar_worktree_button(pane, event, context);
            }
            UIItemType::SidebarAgentLaunchButton => {
                self.mouse_event_sidebar_agent_launch_button(item, event, context);
            }
            UIItemType::SidebarAgentMenuItem { adapter_id } => {
                self.mouse_event_sidebar_agent_menu_item(&adapter_id, event, context);
            }
            UIItemType::SidebarAgentMenuProjectRootToggle => {
                self.mouse_event_sidebar_agent_menu_project_root_toggle(event, context);
            }
            UIItemType::SidebarAgentMenuHerd => {
                self.mouse_event_sidebar_agent_menu_herd(event, context);
            }
            UIItemType::SidebarAgentMenuTarget { adapter_id, target } => {
                self.mouse_event_sidebar_agent_menu_target(&adapter_id, target, event, context);
            }
            UIItemType::SidebarAgentMenuResume => {
                self.mouse_event_sidebar_agent_menu_resume(event, context);
            }
            UIItemType::SidebarAgentMenuResumeSession { index } => {
                self.mouse_event_sidebar_agent_menu_resume_session(index, event, context);
            }
            UIItemType::SidebarAgentMenuRestoreLastWindow => {
                self.mouse_event_sidebar_agent_menu_restore_last_window(event, context);
            }
            UIItemType::SidebarAgentSectionHeader => {
                self.mouse_event_sidebar_agent_section_header(event, context);
            }
            UIItemType::SidebarAgentRow { ref key } => {
                let key = key.clone();
                self.mouse_event_sidebar_agent_row(&key, event, context);
            }
            UIItemType::SidebarAgentRowChevron { ref key } => {
                let key = key.clone();
                self.mouse_event_sidebar_agent_row_chevron(&key, event, context);
            }
            UIItemType::SidebarAgentAction { ref key, action } => {
                let key = key.clone();
                self.mouse_event_sidebar_agent_action(&key, action, event, context);
            }
            UIItemType::SidebarNewTabMenuButton => {
                self.mouse_event_sidebar_new_tab_menu_button(item, event, context);
            }
            UIItemType::SidebarNewTabMenuItem { index } => {
                self.mouse_event_sidebar_new_tab_menu_item(index, event, context);
            }
            UIItemType::SidebarSshLaunchButton => {
                self.mouse_event_sidebar_ssh_launch_button(item, event, context);
            }
            UIItemType::SidebarSshMenuItem { domain_name } => {
                self.mouse_event_sidebar_ssh_menu_item(domain_name, event, context);
            }
            UIItemType::SidebarWaitingCounter => {
                self.mouse_event_sidebar_waiting_counter(event, context);
            }
            UIItemType::AgentToolbeltButton { pane_id, action } => {
                self.mouse_event_agent_toolbelt_button(pane_id, action, event, context);
            }
            UIItemType::AgentCopyMenuItem { pane_id, action } => {
                self.mouse_event_agent_copy_menu_item(pane_id, action, event, context);
            }
        }
    }

    fn finish_sidebar_resize(&mut self) {
        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            let dims = self.dimensions;
            self.apply_dimensions(&dims, None, &window);
        }
    }

    fn drag_sidebar_resize(
        &mut self,
        start_width: usize,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let min_width = self.sidebar_collapsed_width().max(140) as isize;
        let border = self.get_os_border();
        let max_width = (self.dimensions.pixel_width / 2).max(min_width as usize) as isize;
        let raw_width = match self.config.sidebar_position {
            config::SidebarPosition::Left => {
                event.coords.x.saturating_sub(border.left.get() as isize)
            }
            config::SidebarPosition::Right => (self.dimensions.pixel_width as isize)
                .saturating_sub(border.right.get() as isize)
                .saturating_sub(event.coords.x),
        };
        let width = raw_width.clamp(min_width, max_width).max(0) as usize;
        context.set_cursor(Some(MouseCursor::SizeLeftRight));
        if self.sidebar_drag_width == Some(width) {
            self.dragging.replace((
                UIItem {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    item_type: UIItemType::SidebarResize { start_width },
                },
                start_event,
            ));
            return;
        }
        self.sidebar_drag_width = Some(width);
        self.quad_generation += 1;
        *self.has_animation.borrow_mut() = Some(Instant::now());
        context.invalidate();
        self.dragging.replace((
            UIItem {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                item_type: UIItemType::SidebarResize { start_width },
            },
            start_event,
        ));
    }

    fn mouse_event_sidebar_resize(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(MouseCursor::SizeLeftRight));
        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    fn mouse_event_sidebar_tab_expand(
        &mut self,
        tab_idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            self.toggle_sidebar_tab_expanded(tab_idx);
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_pane_row(
        &mut self,
        pane_id: PaneId,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            let _ = self.activate_sidebar_pane(pane_id);
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_pane_close(
        &mut self,
        pane_id: PaneId,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            self.close_sidebar_pane(pane_id);
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_worktree_button(
        &mut self,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.open_file_browser(&pane);
            self.pressed_ui_item = None;
        }
        context.invalidate();
    }

    /// Left-click launches the default agent; right-click opens the picker.
    /// This mirrors the new-tab button, where right-click already means
    /// "show me the alternatives" (see `do_new_tab_button_click`).
    fn mouse_event_sidebar_agent_launch_button(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Release(MousePress::Left) => {
                self.pressed_ui_item = None;
                // A left-click while the menu is open acts as a dismiss, so
                // the button does not re-launch out from under the picker.
                if self.agent_launch_menu.take().is_none() {
                    if let Some(entry) = self.agent_launcher_default() {
                        let invert = event.modifiers.contains(KeyModifiers::ALT);
                        self.launch_agent(&entry, None, invert);
                    } else {
                        // No adapter configured: open the dropdown so the
                        // "Agent insight" and "Resume session" rows are
                        // still reachable.
                        self.agent_launch_menu = Some(AgentLaunchMenuState {
                            x: item.x,
                            y: item.y,
                            expanded: None,
                        });
                    }
                }
            }
            WMEK::Press(MousePress::Right) => {
                self.agent_launch_menu = match self.agent_launch_menu.take() {
                    Some(_) => None,
                    None => Some(AgentLaunchMenuState {
                        x: item.x,
                        y: item.y,
                        expanded: None,
                    }),
                };
            }
            _ => {}
        }
        context.invalidate();
    }

    /// Clicking an agent row expands (or collapses) its Split pane /
    /// Fullscreen / New tab submenu rather than launching immediately — the
    /// launcher button itself still launches on a plain click, so the fast
    /// path is unaffected.
    fn mouse_event_sidebar_agent_menu_item(
        &mut self,
        adapter_id: &str,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            if let Some(menu) = self.agent_launch_menu.as_mut() {
                let row = ExpandedMenuRow::Agent(adapter_id.to_string());
                menu.expanded = if menu.expanded.as_ref() == Some(&row) {
                    None
                } else {
                    Some(row)
                };
            }
        }
        context.invalidate();
    }

    /// A Split pane / Fullscreen / New tab row under an expanded agent:
    /// launch that one agent at the explicit target, ignoring both the
    /// configured `open_in` and the Alt-click inversion.
    fn mouse_event_sidebar_agent_menu_target(
        &mut self,
        adapter_id: &str,
        target: config::AgentLaunchTarget,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.agent_launch_menu = None;
            self.pressed_ui_item = None;
            self.launch_agent_by_id(adapter_id, Some(target), false);
        }
        context.invalidate();
    }

    /// Chevron beside the sidebar new-tab button: toggles the shell/domain
    /// picker. The `+` label itself is untouched and still spawns a tab.
    fn mouse_event_sidebar_new_tab_menu_button(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            self.new_tab_menu = match self.new_tab_menu.take() {
                Some(_) => None,
                None => Some(AgentLaunchMenuState {
                    x: item.x,
                    y: item.y,
                    expanded: None,
                }),
            };
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_new_tab_menu_item(
        &mut self,
        index: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.new_tab_menu = None;
            self.pressed_ui_item = None;
            self.spawn_new_tab_menu_entry(index);
        }
        context.invalidate();
    }

    /// Sidebar SSH quick-launch button: toggles the dropdown of pre-registered
    /// ssh_domains. Other open menus dismiss themselves through their own
    /// outside-click guards when this click is dispatched.
    fn mouse_event_sidebar_ssh_launch_button(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            // Don't open an empty dropdown — the button shouldn't have been
            // hit-testable in that case, but be defensive against a stale
            // render between the cache rebuild and the click.
            if self.ssh_quick_launch_entries().is_empty() {
                self.ssh_launch_menu = None;
            } else {
                self.ssh_launch_menu = match self.ssh_launch_menu.take() {
                    Some(_) => None,
                    None => Some(SshLaunchMenuState {
                        x: item.x,
                        y: item.y,
                    }),
                };
            }
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_ssh_menu_item(
        &mut self,
        domain_name: String,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.ssh_launch_menu = None;
            self.pressed_ui_item = None;
            self.spawn_ssh_quick_launch_entry(&domain_name);
        }
        context.invalidate();
    }

    /// Waiting-queue footer chip: a left-click jumps to the oldest waiting
    /// agent pane, exactly like `CycleWaitingAgent` does. Focusing it acts as
    /// the acknowledge that drops it from the queue.
    fn mouse_event_sidebar_waiting_counter(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            let oldest = self.waiting_queue().into_iter().next().map(|(id, _)| id);
            if let Some(target) = oldest {
                let _ = self.activate_pane_by_id(target);
            }
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_agent_menu_project_root_toggle(
        &mut self,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            // Deliberately leaves the menu open: the user can see the tick
            // change and then pick an agent in the same interaction.
            self.toggle_agent_launcher_project_root();
        }
        context.invalidate();
    }

    /// The "Resume session" row: expands the list of past sessions, and starts
    /// the scan that fills it.
    ///
    /// Like the project-root tick this leaves the menu open — expanding it is
    /// the whole point of the click. The scan is kicked here rather than in
    /// paint because paint must not touch the filesystem; it self-throttles, so
    /// toggling the row repeatedly costs nothing.
    fn mouse_event_sidebar_agent_menu_resume(
        &mut self,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            let expand = match self.agent_launch_menu.as_mut() {
                Some(menu) => {
                    let already = menu.expanded.as_ref() == Some(&ExpandedMenuRow::ResumeSessions);
                    menu.expanded = (!already).then_some(ExpandedMenuRow::ResumeSessions);
                    !already
                }
                None => false,
            };
            if expand {
                self.kick_agent_session_scan();
            }
        }
        context.invalidate();
    }

    /// One past session: resume it, honoring the configured launch placement the
    /// same way a fresh launch does.
    fn mouse_event_sidebar_agent_menu_resume_session(
        &mut self,
        index: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.agent_launch_menu = None;
            self.pressed_ui_item = None;
            self.resume_agent_session(index, None);
        }
        context.invalidate();
    }

    /// Reopen every agent session the previous run's last window had open.
    fn mouse_event_sidebar_agent_menu_restore_last_window(
        &mut self,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            // Acts on the tab layout, so the menu has no reason to stay open.
            self.agent_launch_menu = None;
            self.pressed_ui_item = None;
            self.restore_last_window_agent_sessions();
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_agent_menu_herd(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            // Unlike the project-root tick, this acts on the tab layout, so
            // the menu has no reason to stay open.
            self.agent_launch_menu = None;
        }
        context.invalidate();
    }

    fn drag_sidebar_tab(
        &mut self,
        mut item: UIItem,
        current_idx: usize,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let delta_y = event.coords.y.saturating_sub(start_event.coords.y).abs();
        if delta_y < 4 {
            self.dragging.replace((item, start_event));
            return;
        }

        let pointer_y = event.coords.y;
        let target_idx = self
            .ui_items
            .iter()
            .filter_map(|ui_item| match &ui_item.item_type {
                UIItemType::SidebarTab { tab_idx, .. } => {
                    let top = ui_item.y as isize - 4;
                    let bottom = (ui_item.y + ui_item.height) as isize + 4;
                    if pointer_y >= top && pointer_y <= bottom {
                        Some(*tab_idx)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .next();

        if let Some(target_idx) = target_idx {
            if target_idx != current_idx && self.move_tab(target_idx).is_ok() {
                item.item_type = UIItemType::SidebarTab {
                    tab_idx: target_idx,
                    active: true,
                };
                self.pressed_ui_item.replace(item.item_type.clone());
                self.last_ui_item.replace(item.clone());
                context.invalidate();
            }
        }

        context.set_cursor(Some(MouseCursor::Arrow));
        self.dragging.replace((item, start_event));
    }

    fn mouse_event_sidebar_tab(
        &mut self,
        item: UIItem,
        tab_idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.activate_tab(tab_idx as isize).ok();
                self.dragging.replace((item, event));
                context.invalidate();
            }
            WMEK::Press(MousePress::Middle) => {
                self.close_specific_tab(tab_idx, true);
            }
            WMEK::Press(MousePress::Right) => {
                self.show_tab_navigator();
            }
            WMEK::VertWheel(n) => {
                if self.scroll_sidebar_tabs(n.into()) {
                    context.invalidate();
                } else if self.config.mouse_wheel_scrolls_tabs {
                    self.activate_tab_relative(if n < 1 { 1 } else { -1 }, true)
                        .ok();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn mouse_event_sidebar_tab_list(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        if let WMEK::VertWheel(n) = event.kind {
            if self.scroll_sidebar_tabs(n.into()) {
                context.invalidate();
            }
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn drag_sidebar_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let drag_delta_y = event.coords.y.saturating_sub(start_event.coords.y);
        let thumb_top = item.y as isize + drag_delta_y;
        if self.scroll_sidebar_thumb_top_to(thumb_top) {
            context.invalidate();
        }
        self.dragging.replace((item, start_event));
    }

    fn mouse_event_sidebar_scroll_track(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                if self.scroll_sidebar_tabs_page_toward(event.coords.y) {
                    context.invalidate();
                }
            }
            WMEK::VertWheel(n) => {
                if self.scroll_sidebar_tabs(n.into()) {
                    context.invalidate();
                }
            }
            _ => {}
        }
        self.pressed_ui_item.replace(item.item_type);
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn mouse_event_sidebar_scroll_thumb(
        &mut self,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(item.item_type.clone());
                self.dragging.replace((item, event));
            }
            WMEK::VertWheel(n) => {
                if self.scroll_sidebar_tabs(n.into()) {
                    context.invalidate();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn mouse_event_sidebar_search(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        if event.kind == WMEK::Press(MousePress::Left) {
            self.sidebar_search.get_or_insert_with(Default::default);
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Text));
    }

    fn mouse_event_sidebar_autohide_toggle(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        let item_type = UIItemType::SidebarAutoHideToggle;
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(item_type);
                context.invalidate();
            }
            WMEK::Release(MousePress::Left) => {
                if self.pressed_ui_item.as_ref() == Some(&item_type) {
                    let new_val = !self.config.sidebar_auto_hide;

                    // Merge the new value into any existing per-window config
                    // overrides so every read site of self.config.sidebar_auto_hide
                    // picks it up on the next paint.
                    use wezterm_dynamic::Value;
                    let mut map: std::collections::BTreeMap<Value, Value> = match &self
                        .config_overrides
                    {
                        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                        _ => Default::default(),
                    };
                    map.insert(
                        Value::String("sidebar_auto_hide".to_string()),
                        Value::Bool(new_val),
                    );
                    self.config_overrides = Value::Object(map.into());

                    // Persist across restarts, then rebuild self.config from the
                    // overrides (config_was_reloaded also relayouts + invalidates).
                    crate::termwindow::tgz_ui_state::save_sidebar_auto_hide(new_val);
                    self.config_was_reloaded();

                    self.pressed_ui_item.take();
                    context.invalidate();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn mouse_event_agent_toolbelt_button(
        &mut self,
        pane_id: mux::pane::PaneId,
        action: AgentToolbeltAction,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let item_type = UIItemType::AgentToolbeltButton {
            pane_id,
            action: action.clone(),
        };
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(item_type);
                context.invalidate();
            }
            WMEK::Release(MousePress::Left) => {
                if self.pressed_ui_item.as_ref() == Some(&item_type) {
                    if let Some(pane) = Mux::get().get_pane(pane_id) {
                        match action {
                            AgentToolbeltAction::Interrupt => {
                                if let Err(err) =
                                    pane.key_down(KeyCode::Char('c'), KeyModifiers::CTRL)
                                {
                                    log::warn!("failed to send agent interrupt: {err:#}");
                                } else {
                                    wezterm_toast_notification::show(
                                        wezterm_toast_notification::ToastNotification {
                                            title: "Agent control".to_string(),
                                            message: "Sent Ctrl-C to the active agent pane"
                                                .to_string(),
                                            url: None,
                                            timeout: Some(Duration::from_millis(1800)),
                                        },
                                    );
                                }
                            }
                            AgentToolbeltAction::CopyMenu => {
                                self.agent_copy_menu = Some(AgentCopyMenuState {
                                    pane_id,
                                    x: event.coords.x.max(0) as usize,
                                    y: event.coords.y.max(0) as usize,
                                });
                            }
                            AgentToolbeltAction::Compose => {
                                let already_open = self
                                    .get_modal()
                                    .map(|m| {
                                        m.downcast_ref::<crate::termwindow::composer::Composer>()
                                            .is_some()
                                    })
                                    .unwrap_or(false);
                                if already_open {
                                    self.cancel_modal();
                                } else if let Some(modal) =
                                    crate::termwindow::composer::Composer::new(self, &pane)
                                {
                                    self.set_modal(std::rc::Rc::new(modal));
                                }
                            }
                            AgentToolbeltAction::DockInput => {
                                // The toolbelt only renders on agent panes, so
                                // this button is inherently agent-only.
                                self.toggle_docked_input_pane(pane.pane_id());
                            }
                            AgentToolbeltAction::Attach => {
                                self.agent_attach_pane(&pane);
                            }
                            AgentToolbeltAction::Resume => {
                                self.agent_resume_pane(&pane);
                            }
                            AgentToolbeltAction::OpenLogs => {
                                self.agent_open_logs_for_pane(&pane);
                            }
                        }
                    }
                    self.pressed_ui_item.take();
                    context.invalidate();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn mouse_event_agent_copy_menu_item(
        &mut self,
        pane_id: mux::pane::PaneId,
        action: AgentCopyAction,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let item_type = UIItemType::AgentCopyMenuItem {
            pane_id,
            action: action.clone(),
        };
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(item_type);
                context.invalidate();
            }
            WMEK::Release(MousePress::Left) => {
                if self.pressed_ui_item.as_ref() == Some(&item_type) {
                    if let Some(pane) = Mux::get().get_pane(pane_id) {
                        let payload = self.agent_pane_copy_payload(&pane, &action);
                        let message = self.agent_copy_toast_message(&action, &payload);
                        // Never overwrite the clipboard with nothing: an empty
                        // copy plus a success toast is how this bug hid.
                        if !payload.text.trim().is_empty() {
                            self.copy_to_clipboard(
                                ClipboardCopyDestination::Clipboard,
                                payload.text,
                            );
                        }
                        wezterm_toast_notification::show(
                            wezterm_toast_notification::ToastNotification {
                                title: "Agent copy".to_string(),
                                message,
                                url: None,
                                timeout: Some(Duration::from_millis(1800)),
                            },
                        );
                    }
                    self.agent_copy_menu = None;
                    self.pressed_ui_item.take();
                    context.invalidate();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_close_tab(
        &mut self,
        idx: usize,
        source: CloseTabSource,
        item: &UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let close_type = match source {
            CloseTabSource::TabBar => UIItemType::CloseTab(idx),
            CloseTabSource::Sidebar => UIItemType::SidebarCloseTab(idx),
        };
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(close_type);
                context.invalidate();
            }
            WMEK::Release(MousePress::Left) => {
                if self.pressed_ui_item.as_ref() == Some(&close_type) {
                    log::debug!("Should close tab {}", idx);
                    self.close_specific_tab(idx, true);
                }
            }
            WMEK::Press(MousePress::Right) => {
                if self.config.tab_close_context_menu {
                    self.close_tab_menu = Some(CloseTabMenuState {
                        x: item.x,
                        y: item.y + item.height,
                        source,
                        anchor_tab_idx: idx,
                    });
                    context.invalidate();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_close_tab_menu_item(
        &mut self,
        source: CloseTabSource,
        action: CloseTabMenuAction,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let item_type = UIItemType::CloseTabMenuItem { source, action };
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                self.pressed_ui_item.replace(item_type);
                context.invalidate();
            }
            WMEK::Release(MousePress::Left) => {
                if self.pressed_ui_item.as_ref() == Some(&item_type) {
                    let anchor_tab_idx = self
                        .close_tab_menu
                        .as_ref()
                        .map(|m| m.anchor_tab_idx)
                        .unwrap_or(0);
                    self.close_tab_menu = None;
                    match action {
                        CloseTabMenuAction::CloseAbove => self.close_tabs_above(anchor_tab_idx),
                        CloseTabMenuAction::CloseBelow => self.close_tabs_below(anchor_tab_idx),
                        CloseTabMenuAction::CloseAllOther => {
                            self.close_all_other_tabs(anchor_tab_idx)
                        }
                    }
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn do_new_tab_button_click(&mut self, button: MousePress) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };
        let action = match button {
            MousePress::Left => Some(KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain)),
            MousePress::Right => Some(KeyAssignment::ShowLauncher),
            MousePress::Middle => None,
        };

        async fn dispatch_new_tab_button(
            lua: Option<Rc<mlua::Lua>>,
            window: GuiWin,
            pane: MuxPane,
            button: MousePress,
            action: Option<KeyAssignment>,
        ) -> anyhow::Result<()> {
            let default_action = match lua {
                Some(lua) => {
                    let args = lua.pack_multi((
                        window.clone(),
                        pane,
                        format!("{button:?}"),
                        action.clone(),
                    ))?;
                    config::lua::emit_event(&lua, ("new-tab-button-click".to_string(), args))
                        .await
                        .map_err(|e| {
                            log::error!("while processing new-tab-button-click event: {:#}", e);
                            e
                        })?
                }
                None => true,
            };
            if let (true, Some(assignment)) = (default_action, action) {
                window.window.notify(TermWindowNotif::PerformAssignment {
                    pane_id: pane.0,
                    assignment,
                    tx: None,
                });
            }
            Ok(())
        }
        let window = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());
        promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
            dispatch_new_tab_button(lua, window, pane, button, action)
        }))
        .detach();
    }

    pub fn mouse_event_tab_bar(
        &mut self,
        item: TabBarItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.activate_tab(tab_idx as isize).ok();
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Left);
                }
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    let maximized = self
                        .window_state
                        .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                    if let Some(ref window) = self.window {
                        if self.config.window_decorations
                            == WindowDecorations::INTEGRATED_BUTTONS | WindowDecorations::RESIZE
                        {
                            if self.last_mouse_click.as_ref().map(|c| c.streak) == Some(2) {
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                        }
                    }
                    // Potentially starting a drag by the tab bar
                    if !maximized {
                        self.window_drag_position.replace(event.clone());
                    }
                    context.request_drag_move();
                }
                TabBarItem::WindowButton(button) => {
                    use window::IntegratedTitleButton as Button;
                    if let Some(ref window) = self.window {
                        match button {
                            Button::Hide => window.hide(),
                            Button::Maximize => {
                                let maximized = self
                                    .window_state
                                    .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                            Button::Close => self.close_requested(&window.clone()),
                        }
                    }
                }
            },
            WMEK::Press(MousePress::Middle) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.close_specific_tab(tab_idx, true);
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Middle);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Press(MousePress::Right) => match item {
                TabBarItem::Tab { .. } => {
                    self.show_tab_navigator();
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Right);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Move => match item {
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    context.set_window_drag_position(event.screen_coords);
                }
                TabBarItem::WindowButton(window::IntegratedTitleButton::Maximize) => {
                    let item = self.last_ui_item.clone().unwrap();
                    let bounds: ::window::ScreenRect = euclid::rect(
                        item.x as isize - (event.coords.x as isize - event.screen_coords.x),
                        item.y as isize - (event.coords.y as isize - event.screen_coords.y),
                        item.width as isize,
                        item.height as isize,
                    );
                    context.set_maximize_button_position(bounds);
                }
                TabBarItem::WindowButton(_)
                | TabBarItem::Tab { .. }
                | TabBarItem::NewTabButton { .. } => {}
            },
            WMEK::VertWheel(n) => {
                if self.config.mouse_wheel_scrolls_tabs {
                    self.activate_tab_relative(if n < 1 { 1 } else { -1 }, true)
                        .ok();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_above_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page up
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_sub(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_below_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page down
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_add(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_scroll_thumb(
        &mut self,
        item: UIItem,
        _pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            // Start a scroll drag
            // self.scroll_drag_start = Some(from_top);
            self.dragging = Some((item, event));
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_split(
        &mut self,
        item: UIItem,
        split: PositionedSplit,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(match &split.direction {
            SplitDirection::Horizontal => MouseCursor::SizeLeftRight,
            SplitDirection::Vertical => MouseCursor::SizeUpDown,
        }));

        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    fn mouse_event_terminal(
        &mut self,
        mut pane: Arc<dyn Pane>,
        position: ClickPosition,
        event: MouseEvent,
        context: &dyn WindowOps,
        capture_mouse: bool,
    ) {
        let mut is_click_to_focus_pane = false;

        let ClickPosition {
            mut column,
            mut row,
            mut x_pixel_offset,
            mut y_pixel_offset,
        } = position;

        let is_already_captured = matches!(
            self.current_mouse_capture,
            Some(MouseCapture::TerminalPane(_))
        );

        for pos in self.get_panes_to_render() {
            if !is_already_captured
                && row >= pos.top as i64
                && row <= (pos.top + pos.height) as i64
                && column >= pos.left
                && column <= pos.left + pos.width
            {
                if pane.pane_id() != pos.pane.pane_id() {
                    // We're over a pane that isn't active
                    match &event.kind {
                        WMEK::Press(_) => {
                            let mux = Mux::get();
                            mux.get_active_tab_for_window(self.mux_window_id)
                                .map(|tab| tab.set_active_idx(pos.index));

                            pane = Arc::clone(&pos.pane);
                            is_click_to_focus_pane = true;
                        }
                        WMEK::Move => {
                            if self.config.pane_focus_follows_mouse {
                                let mux = Mux::get();
                                mux.get_active_tab_for_window(self.mux_window_id)
                                    .map(|tab| tab.set_active_idx(pos.index));

                                pane = Arc::clone(&pos.pane);
                                context.invalidate();
                            }
                        }
                        WMEK::Release(_) | WMEK::HorzWheel(_) => {}
                        WMEK::VertWheel(_) => {
                            // Let wheel events route to the hovered pane,
                            // even if it doesn't have focus
                            pane = Arc::clone(&pos.pane);
                            context.invalidate();
                        }
                    }
                }
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64);
                break;
            } else if is_already_captured && pane.pane_id() == pos.pane.pane_id() {
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64).max(0);

                if position.column < pos.left {
                    x_pixel_offset -= self.render_metrics.cell_size.width
                        * (pos.left as isize - position.column as isize);
                }
                if position.row < pos.top as i64 {
                    y_pixel_offset -= self.render_metrics.cell_size.height
                        * (pos.top as isize - position.row as isize);
                }

                break;
            }
        }

        if capture_mouse {
            self.current_mouse_capture = Some(MouseCapture::TerminalPane(pane.pane_id()));
        }

        let is_focused = if let Some(focused) = self.focused.as_ref() {
            !self.config.swallow_mouse_click_on_window_focus
                || (focused.elapsed() > Duration::from_millis(200))
        } else {
            false
        };

        if self.focused.is_some() && !is_focused {
            if matches!(&event.kind, WMEK::Press(_))
                && self.config.swallow_mouse_click_on_window_focus
            {
                // Entering click to focus state
                self.is_click_to_focus_window = true;
                context.invalidate();
                log::trace!("enter click to focus");
                return;
            }
        }
        if self.is_click_to_focus_window && matches!(&event.kind, WMEK::Release(_)) {
            // Exiting click to focus state
            self.is_click_to_focus_window = false;
            context.invalidate();
            log::trace!("exit click to focus");
            return;
        }

        let allow_action = if self.is_click_to_focus_window || !is_focused {
            matches!(&event.kind, WMEK::VertWheel(_) | WMEK::HorzWheel(_))
        } else {
            true
        };

        log::trace!(
            "is_focused={} allow_action={} event={:?}",
            is_focused,
            allow_action,
            event
        );

        let dims = pane.get_dimensions();
        let stable_row = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            + row as StableRowIndex;

        self.pane_state(pane.pane_id())
            .mouse_terminal_coords
            .replace((
                ClickPosition {
                    column,
                    row,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                stable_row,
            ));

        pane.apply_hyperlinks(stable_row..stable_row + 1, &self.config.hyperlink_rules);

        struct FindCurrentLink {
            current: Option<Arc<Hyperlink>>,
            stable_row: StableRowIndex,
            column: usize,
        }

        impl WithPaneLines for FindCurrentLink {
            fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                if stable_top == self.stable_row {
                    if let Some(line) = lines.get(0) {
                        if let Some(cell) = line.get_cell(self.column) {
                            self.current = cell.attrs().hyperlink().cloned();
                        }
                    }
                }
            }
        }

        let mut find_link = FindCurrentLink {
            current: None,
            stable_row,
            column,
        };
        pane.with_lines_mut(stable_row..stable_row + 1, &mut find_link);
        let new_highlight = find_link.current;

        match (self.current_highlight.as_ref(), new_highlight) {
            (Some(old_link), Some(new_link)) if Arc::ptr_eq(&old_link, &new_link) => {
                // Unchanged
            }
            (None, None) => {
                // Unchanged
            }
            (_, rhs) => {
                // We're hovering over a different URL, so invalidate and repaint
                // so that we render the underline correctly
                self.current_highlight = rhs;
                context.invalidate();
            }
        };

        let outside_window = event.coords.x < 0
            || event.coords.x as usize > self.dimensions.pixel_width
            || event.coords.y < 0
            || event.coords.y as usize > self.dimensions.pixel_height;

        context.set_cursor(Some(if self.current_highlight.is_some() {
            // When hovering over a hyperlink, show an appropriate
            // mouse cursor to give the cue that it is clickable
            MouseCursor::Hand
        } else if pane.is_mouse_grabbed() || outside_window {
            MouseCursor::Arrow
        } else {
            MouseCursor::Text
        }));

        let event_trigger_type = match &event.kind {
            WMEK::Press(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Down {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Release(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Up {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Move => {
                if !self.current_mouse_buttons.is_empty() {
                    if let Some(LastMouseClick { streak, button, .. }) =
                        self.last_mouse_click.as_ref()
                    {
                        if Some(*button)
                            == self.current_mouse_buttons.last().map(mouse_press_to_tmb)
                        {
                            Some(MouseEventTrigger::Drag {
                                streak: *streak,
                                button: *button,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            WMEK::VertWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelUp(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelDown(-amount as usize),
                },
            }),
            WMEK::HorzWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelLeft(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelRight(-amount as usize),
                },
            }),
        };

        if allow_action {
            if let Some(mut event_trigger_type) = event_trigger_type {
                self.current_event = Some(event_trigger_type.to_dynamic());
                let mut modifiers = event.modifiers;

                // Since we use shift to force assessing the mouse bindings, pretend
                // that shift is not one of the mods when the mouse is grabbed.
                let mut mouse_reporting = pane.is_mouse_grabbed();
                if mouse_reporting {
                    if modifiers.contains(self.config.bypass_mouse_reporting_modifiers) {
                        modifiers.remove(self.config.bypass_mouse_reporting_modifiers);
                        mouse_reporting = false;
                    }
                }

                if mouse_reporting {
                    // If they were scrolled back prior to launching an
                    // application that captures the mouse, then mouse based
                    // scrolling assignments won't have any effect.
                    // Ensure that we scroll to the bottom if they try to
                    // use the mouse so that things are less surprising
                    self.scroll_to_bottom(&pane);
                }

                // normalize delta and streak to make mouse assignment
                // easier to wrangle
                match event_trigger_type {
                    MouseEventTrigger::Down {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Up {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Drag {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    } => {
                        *streak = 1;
                        *delta = 1;
                    }
                    _ => {}
                };

                let mouse_mods = config::MouseEventTriggerMods {
                    mods: modifiers,
                    mouse_reporting,
                    alt_screen: if pane.is_alt_screen_active() {
                        MouseEventAltScreen::True
                    } else {
                        MouseEventAltScreen::False
                    },
                };

                if let Some(action) = self.input_map.lookup_mouse(event_trigger_type, mouse_mods) {
                    self.perform_key_assignment(&pane, &action).ok();
                    return;
                }
            }
        }

        let mouse_event = wezterm_term::MouseEvent {
            kind: match event.kind {
                WMEK::Move => TMEK::Move,
                WMEK::VertWheel(_) | WMEK::HorzWheel(_) | WMEK::Press(_) => TMEK::Press,
                WMEK::Release(_) => TMEK::Release,
            },
            button: match event.kind {
                WMEK::Release(ref press) | WMEK::Press(ref press) => mouse_press_to_tmb(press),
                WMEK::Move => {
                    if event.mouse_buttons == WMB::LEFT {
                        TMB::Left
                    } else if event.mouse_buttons == WMB::RIGHT {
                        TMB::Right
                    } else if event.mouse_buttons == WMB::MIDDLE {
                        TMB::Middle
                    } else {
                        TMB::None
                    }
                }
                WMEK::VertWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelUp(amount as usize)
                    } else {
                        TMB::WheelDown((-amount) as usize)
                    }
                }
                WMEK::HorzWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelLeft(amount as usize)
                    } else {
                        TMB::WheelRight((-amount) as usize)
                    }
                }
            },
            x: column,
            y: row,
            x_pixel_offset,
            y_pixel_offset,
            modifiers: event.modifiers,
        };

        if allow_action
            && !(self.config.swallow_mouse_click_on_pane_focus && is_click_to_focus_pane)
        {
            pane.mouse_event(mouse_event).ok();
        }

        match event.kind {
            WMEK::Move => {}
            _ => {
                context.invalidate();
            }
        }
    }

    fn mouse_event_sidebar_agent_section_header(
        &mut self,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Right) {
            let view = {
                let mut state = self.agent_herd_state.borrow_mut();
                state.view = state.view.toggled();
                state.view
            };
            crate::termwindow::tgz_ui_state::save_agent_section_view(view);
            context.invalidate();
            return;
        }
        if event.kind == WMEK::Release(MousePress::Left) {
            let collapsed = {
                let mut state = self.agent_herd_state.borrow_mut();
                state.collapsed = !state.collapsed;
                state.collapsed
            };
            crate::termwindow::tgz_ui_state::save_agent_section_collapsed(collapsed);
            context.invalidate();
        }
    }

    /// Scroll the agent list when it is taller than the section. Wheel notches
    /// move one row; the per-frame `visible_rows` and `agents.len()` bound it.
    fn mouse_event_sidebar_agent_section_wheel(
        &mut self,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let amount = match event.kind {
            WMEK::VertWheel(n) => n,
            WMEK::HorzWheel(n) => n,
            _ => return,
        };
        // `amount` is the lines-per-notch the OS reported; sign is wheel
        // direction. Delta in rows = the notch count, one row per notch.
        let delta = if amount > 0 { 1 } else { -1 };
        let clamped = {
            let state = self.agent_herd_state.borrow();
            let max = state.agents.len().saturating_sub(state.visible_rows);
            state.scroll_offset.saturating_add_signed(delta).min(max)
        };
        let changed = {
            let mut state = self.agent_herd_state.borrow_mut();
            let changed = state.scroll_offset != clamped;
            state.scroll_offset = clamped;
            changed
        };
        if changed {
            context.invalidate();
        }
    }

    /// Clicking a row brings that agent on screen. Expanding is the chevron's
    /// job — the row used to only toggle its own detail, which meant there was
    /// no way to reach the agent from the sidebar at all.
    fn mouse_event_sidebar_agent_row(
        &mut self,
        key: &AgentKey,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        // Right-click opens/closes this row's action menu.
        if event.kind == WMEK::Release(MousePress::Right) {
            let mut state = self.agent_herd_state.borrow_mut();
            state.context_menu = if state.context_menu.as_ref() == Some(key) {
                None
            } else {
                Some(key.clone())
            };
            context.invalidate();
            return;
        }
        if event.kind != WMEK::Release(MousePress::Left) {
            return;
        }
        self.pressed_ui_item = None;
        // A left click anywhere while a row menu is open closes it (the menu's
        // own actions are separate UIItems that win the hit-test).
        self.agent_herd_state.borrow_mut().context_menu = None;
        match self.herd_agent_by_key(key) {
            Some(agent) if !agent.is_detached() => {
                self.focus_herd_agent(&agent);
            }
            // Detached: there is no pane to show. Say so in the section header
            // (deduped by the 5s feedback window) instead of an OS toast on
            // every click, and expand so the Resume button is visible.
            Some(_) => {
                self.toggle_herd_row_expansion(key);
                self.agent_herd_state.borrow_mut().feedback = Some((
                    "not attached here — use Resume".to_string(),
                    std::time::Instant::now(),
                ));
            }
            None => {}
        }
        context.invalidate();
    }

    fn mouse_event_sidebar_agent_row_chevron(
        &mut self,
        key: &AgentKey,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind == WMEK::Release(MousePress::Left) {
            self.pressed_ui_item = None;
            self.toggle_herd_row_expansion(key);
            context.invalidate();
        }
    }

    pub(crate) fn toggle_herd_row_expansion(&mut self, key: &AgentKey) {
        let mut state = self.agent_herd_state.borrow_mut();
        state.expanded = if state.expanded.as_ref() == Some(key) {
            None
        } else {
            Some(key.clone())
        };
        let expanded_now = state.expanded.is_some();
        drop(state);
        // An expanded row is taller than a plain one; keep it on screen.
        if expanded_now {
            self.scroll_agent_selection_into_view(key);
        }
    }

    /// Look up a row by identity: the list is rebuilt every paint, so the
    /// agent that was under the cursor may have moved by now.
    pub(crate) fn herd_agent_by_key(&self, key: &AgentKey) -> Option<crate::agent_herd::HerdAgent> {
        self.agent_herd_state
            .borrow()
            .agents
            .iter()
            .find(|agent| &agent.key() == key)
            .cloned()
    }

    pub(crate) fn focus_herd_agent(&mut self, agent: &crate::agent_herd::HerdAgent) {
        let Some(pane_id) = agent.pane_id else {
            return;
        };
        // The sidebar path also activates the containing tab and refreshes the
        // title; fall back to the mux for a pane in another window.
        if self.activate_sidebar_pane(pane_id) {
            return;
        }
        if let Err(err) = Mux::get().focus_pane_and_containing_tab(pane_id) {
            log::warn!("could not focus agent pane {pane_id}: {err:#}");
        }
    }

    fn mouse_event_sidebar_agent_action(
        &mut self,
        key: &AgentKey,
        action: AgentRowAction,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if event.kind != WMEK::Release(MousePress::Left) {
            return;
        }
        self.pressed_ui_item = None;
        let Some(agent) = self.herd_agent_by_key(key) else {
            return;
        };

        match action {
            AgentRowAction::Log => self.show_agent_log(agent.clone()),
            AgentRowAction::CopyId => {
                if let Some(id) = agent.session_id.as_deref() {
                    self.copy_to_clipboard(ClipboardCopyDestination::Clipboard, id.to_string());
                    self.agent_herd_state.borrow_mut().feedback =
                        Some(("session id copied".to_string(), Instant::now()));
                }
            }
            AgentRowAction::Transcript => {
                if let Some(cwd) = agent.cwd.as_ref() {
                    let _ = open_url(&format!("file://{}", cwd.display()));
                }
            }
            AgentRowAction::Focus => self.focus_herd_agent(&agent),
            AgentRowAction::Resume => {
                let session_id = agent.session_id.clone().unwrap_or_default();
                let cwd = agent.cwd.clone();
                match (session_id.is_empty(), cwd) {
                    (false, Some(cwd)) => {
                        self.resume_agent_session_by_id(&agent.provider, &session_id, cwd, None);
                    }
                    // Without both an id and a directory the resume command
                    // cannot be built; saying so beats spawning something wrong.
                    _ => {
                        wezterm_toast_notification::show(
                            wezterm_toast_notification::ToastNotification {
                                title: "Agent resume".to_string(),
                                message: "This agent did not report a session id and directory"
                                    .to_string(),
                                url: None,
                                timeout: Some(std::time::Duration::from_millis(2600)),
                            },
                        );
                    }
                }
            }
            AgentRowAction::Attach | AgentRowAction::Logs | AgentRowAction::Stop => {
                let Some(pane) = agent.pane_id.and_then(|id| Mux::get().get_pane(id)) else {
                    wezterm_toast_notification::show(
                        wezterm_toast_notification::ToastNotification {
                            title: "Agent".to_string(),
                            message: format!(
                                "{} needs a live pane; this agent is detached",
                                action.label()
                            ),
                            url: None,
                            timeout: Some(std::time::Duration::from_millis(2600)),
                        },
                    );
                    return;
                };
                match action {
                    AgentRowAction::Attach => self.agent_attach_pane(&pane),
                    AgentRowAction::Logs => self.agent_open_logs_for_pane(&pane),
                    AgentRowAction::Stop => {
                        pane.key_down(KeyCode::Char('c'), KeyModifiers::CTRL).ok();
                    }
                    _ => unreachable!("outer match limits this to attach/logs/stop"),
                }
            }
        }
        self.agent_herd_state.borrow_mut().context_menu = None;
        context.invalidate();
    }
}

fn mouse_press_to_tmb(press: &MousePress) -> TMB {
    match press {
        MousePress::Left => TMB::Left,
        MousePress::Right => TMB::Right,
        MousePress::Middle => TMB::Middle,
    }
}
