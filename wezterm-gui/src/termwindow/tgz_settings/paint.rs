//! Draws the Settings tab over its pane.
//!
//! Sections down the left, the chosen section's settings on the right: a
//! name and a line of explanation, and one control. A setting the config
//! file computes shows where instead of a control. What a click or a key
//! does is decided in `pane.rs`.

use super::catalog::{in_section, Kind, Section, Setting, Value, CATALOG};
use super::lua_edit::{LuaValue, PathStatus};
use super::pane::{font_families, SettingsHit, SettingsPane, Statuses, PICKER_ROWS, TITLE};
use super::store;
use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::diff_panel::opaque;
use crate::termwindow::render::sidebar::{contrast_label_color, lerp_rgba};
use crate::termwindow::{TermWindow, UIItem, UIItemType};
use ::window::color::LinearRgba;
use ::window::{RectF, WindowOps};
use config::keyassignment::{KeyAssignment, SpawnTabDomain};
use mux::pane::{CachePolicy, Pane};
use mux::tab::{PositionedPane, Tab};
use mux::Mux;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a reading of the config file is trusted before it is read again;
/// somebody may be editing it by hand in the meantime.
const STATUS_TTL: Duration = Duration::from_secs(2);

/// Layout, in pixels at 96 dpi. Widths come from the chrome font's own
/// measurements, never from terminal columns: the tab reads the same
/// whatever the terminal font is.
const NAV_MIN_W: f32 = 132.;
const CONTROL_W: f32 = 320.;
const MAX_CONTENT_W: f32 = 820.;
/// Space on each side of a label inside its box.
const SEGMENT_PAD: f32 = 10.;

pub(crate) fn is_settings_pane(pane: &Arc<dyn Pane>) -> bool {
    pane.downcast_ref::<SettingsPane>().is_some()
}

/// The actions listed under Shortcuts: the fork's own, in the order somebody
/// new would meet them.
fn shortcut_actions() -> Vec<KeyAssignment> {
    use KeyAssignment::*;
    vec![
        ShowSettings,
        ActivateCommandPalette,
        SpawnTab(SpawnTabDomain::CurrentPaneDomain),
        ShowNewTabMenu,
        LaunchDefaultAgent,
        ShowAgentLaunchMenu,
        ShowSshHostMenu,
        ShowSessionsMenu,
        ShowUsageMenu,
        CycleWaitingAgent,
        ActivateAgentSection,
        ToggleAgentHerdView,
        FocusSidebarSearch,
        ToggleSidebarAutoHide,
        CustomizeSidebarButtons,
        TogglePaneToolbelt,
        OpenFileBrowser,
        FocusFileTree,
        ToggleDiffPanel,
        ActivateComposer,
        ToggleDockedInput,
        ToggleSecureKeyboardEntry,
        LeaveAlternateScreen,
    ]
}

/// One piece of a control: a label in a box, lit when it is the current
/// choice, clickable when it has a hit.
struct Segment {
    label: String,
    selected: bool,
    hit: Option<SettingsHit>,
    /// Width to reserve for the label, in pixels; the label's own when
    /// that is wider.
    min_px: f32,
    dim: bool,
}

impl Segment {
    fn button(label: impl Into<String>, hit: SettingsHit) -> Self {
        Self {
            label: label.into(),
            selected: false,
            hit: Some(hit),
            min_px: 0.,
            dim: false,
        }
    }

    fn label(label: impl Into<String>, min_px: f32) -> Self {
        Self {
            label: label.into(),
            selected: false,
            hit: None,
            min_px,
            dim: false,
        }
    }
}

fn tilde(path: &std::path::Path) -> String {
    match path.strip_prefix(&*config::HOME_DIR) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn value_text(setting: &Setting, value: &Value) -> String {
    match setting.to_lua(value) {
        LuaValue::Bool(b) => if b { "On" } else { "Off" }.to_string(),
        LuaValue::Int(n) => n.to_string(),
        LuaValue::Float(f) => {
            let text = format!("{f:.2}");
            let text = text.trim_end_matches('0');
            if text.ends_with('.') {
                format!("{text}0")
            } else {
                text.to_string()
            }
        }
        LuaValue::Str(s) | LuaValue::Font(s) => s,
    }
}

/// The pane's cell rectangle in window pixels, and the rectangle its
/// background fills, which reaches the window edges like a terminal pane's.
fn pane_rects(tw: &TermWindow, pos: &PositionedPane) -> anyhow::Result<(RectF, RectF)> {
    let (padding_left, padding_top) = tw.padding_left_top();
    let border = tw.get_os_border();
    let tab_bar_height = if tw.show_tab_bar && !tw.sidebar_is_active() {
        tw.tab_bar_pixel_height()?
    } else {
        0.
    };
    let top_bar = if tw.config.tab_bar_at_bottom {
        0.
    } else {
        tab_bar_height
    };
    let cw = tw.render_metrics.cell_size.width as f32;
    let ch = tw.render_metrics.cell_size.height as f32;
    let x0 = padding_left + border.left.get() as f32 + pos.left as f32 * cw;
    let y0 = top_bar + padding_top + border.top.get() as f32 + pos.top as f32 * ch;
    let content: RectF = euclid::rect(x0, y0, pos.width as f32 * cw, pos.height as f32 * ch);

    let rightmost = pos.left + pos.width >= tw.terminal_size.cols as usize;
    let bottommost = pos.top + pos.height >= tw.terminal_size.rows as usize;
    let left = if pos.left == 0 {
        x0 - padding_left.min(cw * 0.5)
    } else {
        x0 - cw * 0.5
    };
    let top = if pos.top == 0 {
        y0 - padding_top
    } else {
        y0 - ch * 0.5
    };
    let right = if rightmost {
        tw.dimensions.pixel_width as f32 - border.right.get() as f32
    } else {
        content.max_x() + cw * 0.5
    };
    let bottom = if bottommost {
        tw.dimensions.pixel_height as f32
            - tw.docked_input_pixel_height()
            - border.bottom.get() as f32
    } else {
        content.max_y() + ch * 0.5
    };
    let background = euclid::rect(left, top, (right - left).max(0.), (bottom - top).max(0.));
    Ok((content, background))
}

impl TermWindow {
    /// Text of the Settings tab, in the chrome font like the sidebar's.
    /// Takes what the cell-based painter took, so call sites read the same.
    fn settings_text(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        text: &str,
        x: f32,
        y: f32,
        pixel_width: f32,
        fg: LinearRgba,
        _bg: LinearRgba,
        bold: bool,
    ) -> anyhow::Result<()> {
        self.paint_chrome_text(layers, text, x, y, pixel_width, fg, bold)
    }

    /// Focus this window's Settings tab, creating it when there is none.
    pub(crate) fn show_settings(&mut self) {
        if let Err(err) = self.show_settings_impl() {
            log::error!("settings: could not open: {err:#}");
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Open the Settings tab on `section`.
    pub(crate) fn show_settings_section(&mut self, section: Section) {
        self.show_settings();
        let Some(pane) = self.active_settings_pane() else {
            return;
        };
        if let (Some(settings), Some(index)) = (
            pane.downcast_ref::<SettingsPane>(),
            Section::ALL.iter().position(|s| *s == section),
        ) {
            settings
                .state
                .lock()
                .hit(SettingsHit::Section(index), &self.config, pane.pane_id());
        }
    }

    fn show_settings_impl(&mut self) -> anyhow::Result<()> {
        let mux = Mux::get();
        let existing = mux.get_window(self.mux_window_id).and_then(|window| {
            window.iter_tabs().position(|tab| {
                tab.iter_panes_ignoring_zoom()
                    .iter()
                    .any(|p| is_settings_pane(&p.pane))
            })
        });
        if let Some(index) = existing {
            return self.activate_tab(index as isize);
        }

        let active = mux.get_active_tab_for_window(self.mux_window_id);
        let size = active.as_ref().map(|t| t.get_size()).unwrap_or_default();
        // A split opened beside Settings starts where you were working.
        let cwd = active
            .as_ref()
            .and_then(|t| t.get_active_pane())
            .and_then(|p| p.get_current_working_dir(CachePolicy::AllowStale));
        let domain_id = mux
            .get_domain_by_name("local")
            .unwrap_or_else(|| mux.default_domain())
            .domain_id();

        let pane: Arc<dyn Pane> = Arc::new(SettingsPane::new(domain_id, size, cwd));
        let tab = Arc::new(Tab::new(&size));
        tab.assign_pane(&pane);
        mux.add_tab_and_active_pane(&tab)?;
        mux.add_tab_to_window(&tab, self.mux_window_id)?;
        let count = mux
            .get_window(self.mux_window_id)
            .map(|w| w.count_tabs())
            .unwrap_or(1);
        self.activate_tab(count.saturating_sub(1) as isize)
    }

    /// The Settings pane of the active tab, when it holds one.
    fn active_settings_pane(&self) -> Option<Arc<dyn Pane>> {
        let tab = Mux::get().get_active_tab_for_window(self.mux_window_id)?;
        tab.iter_panes_ignoring_zoom()
            .into_iter()
            .map(|p| p.pane)
            .find(is_settings_pane)
    }

    /// The first key bound to `action`, as the menus spell keys.
    fn settings_shortcut_text(&self, action: &KeyAssignment) -> Option<String> {
        let (key, mods) = self
            .input_map
            .locate_app_wide_key_assignment(action)
            .into_iter()
            .next()?;
        let rendering = self.config.ui_key_cap_rendering;
        let separator = if rendering == ::window::UIKeyCapRendering::AppleSymbols {
            ""
        } else {
            "-"
        };
        let mut text = mods.to_string_with_separator(::window::ModifierToStringArgs {
            separator,
            want_none: false,
            ui_key_cap_rendering: Some(rendering),
        });
        if !text.is_empty() {
            text.push_str(separator);
        }
        text.push_str(&crate::inputmap::ui_key(&key, rendering));
        Some(text)
    }

    /// A row of boxes ending at `right`. Returns the x it starts at.
    fn paint_settings_segments(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
        right: f32,
        y: f32,
        segments: &[Segment],
        ground: LinearRgba,
    ) -> anyhow::Result<f32> {
        let sb = self.sidebar_palette();
        let accent = sb.accent();
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_h = self.render_metrics.cell_size.height as f32;
        let height = (cell_h + 6. * dpi_scale).ceil();
        // The sidebar's control radius, so both read as one product.
        let radius = (10. * dpi_scale).min(height * 0.5);
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());

        let seg_pad = (SEGMENT_PAD * dpi_scale).round();
        let widths: Vec<f32> = segments
            .iter()
            .map(|s| {
                (self.chrome_text_width(&s.label, false).max(s.min_px) + seg_pad * 2.)
                    .max(height)
                    .ceil()
            })
            .collect();
        let total: f32 = widths.iter().sum();
        let left = right - total;
        let trough = opaque(lerp_rgba(ground, sb.text_meta, 0.12));
        self.sidebar_rounded_fill(
            layers,
            0,
            euclid::rect(left, y, total, height),
            radius,
            trough,
        )?;

        let mut x = left;
        for (segment, width) in segments.iter().zip(widths) {
            let item_type = segment.hit.clone().map(UIItemType::Settings);
            let is_hovered = item_type.is_some() && hovered == item_type;
            let fill = if segment.selected {
                opaque(lerp_rgba(ground, accent, 0.85))
            } else if is_hovered {
                opaque(lerp_rgba(ground, sb.text_meta, 0.3))
            } else {
                trough
            };
            if fill != trough {
                self.sidebar_rounded_fill(
                    layers,
                    0,
                    euclid::rect(x, y, width, height),
                    radius,
                    fill,
                )?;
            }
            let fg = if segment.selected {
                contrast_label_color(fill, sb.text_active)
            } else if segment.dim {
                sb.text_meta
            } else {
                sb.text_active
            };
            let label_px = self.chrome_text_width(&segment.label, false);
            // Centred in its box; a label too long for it starts at the
            // padding and is cut at the far one.
            let inset = ((width - label_px) * 0.5).max(seg_pad).round();
            self.settings_text(
                layers,
                &segment.label,
                x + inset,
                y + (height - cell_h) * 0.5,
                (width - inset - seg_pad).max(1.) + 1.,
                fg,
                fill,
                false,
            )?;
            if let Some(item_type) = item_type {
                self.ui_items.push(UIItem {
                    x: x.max(0.) as usize,
                    y: y.max(0.) as usize,
                    width: width as usize,
                    height: height as usize,
                    item_type,
                });
            }
            x += width;
        }
        Ok(left)
    }

    pub(crate) fn paint_settings_pane(
        &mut self,
        pos: &PositionedPane,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(pane) = pos.pane.downcast_ref::<SettingsPane>() else {
            return Ok(());
        };
        let pane_id = pos.pane.pane_id();
        let state = Arc::clone(&pane.state);
        let (area, background) = pane_rects(self, pos)?;

        let sb = self.sidebar_palette();
        let accent = sb.accent();
        // The sidebar's and the panels' ground, so the tab is one more
        // surface of the same chrome and not a terminal with boxes on it.
        let ground = opaque(sb.surface);
        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let cell_h = self.render_metrics.cell_size.height as f32;
        self.filled_rectangle(layers, 0, background, ground)?;

        let mut state = state.lock();
        if state
            .statuses
            .as_ref()
            .map_or(true, |s| s.read_at.elapsed() >= STATUS_TTL)
        {
            state.file = store::config_path();
            let paths: Vec<&[&str]> = CATALOG.iter().map(|s| s.path).collect();
            state.statuses = Some(Statuses {
                read_at: Instant::now(),
                by_setting: store::statuses(&state.file, &paths),
            });
        }
        let editable = state
            .statuses
            .as_ref()
            .is_some_and(|s| s.by_setting.is_some());
        let file_label = tilde(&state.file);

        // The font list arrives from a worker: take it once it is there.
        if let Some(picker) = state.editing.as_mut().and_then(|e| e.picker.as_mut()) {
            if picker.loading {
                if let Some(families) = font_families(pane_id) {
                    picker.items = families;
                    picker.loading = false;
                }
            }
        }

        let pad = (20. * dpi_scale).round();
        let unit = (8. * dpi_scale).round();
        let left = area.origin.x + pad;
        let right = area.max_x() - pad;
        if right - left < 300. * dpi_scale || area.size.height < 6. * cell_h {
            return Ok(());
        }

        // Header: the tab's name, and the file everything is written to.
        let header_y = area.origin.y + (cell_h * 0.5).floor();
        self.settings_text(
            layers,
            TITLE,
            left,
            header_y,
            self.chrome_text_width(TITLE, true) + 2.,
            sb.text_active,
            ground,
            true,
        )?;
        let open = [Segment::button(file_label, SettingsHit::OpenFile)];
        self.paint_settings_segments(layers, right, header_y - 3. * dpi_scale, &open, ground)?;

        let body_top = header_y + cell_h * 2.5;
        let footer_y = area.max_y() - cell_h * 1.5;

        // Sections.
        let nav_w = Section::ALL
            .iter()
            .map(|section| self.chrome_text_width(section.label(), true))
            .fold(0., f32::max)
            .ceil()
            .max(NAV_MIN_W * dpi_scale - unit * 3.)
            + unit * 3.;
        let nav_row_h = (cell_h + 8. * dpi_scale).ceil();
        let hovered = self
            .last_ui_item
            .as_ref()
            .map(|item| item.item_type.clone());
        for (index, section) in Section::ALL.iter().enumerate() {
            let y = body_top + index as f32 * nav_row_h;
            if y + nav_row_h > footer_y {
                break;
            }
            let item_type = UIItemType::Settings(SettingsHit::Section(index));
            let active = index == state.section;
            let fill = if active {
                opaque(lerp_rgba(ground, accent, 0.28))
            } else if hovered.as_ref() == Some(&item_type) {
                opaque(lerp_rgba(ground, sb.text_meta, 0.16))
            } else {
                ground
            };
            if fill != ground {
                self.sidebar_rounded_fill(
                    layers,
                    0,
                    euclid::rect(left, y, nav_w, nav_row_h),
                    (10. * dpi_scale).min(nav_row_h * 0.5),
                    fill,
                )?;
            }
            self.settings_text(
                layers,
                section.label(),
                left + unit * 1.5,
                y + (nav_row_h - cell_h) * 0.5,
                nav_w - unit * 3. + 1.,
                if active { sb.text_active } else { sb.text_idle },
                fill,
                active,
            )?;
            self.ui_items.push(UIItem {
                x: left as usize,
                y: y as usize,
                width: nav_w as usize,
                height: nav_row_h as usize,
                item_type,
            });
        }

        let section = state.current_section();
        let content_x = left + nav_w + unit * 3.;
        let content_w = (right - content_x).min(MAX_CONTENT_W * dpi_scale);
        let content_right = content_x + content_w;
        let body_h = footer_y - cell_h * 0.5 - body_top;

        // Shortcuts: a list to read.
        if section == Section::Shortcuts {
            let rows: Vec<(String, Option<String>)> = shortcut_actions()
                .iter()
                .filter_map(|action| {
                    let command = crate::commands::derive_command_from_key_assignment(action)?;
                    Some((
                        command.brief.to_string(),
                        self.settings_shortcut_text(action),
                    ))
                })
                .collect();
            let line_h = (cell_h + 8. * dpi_scale).ceil();
            let visible = ((body_h / line_h).floor() as usize).max(1);
            state.visible_rows = visible;
            state.max_scroll = rows.len().saturating_sub(visible);
            state.scroll = state.scroll.min(state.max_scroll);
            for (slot, (brief, keys)) in rows.iter().skip(state.scroll).take(visible).enumerate() {
                let y = body_top + slot as f32 * line_h;
                let (keys, fg) = match keys {
                    Some(keys) => (keys.as_str(), sb.text_active),
                    None => ("not bound", sb.text_meta),
                };
                let keys_w = self.chrome_text_width(keys, false).ceil() + 1.;
                self.settings_text(
                    layers,
                    brief,
                    content_x,
                    y,
                    (content_w - keys_w - unit * 2.).max(unit),
                    sb.text_idle,
                    ground,
                    false,
                )?;
                self.settings_text(
                    layers,
                    keys,
                    content_right - keys_w,
                    y,
                    keys_w,
                    fg,
                    ground,
                    false,
                )?;
            }
            let footer = if state.max_scroll > 0 {
                "Change these with config.keys in the config file. Scroll for more."
            } else {
                "Change these with config.keys in the config file."
            };
            self.settings_text(
                layers,
                footer,
                content_x,
                footer_y,
                right - content_x,
                sb.text_meta,
                ground,
                false,
            )?;
            return Ok(());
        }

        // Settings of the chosen section.
        let rows = in_section(section);
        let control_w = (CONTROL_W * dpi_scale).min(content_w * 0.55);
        let label_w = (content_w - control_w - unit * 2.).max(unit * 8.);
        let row_h = (cell_h * 2. + 12. * dpi_scale).ceil();
        let visible = ((body_h / row_h).floor() as usize).max(1);
        state.visible_rows = visible;
        state.max_scroll = rows.len().saturating_sub(visible);
        state.scroll = state.scroll.min(state.max_scroll);
        let control_h = (cell_h + 6. * dpi_scale).ceil();
        let seg_pad = (SEGMENT_PAD * dpi_scale).round();
        // A text field is as wide as a control may be; its text has that
        // less the padding.
        let field_w = control_w.max(unit * 12.).round();
        let field_text_w = field_w - seg_pad * 2.;
        let control_y_of =
            |slot: usize| body_top + slot as f32 * row_h + (cell_h * 2. - control_h) * 0.5;

        // The open picker's list: what it shows and where, worked out before
        // the rows so the controls it covers are left out.
        struct Popup {
            rect: RectF,
            /// Matches in view, with their place among all matches.
            shown: Vec<(usize, String)>,
            selected: usize,
            note: Option<&'static str>,
        }
        let item_h = (cell_h + 4. * dpi_scale).ceil();
        let popup_pad = (4. * dpi_scale).round();
        let first_row = state.scroll;
        let popup = state.editing.as_mut().and_then(|editing| {
            let slot = rows
                .iter()
                .skip(first_row)
                .take(visible)
                .position(|index| *index == editing.index)?;
            let matches: Vec<String> = editing.matches().iter().map(|m| m.to_string()).collect();
            let is_font = matches!(CATALOG[editing.index].kind, Kind::Font);
            let picker = editing.picker.as_mut()?;
            picker.selected = picker.selected.min(matches.len().saturating_sub(1));
            picker.scroll = picker.scroll.min(matches.len().saturating_sub(PICKER_ROWS));
            let note = if picker.loading {
                Some("Looking for fonts\u{2026}")
            } else if matches.is_empty() && is_font {
                Some("Not listed. Enter uses what you typed.")
            } else if matches.is_empty() {
                Some("Nothing matches.")
            } else {
                None
            };
            let shown: Vec<(usize, String)> = matches
                .into_iter()
                .enumerate()
                .skip(picker.scroll)
                .take(PICKER_ROWS)
                .collect();
            let height = shown.len().max(1) as f32 * item_h + popup_pad * 2.;
            let field_y = control_y_of(slot);
            let below = field_y + control_h + popup_pad;
            let y = if below + height <= area.max_y() - popup_pad {
                below
            } else {
                (field_y - popup_pad - height).max(area.origin.y + popup_pad)
            };
            Some(Popup {
                rect: euclid::rect(content_right - field_w, y, field_w, height),
                shown,
                selected: picker.selected,
                note,
            })
        });

        for (slot, index) in rows.iter().skip(state.scroll).take(visible).enumerate() {
            let setting = &CATALOG[*index];
            let y = body_top + slot as f32 * row_h;
            let focused = state.focus == Some(state.scroll + slot);
            let row_bg = if focused {
                opaque(lerp_rgba(ground, sb.text_meta, 0.1))
            } else {
                ground
            };
            if focused {
                let band: RectF = euclid::rect(
                    content_x - unit * 1.5,
                    y - 5. * dpi_scale,
                    content_w + unit * 3.,
                    cell_h * 2. + 10. * dpi_scale,
                );
                self.sidebar_rounded_fill(layers, 0, band, 10. * dpi_scale, row_bg)?;
                // The selection bar of a sidebar row.
                let bar_w = (3. * dpi_scale).round();
                self.sidebar_pill_fill(
                    layers,
                    0,
                    euclid::rect(
                        band.origin.x + 2. * dpi_scale,
                        band.origin.y + band.size.height * 0.25,
                        bar_w,
                        band.size.height * 0.5,
                    ),
                    bar_w * 0.5,
                    accent,
                )?;
            }
            self.settings_text(
                layers,
                setting.label,
                content_x,
                y,
                label_w,
                sb.text_active,
                row_bg,
                false,
            )?;
            {
                use crate::termwindow::tgz_ui_text::{Align, UiFont};
                let help_h = self.ui_line_height(UiFont::Small);
                self.paint_ui_text(
                    layers,
                    UiFont::Small,
                    setting.help,
                    content_x,
                    y + cell_h + (cell_h - help_h) * 0.5,
                    label_w,
                    sb.text_meta,
                    Align::Left,
                )?;
            }

            let control_y = control_y_of(slot);
            let editing_this = state
                .editing
                .as_ref()
                .is_some_and(|editing| editing.index == *index);
            // Under the open list: its control would show through.
            if !editing_this
                && popup.as_ref().is_some_and(|popup| {
                    control_y < popup.rect.max_y() && control_y + control_h > popup.rect.origin.y
                })
            {
                continue;
            }

            let value = (setting.get)(&self.config);
            let status = state
                .statuses
                .as_ref()
                .and_then(|s| s.by_setting.as_ref())
                .and_then(|all| all.get(*index))
                .cloned();

            let segments: Vec<Segment> = if let Some(PathStatus::Locked { line }) = status {
                vec![
                    Segment {
                        dim: true,
                        ..Segment::label(value_text(setting, &value), 0.)
                    },
                    Segment::button(format!("set in code, line {line}"), SettingsHit::OpenFile),
                ]
            } else if !editable {
                vec![Segment {
                    dim: true,
                    ..Segment::label(value_text(setting, &value), 0.)
                }]
            } else {
                match (setting.kind, &value) {
                    (Kind::Toggle, Value::Bool(on)) => vec![
                        Segment {
                            selected: !*on,
                            ..Segment::button("Off", SettingsHit::Toggle(*index))
                        },
                        Segment {
                            selected: *on,
                            ..Segment::button("On", SettingsHit::Toggle(*index))
                        },
                    ],
                    (Kind::Choice(variants), Value::Text(current)) => {
                        let variants = variants();
                        let needed: f32 = variants
                            .iter()
                            .map(|v| (self.chrome_text_width(v, false) + seg_pad * 2.).ceil())
                            .sum();
                        if needed <= control_w {
                            variants
                                .iter()
                                .enumerate()
                                .map(|(n, variant)| Segment {
                                    selected: variant == current,
                                    ..Segment::button(*variant, SettingsHit::Choose(*index, n))
                                })
                                .collect()
                        } else {
                            let widest = variants
                                .iter()
                                .map(|v| self.chrome_text_width(v, false))
                                .fold(0., f32::max);
                            vec![
                                Segment::button("\u{2039}", SettingsHit::Step(*index, -1)),
                                Segment::label(current.clone(), widest.ceil()),
                                Segment::button("\u{203a}", SettingsHit::Step(*index, 1)),
                            ]
                        }
                    }
                    (Kind::Int { .. } | Kind::Float { .. }, _) => vec![
                        Segment::button("\u{2212}", SettingsHit::Step(*index, -1)),
                        Segment::label(
                            value_text(setting, &value),
                            self.chrome_text_width("00000", false).ceil(),
                        ),
                        Segment::button("+", SettingsHit::Step(*index, 1)),
                    ],
                    (Kind::Text | Kind::Font | Kind::Scheme, Value::Text(current)) => {
                        let typed = state
                            .editing
                            .as_ref()
                            .filter(|editing| editing.index == *index)
                            .map(|editing| (editing.buffer.clone(), editing.picker.is_some()));
                        let (label, dim) = match typed {
                            Some((buffer, true)) if buffer.is_empty() => {
                                ("| type to filter".to_string(), true)
                            }
                            // The end of a long entry stays in view, with
                            // room for the caret after it.
                            Some((buffer, _)) => (
                                format!(
                                    "{}|",
                                    self.chrome_elide_start(
                                        &buffer,
                                        field_text_w - self.chrome_text_width("|", false) - 1.,
                                        false,
                                    )
                                ),
                                false,
                            ),
                            None if current.is_empty() => ("not set".to_string(), true),
                            None => (
                                self.chrome_elide_start(current, field_text_w - 1., false),
                                false,
                            ),
                        };
                        vec![Segment {
                            label,
                            selected: false,
                            hit: Some(SettingsHit::Field(*index)),
                            min_px: field_text_w,
                            dim,
                        }]
                    }
                    _ => vec![Segment::label(value_text(setting, &value), 0.)],
                }
            };
            let control_left =
                self.paint_settings_segments(layers, content_right, control_y, &segments, row_bg)?;

            // The field being typed into gets an accent line under it.
            if editing_this {
                self.filled_rectangle(
                    layers,
                    0,
                    euclid::rect(
                        control_left,
                        control_y + control_h,
                        content_right - control_left,
                        (2. * dpi_scale).round().max(1.),
                    ),
                    accent,
                )?;
            }
        }

        if let Some(popup) = &popup {
            let surface = opaque(lerp_rgba(ground, sb.text_meta, 0.14));
            let stroke = dpi_scale.round().max(1.);
            self.sidebar_rounded_fill(
                layers,
                0,
                popup.rect,
                8. * dpi_scale,
                opaque(lerp_rgba(ground, sb.text_meta, 0.45)),
            )?;
            self.sidebar_rounded_fill(
                layers,
                0,
                euclid::rect(
                    popup.rect.origin.x + stroke,
                    popup.rect.origin.y + stroke,
                    popup.rect.size.width - stroke * 2.,
                    popup.rect.size.height - stroke * 2.,
                ),
                (8. * dpi_scale - stroke).max(0.),
                surface,
            )?;
            let inner_x = popup.rect.origin.x + popup_pad;
            let inner_w = popup.rect.size.width - popup_pad * 2.;
            let top = popup.rect.origin.y + popup_pad;
            if let Some(note) = popup.note {
                self.settings_text(
                    layers,
                    note,
                    inner_x + unit,
                    top + (item_h - cell_h) * 0.5,
                    inner_w - unit * 2.,
                    sb.text_meta,
                    surface,
                    false,
                )?;
            }
            for (slot, (at, item)) in popup.shown.iter().enumerate() {
                let y = top + slot as f32 * item_h;
                let item_type = UIItemType::Settings(SettingsHit::Pick(*at));
                let fill = if *at == popup.selected {
                    opaque(lerp_rgba(surface, accent, 0.45))
                } else if hovered.as_ref() == Some(&item_type) {
                    opaque(lerp_rgba(surface, sb.text_meta, 0.25))
                } else {
                    surface
                };
                if fill != surface {
                    self.sidebar_rounded_fill(
                        layers,
                        0,
                        euclid::rect(inner_x, y, inner_w, item_h),
                        (6. * dpi_scale).min(item_h * 0.5),
                        fill,
                    )?;
                }
                self.settings_text(
                    layers,
                    item,
                    inner_x + unit,
                    y + (item_h - cell_h) * 0.5,
                    inner_w - unit * 2.,
                    contrast_label_color(fill, sb.text_active),
                    fill,
                    false,
                )?;
                self.ui_items.push(UIItem {
                    x: inner_x as usize,
                    y: y as usize,
                    width: inner_w as usize,
                    height: item_h as usize,
                    item_type,
                });
            }
        }

        let footer = match &state.message {
            Some((text, _)) => text.clone(),
            None if !editable => store::ApplyError::Shape.message(&state.file),
            None if popup.is_some() => {
                "Type to filter \u{00b7} \u{2191}\u{2193} choose \u{00b7} Enter takes it \u{00b7} Esc cancels"
                    .to_string()
            }
            None if state.focus.is_some() => {
                "\u{2191}\u{2193} rows \u{00b7} \u{2190}\u{2192} change \u{00b7} Enter opens a field \u{00b7} Tab next section"
                    .to_string()
            }
            None if state.max_scroll > 0 => format!(
                "{}\u{2013}{} of {}  \u{00b7}  scroll for more",
                state.scroll + 1,
                (state.scroll + visible).min(rows.len()),
                rows.len()
            ),
            None => "Changes are saved to the config file as you make them.".to_string(),
        };
        let is_error = state.message.as_ref().is_some_and(|(_, error)| *error) || !editable;
        self.settings_text(
            layers,
            &footer,
            content_x,
            footer_y,
            right - content_x,
            if is_error {
                sb.text_active
            } else {
                sb.text_meta
            },
            ground,
            false,
        )?;
        Ok(())
    }

    /// A left click on one of the Settings tab's controls.
    pub(crate) fn settings_click(&mut self, hit: SettingsHit) {
        let Some(pane) = self.active_settings_pane() else {
            return;
        };
        let Some(settings) = pane.downcast_ref::<SettingsPane>() else {
            return;
        };
        let config = self.config.clone();
        settings.state.lock().hit(hit, &config, pane.pane_id());
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }
}
