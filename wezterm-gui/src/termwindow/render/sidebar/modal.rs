//! The two things that float over the whole window: the tab palette (what
//! `ShowTabNavigator` opens) and the "close these?" dialog.
//!
//! Both are painted with the menus, after everything else, over a dimmed
//! backdrop that swallows clicks, and both take the keyboard while open.
//! A child of `sidebar` so it can use the sidebar's palette and row data.

use super::*;
use crate::termwindow::tgz_ui_text::{Align, UiFont};
use crate::termwindow::{ChromeDialog, TabPalette};
use ::window::{CursorIcon, Modifiers, MouseEvent, MouseEventKind as WMEK};
use mux::pane::CloseReason;

/// What a tab's status mark says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TabMark {
    Running,
    Waiting,
    Idle,
    /// No agent in the tab.
    Plain,
}

/// One tab as the palette and the dialog list it.
#[derive(Debug, Clone)]
pub(crate) struct TabEntry {
    pub tab_idx: usize,
    pub active: bool,
    pub line1: String,
    pub line2: String,
    pub mark: TabMark,
}

/// The entries whose text contains `query`, case-insensitively.
pub(crate) fn filter_tab_entries(entries: Vec<TabEntry>, query: &str) -> Vec<TabEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter(|entry| {
            entry.line1.to_lowercase().contains(&query)
                || entry.line2.to_lowercase().contains(&query)
        })
        .collect()
}

const PALETTE_MAX_ROWS: usize = 10;

impl crate::TermWindow {
    /// Every tab of this window with the two lines its sidebar row shows.
    pub(crate) fn tab_entries(&self) -> Vec<TabEntry> {
        self.tab_bar
            .items()
            .iter()
            .filter_map(|entry| {
                let TabBarItem::Tab { tab_idx, active } = entry.item else {
                    return None;
                };
                let (title, metadata) = self.sidebar_tab_labels(tab_idx, &entry.title);
                let (line1, line2, mark) = match self.sidebar_agent_row_for_tab_idx(tab_idx) {
                    Some((pane_id, state)) => {
                        let (name, _) = self.sidebar_herd_tab_info(pane_id, state.kind.label());
                        let mut parts = vec![state.kind.label().to_string()];
                        if !matches!(state.status, AgentStatus::Unknown) {
                            parts.push(state.status.label().to_string());
                        }
                        let mark = match state.status {
                            AgentStatus::Running | AgentStatus::Streaming => TabMark::Running,
                            AgentStatus::WaitingForInput => TabMark::Waiting,
                            _ => TabMark::Idle,
                        };
                        (name.unwrap_or(title), parts.join(" \u{b7} "), mark)
                    }
                    None => (title, metadata.join(" \u{b7} "), TabMark::Plain),
                };
                Some(TabEntry {
                    tab_idx,
                    active,
                    line1,
                    line2,
                    mark,
                })
            })
            .collect()
    }

    pub(crate) fn open_tab_palette(&mut self) {
        let selected = self
            .tab_entries()
            .iter()
            .position(|entry| entry.active)
            .unwrap_or(0);
        self.tab_palette = Some(TabPalette {
            query: String::new(),
            selected,
        });
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn modal_backdrop(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        let width = self.dimensions.pixel_width as f32;
        let height = self.dimensions.pixel_height as f32;
        self.filled_rectangle(
            layers,
            0,
            euclid::rect(0., 0., width, height),
            LinearRgba::with_components(0., 0., 0., 0.38),
        )?;
        self.ui_items.push(UIItem {
            x: 0,
            y: 0,
            width: width as usize,
            height: height as usize,
            item_type: UIItemType::ModalBackdrop,
        });
        Ok(())
    }

    pub(crate) fn paint_tab_palette(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(palette) = self.tab_palette.as_ref() else {
            return Ok(());
        };
        let query = palette.query.clone();
        let entries = filter_tab_entries(self.tab_entries(), &query);
        let selected = palette.selected.min(entries.len().saturating_sub(1));
        if let Some(palette) = self.tab_palette.as_mut() {
            palette.selected = selected;
        }
        self.modal_backdrop(layers)?;

        let sb = self.sidebar_palette();
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let window_w = self.dimensions.pixel_width as f32;
        let window_h = self.dimensions.pixel_height as f32;
        let pad = 8. * dpi_scale;
        let line = self.ui_line_height(UiFont::Regular);
        let row_h = (line * 2.1).ceil();
        let first = selected.saturating_sub(PALETTE_MAX_ROWS - 1);
        let shown = entries.len().saturating_sub(first).min(PALETTE_MAX_ROWS);
        let box_w = (560. * dpi_scale).min(window_w - 32. * dpi_scale).max(120.);
        let box_h = pad * 2. + row_h * (shown.max(1) + 1) as f32 + line * 1.6;
        let box_x = ((window_w - box_w) * 0.5).round();
        let box_y = (window_h * 0.16)
            .min(window_h - box_h - pad)
            .max(pad)
            .round();
        let fill = lerp_rgba(sb.surface, sb.text_active, 0.07);
        self.sidebar_bordered_fill(
            layers,
            0,
            euclid::rect(box_x, box_y, box_w, box_h),
            MENU_RADIUS * dpi_scale,
            dpi_scale.max(1.),
            fill,
            sb.menu_border,
        )?;

        // The filter line.
        let text_x = box_x + pad * 2.;
        let text_w = box_w - pad * 4.;
        let mut y = box_y + pad;
        let count = format!("{} of {} tabs", entries.len(), self.tab_entries().len());
        let count_w = self.ui_text_width(UiFont::Small, &count);
        let typed = if query.is_empty() {
            "Go to tab\u{2026}".to_string()
        } else {
            format!("{query}|")
        };
        self.paint_ui_text(
            layers,
            UiFont::Regular,
            &typed,
            text_x,
            y + (row_h - line) * 0.5,
            (text_w - count_w - pad).max(0.),
            if query.is_empty() {
                sb.text_meta
            } else {
                sb.text_active
            },
            Align::Left,
        )?;
        let small = self.ui_line_height(UiFont::Small);
        self.paint_ui_text(
            layers,
            UiFont::Small,
            &count,
            text_x,
            y + (row_h - small) * 0.5,
            text_w,
            sb.text_meta,
            Align::Right,
        )?;
        y += row_h;
        self.filled_rectangle(
            layers,
            0,
            euclid::rect(box_x + pad, y - 1., box_w - pad * 2., dpi_scale.max(1.)),
            sb.divider,
        )?;

        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let dot = (8. * dpi_scale).round();
        for (idx, entry) in entries.iter().enumerate().skip(first).take(shown) {
            let item_type = UIItemType::TabPaletteRow {
                tab_idx: entry.tab_idx,
            };
            let lit = idx == selected || hovered.as_ref() == Some(&item_type);
            if lit {
                self.sidebar_rounded_fill(
                    layers,
                    0,
                    euclid::rect(box_x + pad, y, box_w - pad * 2., row_h - 2.),
                    MENU_ROW_RADIUS * dpi_scale,
                    if idx == selected {
                        sb.active_fill
                    } else {
                        sb.hover_fill
                    },
                )?;
            }
            let mark = match entry.mark {
                TabMark::Running => Some(sb.accent()),
                TabMark::Waiting => Some(sb.attention),
                TabMark::Idle => Some(sb.text_meta),
                TabMark::Plain => None,
            };
            if let Some(color) = mark {
                self.sidebar_pill_fill(
                    layers,
                    1,
                    euclid::rect(text_x, y + (row_h - dot) * 0.5, dot, dot),
                    dot * 0.5,
                    color,
                )?;
            }
            let name_x = text_x + dot + pad;
            let name_w = text_x + text_w - name_x;
            let detail_w = self
                .ui_text_width(UiFont::Small, &entry.line2)
                .min(name_w * 0.5);
            self.paint_ui_text(
                layers,
                UiFont::Medium,
                &entry.line1,
                name_x,
                y + (row_h - self.ui_line_height(UiFont::Medium)) * 0.5,
                (name_w - detail_w - pad).max(0.),
                if lit { sb.text_active } else { sb.text_idle },
                Align::Left,
            )?;
            self.paint_ui_text(
                layers,
                UiFont::Small,
                &entry.line2,
                name_x,
                y + (row_h - small) * 0.5,
                name_w,
                if entry.mark == TabMark::Waiting {
                    sb.attention
                } else {
                    sb.text_meta
                },
                Align::Right,
            )?;
            self.ui_items.push(UIItem {
                x: (box_x + pad) as usize,
                y: y as usize,
                width: (box_w - pad * 2.) as usize,
                height: row_h as usize,
                item_type,
            });
            y += row_h;
        }
        if entries.is_empty() {
            self.paint_ui_text(
                layers,
                UiFont::Regular,
                "No tab matches",
                text_x,
                y + (row_h - line) * 0.5,
                text_w,
                sb.text_meta,
                Align::Left,
            )?;
            y += row_h;
        }
        self.paint_ui_text(
            layers,
            UiFont::Small,
            "Enter switch  \u{b7}  \u{2191}\u{2193} move  \u{b7}  Esc close",
            text_x,
            y + (line * 1.6 - small) * 0.5,
            text_w,
            sb.text_meta,
            Align::Left,
        )?;
        Ok(())
    }

    /// Keys while the tab palette is open. It takes all of them.
    pub(crate) fn tab_palette_key(
        &mut self,
        key: &::termwiz::input::KeyCode,
        modifiers: Modifiers,
    ) {
        use ::termwiz::input::KeyCode as KC;
        let count = self
            .tab_palette
            .as_ref()
            .map(|palette| filter_tab_entries(self.tab_entries(), &palette.query).len())
            .unwrap_or(0);
        match key {
            KC::Escape => self.tab_palette = None,
            KC::Enter => {
                let target = self.tab_palette.take().and_then(|palette| {
                    filter_tab_entries(self.tab_entries(), &palette.query)
                        .get(palette.selected)
                        .map(|entry| entry.tab_idx)
                });
                if let Some(tab_idx) = target {
                    self.activate_tab(tab_idx as isize).ok();
                }
            }
            KC::UpArrow | KC::DownArrow | KC::Tab => {
                if let Some(palette) = self.tab_palette.as_mut() {
                    let up = *key == KC::UpArrow
                        || (*key == KC::Tab && modifiers.contains(Modifiers::SHIFT));
                    if count > 0 {
                        palette.selected = if up {
                            (palette.selected + count - 1) % count
                        } else {
                            (palette.selected + 1) % count
                        };
                    }
                }
            }
            KC::Backspace => {
                if let Some(palette) = self.tab_palette.as_mut() {
                    palette.query.pop();
                    palette.selected = 0;
                }
            }
            KC::Char(c)
                if !modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
                    && !c.is_control() =>
            {
                if let Some(palette) = self.tab_palette.as_mut() {
                    palette.query.push(*c);
                    palette.selected = 0;
                }
            }
            _ => {}
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Asks before closing something that is still at work. `lines` name
    /// what would be stopped; `on_confirm` runs if the answer is yes.
    pub(crate) fn confirm_close(
        &mut self,
        title: String,
        body: String,
        lines: Vec<(String, String)>,
        confirm_label: String,
        on_confirm: impl FnOnce(&mut crate::TermWindow) + Send + Sync + 'static,
    ) {
        self.tab_palette = None;
        self.chrome_dialog = Some(ChromeDialog {
            title,
            body,
            lines,
            confirm_label,
            on_confirm: Some(Box::new(on_confirm)),
        });
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// The `(line 1, line 2)` of the tabs in `tab_ids` that are still
    /// running something, for the close dialog.
    pub(crate) fn busy_tab_lines(&self, tab_ids: &[mux::tab::TabId]) -> Vec<(String, String)> {
        let ids = self.mux_tab_ids();
        let entries = self.tab_entries();
        let mux = Mux::get();
        tab_ids
            .iter()
            .filter(|tab_id| {
                mux.get_tab(**tab_id)
                    .is_some_and(|tab| !tab.can_close_without_prompting(CloseReason::Tab))
            })
            .filter_map(|tab_id| {
                let idx = ids.iter().position(|id| id == tab_id)?;
                let entry = entries.iter().find(|entry| entry.tab_idx == idx)?;
                Some((entry.line1.clone(), entry.line2.clone()))
            })
            .collect()
    }

    pub(crate) fn paint_chrome_dialog(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(dialog) = self.chrome_dialog.as_ref() else {
            return Ok(());
        };
        let (title, body, lines, confirm_label) = (
            dialog.title.clone(),
            dialog.body.clone(),
            dialog.lines.clone(),
            dialog.confirm_label.clone(),
        );
        self.modal_backdrop(layers)?;

        let sb = self.sidebar_palette();
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let window_w = self.dimensions.pixel_width as f32;
        let window_h = self.dimensions.pixel_height as f32;
        let pad = 16. * dpi_scale;
        let line = self.ui_line_height(UiFont::Regular);
        let small = self.ui_line_height(UiFont::Small);
        let title_h = self.ui_line_height(UiFont::Medium);
        let row_h = (line * 1.5).ceil();
        let button_h = (line * 2.2).ceil();
        let shown = lines.len().min(6);
        let list_h = if shown == 0 {
            0.
        } else {
            row_h * shown as f32 + pad
        };
        let box_w = (440. * dpi_scale).min(window_w - 32. * dpi_scale).max(160.);
        let box_h = pad * 2. + title_h + pad * 0.5 + line + pad + list_h + button_h;
        let box_x = ((window_w - box_w) * 0.5).round();
        let box_y = ((window_h - box_h) * 0.38).max(pad).round();
        let fill = lerp_rgba(sb.surface, sb.text_active, 0.07);
        self.sidebar_bordered_fill(
            layers,
            0,
            euclid::rect(box_x, box_y, box_w, box_h),
            MENU_RADIUS * dpi_scale * 1.5,
            dpi_scale.max(1.),
            fill,
            sb.menu_border,
        )?;
        let text_x = box_x + pad;
        let text_w = box_w - pad * 2.;
        let mut y = box_y + pad;
        self.paint_ui_text(
            layers,
            UiFont::Medium,
            &title,
            text_x,
            y,
            text_w,
            sb.text_active,
            Align::Left,
        )?;
        y += title_h + pad * 0.5;
        self.paint_ui_text(
            layers,
            UiFont::Regular,
            &body,
            text_x,
            y,
            text_w,
            sb.text_idle,
            Align::Left,
        )?;
        y += line + pad;
        if shown > 0 {
            self.sidebar_rounded_fill(
                layers,
                0,
                euclid::rect(
                    text_x,
                    y - pad * 0.4,
                    text_w,
                    row_h * shown as f32 + pad * 0.8,
                ),
                MENU_RADIUS * dpi_scale,
                sb.surface,
            )?;
            let dot = (8. * dpi_scale).round();
            for (name, detail) in lines.iter().take(shown) {
                let inner_x = text_x + pad * 0.75;
                let inner_w = text_w - pad * 1.5;
                self.sidebar_pill_fill(
                    layers,
                    1,
                    euclid::rect(inner_x, y + (row_h - dot) * 0.5, dot, dot),
                    dot * 0.5,
                    sb.accent(),
                )?;
                let name_x = inner_x + dot + pad * 0.5;
                let name_w = inner_x + inner_w - name_x;
                let detail_w = self.ui_text_width(UiFont::Small, detail).min(name_w * 0.5);
                self.paint_ui_text(
                    layers,
                    UiFont::Regular,
                    name,
                    name_x,
                    y + (row_h - line) * 0.5,
                    (name_w - detail_w - pad * 0.5).max(0.),
                    sb.text_active,
                    Align::Left,
                )?;
                self.paint_ui_text(
                    layers,
                    UiFont::Small,
                    detail,
                    name_x,
                    y + (row_h - small) * 0.5,
                    name_w,
                    sb.text_meta,
                    Align::Right,
                )?;
                y += row_h;
            }
            y += pad;
        }

        // Buttons, right-aligned: the safe one first, the named action last.
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        let keep = "Keep open";
        let button_pad = 16. * dpi_scale;
        let confirm_w = self.ui_text_width(UiFont::Medium, &confirm_label) + button_pad * 2.;
        let keep_w = self.ui_text_width(UiFont::Medium, keep) + button_pad * 2.;
        let confirm_x = box_x + box_w - pad - confirm_w;
        let keep_x = confirm_x - pad * 0.5 - keep_w;
        let medium = self.ui_line_height(UiFont::Medium);
        for (label, x, w, confirm) in [
            (keep, keep_x, keep_w, false),
            (confirm_label.as_str(), confirm_x, confirm_w, true),
        ] {
            let item_type = UIItemType::DialogButton { confirm };
            let lit = hovered.as_ref() == Some(&item_type);
            let base = if confirm {
                sb.attention
            } else {
                lerp_rgba(sb.surface, sb.text_active, 0.16)
            };
            let button_fill = if lit {
                lerp_rgba(base, sb.text_active, 0.16)
            } else {
                base
            };
            self.sidebar_rounded_fill(
                layers,
                0,
                euclid::rect(x, y, w, button_h),
                MENU_RADIUS * dpi_scale,
                button_fill,
            )?;
            self.paint_ui_text(
                layers,
                UiFont::Medium,
                label,
                x,
                y + (button_h - medium) * 0.5,
                w,
                contrast_label_color(button_fill, sb.text_active),
                Align::Center,
            )?;
            self.ui_items.push(UIItem {
                x: x as usize,
                y: y as usize,
                width: w as usize,
                height: button_h as usize,
                item_type,
            });
        }
        Ok(())
    }

    /// Answers the open dialog.
    pub(crate) fn answer_chrome_dialog(&mut self, confirm: bool) {
        let Some(mut dialog) = self.chrome_dialog.take() else {
            return;
        };
        if confirm {
            if let Some(run) = dialog.on_confirm.take() {
                run(self);
            }
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Keys while the dialog is open. Enter and Esc keep things open: the
    /// closing answer is never the one a stray key gives.
    pub(crate) fn chrome_dialog_key(&mut self, key: &::termwiz::input::KeyCode) {
        use ::termwiz::input::KeyCode as KC;
        match key {
            KC::Escape | KC::Enter | KC::Char('n') | KC::Char('N') => {
                self.answer_chrome_dialog(false)
            }
            KC::Char('y') | KC::Char('Y') => self.answer_chrome_dialog(true),
            _ => {}
        }
    }

    /// Mouse on the palette, the dialog or the backdrop behind them.
    pub(crate) fn mouse_event_modal(
        &mut self,
        item_type: &UIItemType,
        event: &MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        match (item_type, &event.kind) {
            (UIItemType::ModalBackdrop, WMEK::Press(_)) => {
                // Clicking beside the palette dismisses it; a dialog wants
                // an answer.
                self.tab_palette = None;
            }
            (UIItemType::TabPaletteRow { tab_idx }, WMEK::Release(MousePress::Left)) => {
                self.tab_palette = None;
                self.activate_tab(*tab_idx as isize).ok();
            }
            (UIItemType::DialogButton { confirm }, WMEK::Release(MousePress::Left)) => {
                self.answer_chrome_dialog(*confirm);
            }
            _ => {}
        }
        context.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(idx: usize, line1: &str, line2: &str) -> TabEntry {
        TabEntry {
            tab_idx: idx,
            active: false,
            line1: line1.to_string(),
            line2: line2.to_string(),
            mark: TabMark::Plain,
        }
    }

    #[test]
    fn the_palette_filter_matches_either_line_ignoring_case() {
        let entries = || {
            vec![
                entry(0, "Fix WSL probe", "Codex \u{b7} running"),
                entry(1, "prod-db", "ssh \u{b7} mosh"),
                entry(2, "Release plan", "Claude \u{b7} waiting"),
            ]
        };
        let idx = |query: &str| -> Vec<usize> {
            filter_tab_entries(entries(), query)
                .iter()
                .map(|entry| entry.tab_idx)
                .collect()
        };
        assert_eq!(idx(""), [0, 1, 2]);
        assert_eq!(idx("  "), [0, 1, 2]);
        assert_eq!(idx("wsl"), [0]);
        assert_eq!(idx("MOSH"), [1]);
        assert_eq!(idx("cl"), [2]);
        assert!(idx("nothing").is_empty());
    }
}
