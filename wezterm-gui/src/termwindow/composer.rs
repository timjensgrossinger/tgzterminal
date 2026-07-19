//! Optional multiline input composer (Track 3, "rich input").
//!
//! Two surfaces share one editing core ([`ComposerBuffer`]):
//!
//! - [`Composer`]: a bottom-anchored modal overlay opened on demand.
//! - [`DockedInput`]: a persistent Warp-style input strip docked at the bottom
//!   of the active pane, always visible while `rich_input.docked` is enabled.
//!   Keyboard focus toggles between the terminal and the strip.
//!
//! Neither surface attaches hidden file contents or runs commands — context
//! helpers insert plain references only, and submit sends the visible text to
//! the pane as ordinary bracketed-paste terminal input.

use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::{DimensionContext, TermWindow, TermWindowNotif};
use crate::utilsprites::RenderMetrics;
use config::keyassignment::{ClipboardPasteSource, KeyAssignment};
use config::Dimension;
use mux::pane::{CachePolicy, Pane, PaneId};
use percent_encoding::percent_decode_str;
use std::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use wezterm_font::LoadedFont;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;
use window::{Clipboard, WindowOps};

/// Full-block glyph used to mark the caret position in the rendered buffer.
const CURSOR_GLYPH: char = '\u{2588}';

/// Byte offset of character index `char_col` within `line`.
fn byte_idx(line: &str, char_col: usize) -> usize {
    line.char_indices()
        .nth(char_col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

/// Normalize pasted/inserted text: strip carriage returns so line splitting
/// is consistent regardless of the source platform.
fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Shared multiline text-editing core used by both the modal composer and the
/// docked input strip. Plain fields with `&mut self` methods; callers provide
/// their own interior mutability where needed.
pub struct ComposerBuffer {
    /// Buffer content, one entry per logical line. Always non-empty.
    pub lines: Vec<String>,
    /// Cursor line index into `lines`.
    pub cursor_line: usize,
    /// Cursor column as a character (not byte) index within the current line.
    pub cursor_col: usize,
    /// First visible line index for internal scrolling.
    pub scroll_top: usize,
}

impl ComposerBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            scroll_top: 0,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_multiline(&self) -> bool {
        self.lines.len() > 1
    }

    pub fn total_chars(&self) -> usize {
        self.lines.iter().map(|l| char_len(l)).sum()
    }

    /// Insert arbitrary (possibly multiline) text at the cursor.
    pub fn insert_text(&mut self, text: &str) {
        let norm = normalize_paste(text);
        let parts: Vec<&str> = norm.split('\n').collect();

        let cl = self.cursor_line;
        let split_at = byte_idx(&self.lines[cl], self.cursor_col);
        let tail: String = self.lines[cl].split_off(split_at);
        self.lines[cl].push_str(parts[0]);

        if parts.len() == 1 {
            self.cursor_col += char_len(parts[0]);
            self.lines[cl].push_str(&tail);
        } else {
            let mut at = cl + 1;
            for p in &parts[1..parts.len() - 1] {
                self.lines.insert(at, (*p).to_string());
                at += 1;
            }
            let last = parts[parts.len() - 1];
            let new_cc = char_len(last);
            let mut last_line = last.to_string();
            last_line.push_str(&tail);
            self.lines.insert(at, last_line);
            self.cursor_line = at;
            self.cursor_col = new_cc;
        }
    }

    pub fn insert_char(&mut self, c: char) {
        // A literal string keeps char/byte handling in insert_text.
        self.insert_text(&c.to_string());
    }

    pub fn newline(&mut self) {
        self.insert_text("\n");
    }

    pub fn backspace(&mut self) {
        let cl = self.cursor_line;
        if self.cursor_col > 0 {
            let b0 = byte_idx(&self.lines[cl], self.cursor_col - 1);
            let b1 = byte_idx(&self.lines[cl], self.cursor_col);
            self.lines[cl].replace_range(b0..b1, "");
            self.cursor_col -= 1;
        } else if cl > 0 {
            let cur = self.lines.remove(cl);
            self.cursor_line -= 1;
            self.cursor_col = char_len(&self.lines[self.cursor_line]);
            self.lines[self.cursor_line].push_str(&cur);
        }
    }

    pub fn delete_forward(&mut self) {
        let cl = self.cursor_line;
        let cc = self.cursor_col;
        let line_len = char_len(&self.lines[cl]);
        if cc < line_len {
            let b0 = byte_idx(&self.lines[cl], cc);
            let b1 = byte_idx(&self.lines[cl], cc + 1);
            self.lines[cl].replace_range(b0..b1, "");
        } else if cl + 1 < self.lines.len() {
            let next = self.lines.remove(cl + 1);
            self.lines[cl].push_str(&next);
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = char_len(&self.lines[self.cursor_line]);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor_col < char_len(&self.lines[self.cursor_line]) {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(char_len(&self.lines[self.cursor_line]));
        } else {
            self.cursor_col = 0;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(char_len(&self.lines[self.cursor_line]));
        } else {
            self.cursor_col = char_len(&self.lines[self.cursor_line]);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_col = char_len(&self.lines[self.cursor_line]);
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.scroll_top = 0;
    }

    /// Replace the entire buffer (used by history recall) and place the cursor
    /// at the end.
    pub fn set_text(&mut self, text: &str) {
        let norm = normalize_paste(text);
        let lines: Vec<String> = if norm.is_empty() {
            vec![String::new()]
        } else {
            norm.split('\n').map(|l| l.to_string()).collect()
        };
        let last = lines.len() - 1;
        let last_len = char_len(&lines[last]);
        self.lines = lines;
        self.cursor_line = last;
        self.cursor_col = last_len;
        self.scroll_top = 0;
    }

    /// Adjust `scroll_top` so the cursor line stays within `max_rows` visible
    /// rows and return the resulting first-visible-line index.
    fn ensure_cursor_visible(&mut self, max_rows: usize) -> usize {
        if self.cursor_line < self.scroll_top {
            self.scroll_top = self.cursor_line;
        } else if self.cursor_line >= self.scroll_top + max_rows {
            self.scroll_top = self.cursor_line + 1 - max_rows;
        }
        self.scroll_top
    }

    /// Produce up to `max_rows` display strings for the visible window,
    /// optionally with the cursor glyph inserted on the cursor line. Blank
    /// lines are rendered as a single space so they still occupy a row.
    fn display_rows(&mut self, max_rows: usize, show_cursor: bool) -> Vec<String> {
        let top = self.ensure_cursor_visible(max_rows);
        let cursor_line = self.cursor_line;
        let cursor_col = self.cursor_col;
        let mut out = Vec::new();
        for (idx, line) in self.lines.iter().enumerate().skip(top).take(max_rows) {
            let display = if show_cursor && idx == cursor_line {
                let b = byte_idx(line, cursor_col);
                format!("{}{}{}", &line[..b], CURSOR_GLYPH, &line[b..])
            } else {
                line.clone()
            };
            out.push(if display.is_empty() {
                " ".to_string()
            } else {
                display
            });
        }
        out
    }
}

/// Recall the previous history entry into `buffer`, advancing `history_pos`.
fn history_recall_prev(buffer: &mut ComposerBuffer, history: &[String], pos: &mut Option<usize>) {
    if history.is_empty() {
        return;
    }
    let new = match *pos {
        None => history.len() - 1,
        Some(p) => p.saturating_sub(1),
    };
    *pos = Some(new);
    buffer.set_text(&history[new]);
}

/// Recall the next (newer) history entry; stepping past the newest clears back
/// to an empty buffer.
fn history_recall_next(buffer: &mut ComposerBuffer, history: &[String], pos: &mut Option<usize>) {
    match *pos {
        None => {}
        Some(p) if p + 1 < history.len() => {
            *pos = Some(p + 1);
            buffer.set_text(&history[p + 1]);
        }
        Some(_) => {
            *pos = None;
            buffer.clear();
        }
    }
}

/// Persistent, focusable input strip docked at the bottom of the active pane.
pub struct DockedInput {
    pub buffer: ComposerBuffer,
    /// When true, keystrokes are routed into the buffer instead of the pane.
    pub focused: bool,
    /// Panes for which the docked strip has been activated via the agent
    /// toolbelt button (or toggle key). The strip renders only when the active
    /// pane is in this set — it is not persistent chrome.
    pub enabled_panes: HashSet<PaneId>,
    /// Position within the shared history ring during recall.
    history_pos: Option<usize>,
    /// Number of body rows visible on screen (recomputed each layout).
    max_visible_rows: usize,
}

impl DockedInput {
    pub fn new() -> Self {
        Self {
            buffer: ComposerBuffer::new(),
            focused: false,
            enabled_panes: HashSet::new(),
            history_pos: None,
            max_visible_rows: 1,
        }
    }
}

pub struct Composer {
    element: RefCell<Option<Vec<ComputedElement>>>,
    buffer: RefCell<ComposerBuffer>,
    /// Number of body rows visible on screen (recomputed each layout).
    max_visible_rows: RefCell<usize>,
    /// Current position within the shared history ring during recall.
    history_pos: RefCell<Option<usize>>,
    /// Pending second-press confirmation for multiline submit.
    confirm_pending: RefCell<bool>,
    show_send_preview: bool,
    require_confirm_for_multiline: bool,
    history_limit: usize,
    is_agent: bool,
}

impl Composer {
    /// Build a composer for the active pane, honoring the `rich_input` config
    /// gates. Returns `None` when disabled or when agent-only gating rejects
    /// a non-agent pane.
    pub fn new(term_window: &mut TermWindow, pane: &Arc<dyn Pane>) -> Option<Self> {
        let cfg = term_window.config.rich_input.clone();
        if !cfg.enabled {
            return None;
        }
        let is_agent = term_window.pane_is_agent(pane);
        if cfg.agent_panes_only && !is_agent {
            return None;
        }

        Some(Self {
            element: RefCell::new(None),
            buffer: RefCell::new(ComposerBuffer::new()),
            max_visible_rows: RefCell::new(1),
            history_pos: RefCell::new(None),
            confirm_pending: RefCell::new(false),
            show_send_preview: cfg.show_send_preview,
            require_confirm_for_multiline: cfg.require_confirm_for_multiline,
            history_limit: cfg.history_limit,
            is_agent,
        })
    }

    fn reset_confirm(&self) {
        *self.confirm_pending.borrow_mut() = false;
    }

    fn recall_prev(&self, term_window: &TermWindow) {
        let hist = term_window.composer_history.borrow();
        let mut pos = self.history_pos.borrow_mut();
        history_recall_prev(&mut self.buffer.borrow_mut(), &hist, &mut pos);
    }

    fn recall_next(&self, term_window: &TermWindow) {
        let hist = term_window.composer_history.borrow();
        let mut pos = self.history_pos.borrow_mut();
        history_recall_next(&mut self.buffer.borrow_mut(), &hist, &mut pos);
    }

    /// Insert the active pane's working directory as a plain path reference.
    fn insert_cwd(&self, term_window: &TermWindow) {
        if let Some(path) = active_pane_cwd(term_window) {
            self.buffer.borrow_mut().insert_text(&path);
        }
    }

    /// Insert the current terminal selection (text or a selected path) verbatim.
    fn insert_selection(&self, term_window: &TermWindow) {
        if let Some(pane) = term_window.get_active_pane_or_overlay() {
            let text = term_window.selection_text(&pane);
            if !text.is_empty() {
                self.buffer.borrow_mut().insert_text(&text);
            }
        }
    }

    /// Read the clipboard asynchronously and insert the result into the buffer
    /// if the composer is still the active modal when the read completes.
    fn request_paste(&self, term_window: &mut TermWindow, source: ClipboardPasteSource) {
        let window = match term_window.window.as_ref() {
            Some(w) => w.clone(),
            None => return,
        };
        let clip = match source {
            ClipboardPasteSource::Clipboard => Clipboard::Clipboard,
            ClipboardPasteSource::PrimarySelection => Clipboard::PrimarySelection,
        };
        let future = window.get_clipboard(clip);
        let window = window.clone();
        promise::spawn::spawn(async move {
            if let Ok(text) = future.await {
                window.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                    if let Some(modal) = myself.get_modal() {
                        if let Some(composer) = modal.downcast_ref::<Composer>() {
                            composer.buffer.borrow_mut().insert_text(&text);
                        }
                    }
                    myself.invalidate_modal();
                })));
            }
        })
        .detach();
    }

    fn submit(&self, term_window: &mut TermWindow) {
        let text = self.buffer.borrow().text();
        if text.trim().is_empty() {
            term_window.cancel_modal();
            return;
        }

        let is_multiline = self.buffer.borrow().is_multiline();
        if self.require_confirm_for_multiline && is_multiline && !*self.confirm_pending.borrow() {
            *self.confirm_pending.borrow_mut() = true;
            return;
        }

        push_history(&term_window.composer_history, self.history_limit, &text);
        send_text_to_active_pane(term_window, &text);
        term_window.cancel_modal();
    }

    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        // Guard against a degenerate zero cell height before using it as a divisor.
        let cell_h = (metrics.cell_size.height as f32).max(1.0);

        let fg: InheritableColor = term_window
            .config
            .command_palette_fg_color
            .to_linear()
            .into();
        let bg_color = term_window.config.command_palette_bg_color.to_linear();

        // Determine how many body rows fit (cap at a sane maximum) and scroll so
        // the cursor stays visible.
        let max_rows =
            ((term_window.dimensions.pixel_height as f32 * 0.4 / cell_h) as usize).clamp(1, 12);
        *self.max_visible_rows.borrow_mut() = max_rows;

        let header = if self.is_agent {
            "▌ Compose → agent pane"
        } else {
            "▌ Compose"
        };
        let footer = if *self.confirm_pending.borrow() {
            "Press Ctrl+Enter again to send multiline".to_string()
        } else {
            let base = "Ctrl+Enter send · Enter newline · Esc cancel · Alt+D cwd · Alt+S selection";
            if self.show_send_preview {
                let buffer = self.buffer.borrow();
                format!(
                    "{base}   [{} lines, {} chars]",
                    buffer.lines.len(),
                    buffer.total_chars()
                )
            } else {
                base.to_string()
            }
        };

        let body_rows = self.buffer.borrow_mut().display_rows(max_rows, true);
        let render_rows = 2 + body_rows.len().max(1);

        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size;
        let (padding_left, padding_top) = term_window.padding_left_top();
        let border = term_window.get_os_border();
        let cell_w = term_window.render_metrics.cell_size.width as f32;
        let avail_pixel_width = size.cols as f32 * cell_w;

        let bottom_bar_height = if term_window.show_tab_bar
            && !term_window.sidebar_is_active()
            && term_window.config.tab_bar_at_bottom
        {
            term_window.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };

        let element = build_composer_element(
            &font,
            header,
            &body_rows,
            &footer,
            fg,
            bg_color,
            bg_color,
            avail_pixel_width - 2. * cell_w,
        );

        // Anchor the box to the bottom of the content area.
        let approx_height = (render_rows as f32 + 2.0) * cell_h;
        let usable_bottom =
            dimensions.pixel_height as f32 - border.bottom.get() as f32 - bottom_bar_height;
        let min_top = padding_top + border.top.get() as f32;
        let top_pixel_y = (usable_bottom - approx_height).max(min_top);

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: cell_h,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: cell_w,
                },
                bounds: euclid::rect(padding_left, top_pixel_y, avail_pixel_width, approx_height),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        Ok(vec![computed])
    }
}

/// Push `text` onto the shared history ring, de-duplicating consecutive
/// submissions and trimming to `limit`.
fn push_history(history: &RefCell<Vec<String>>, limit: usize, text: &str) {
    if limit == 0 {
        return;
    }
    let mut hist = history.borrow_mut();
    if hist.last().map(|s| s == text).unwrap_or(false) {
        return;
    }
    hist.push(text.to_string());
    while hist.len() > limit {
        hist.remove(0);
    }
}

/// The active pane's working directory as a plain decoded path, if available.
fn active_pane_cwd(term_window: &TermWindow) -> Option<String> {
    let pane = term_window.get_active_pane_or_overlay()?;
    let url = pane.get_current_working_dir(CachePolicy::AllowStale)?;
    percent_decode_str(url.path())
        .decode_utf8()
        .ok()
        .map(|p| p.into_owned())
        .filter(|p| !p.is_empty())
}

/// Send `text` to the active pane as a bracketed paste followed by a single
/// carriage return, so multiline content and control characters are delivered
/// safely and the CLI decides how to act on submit.
fn send_text_to_active_pane(term_window: &mut TermWindow, text: &str) {
    if let Some(pane) = term_window.get_active_pane_or_overlay() {
        pane.send_paste(text).ok();
        pane.writer().write_all(b"\r").ok();
    }
}

/// Build the shared composer/strip box-model element: a header line, one line
/// per visible body row, and a footer hint line.
fn build_composer_element(
    font: &Rc<LoadedFont>,
    header: &str,
    body_rows: &[String],
    footer: &str,
    fg: InheritableColor,
    bg_color: LinearRgba,
    border_color: LinearRgba,
    min_width_px: f32,
) -> Element {
    let line = |text: String| {
        Element::new(font, ElementContent::Text(text))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: LinearRgba::TRANSPARENT.into(),
                text: fg.clone(),
            })
            .display(DisplayType::Block)
    };

    let mut children = Vec::with_capacity(body_rows.len() + 2);
    children.push(line(header.to_string()));
    for row in body_rows {
        children.push(line(row.clone()));
    }
    children.push(line(footer.to_string()));

    Element::new(font, ElementContent::Children(children))
        .colors(ElementColors {
            border: BorderColor::new(border_color),
            bg: bg_color.into(),
            text: fg,
        })
        .margin(BoxDimension {
            left: Dimension::Cells(0.25),
            right: Dimension::Cells(0.25),
            top: Dimension::Cells(0.25),
            bottom: Dimension::Cells(0.25),
        })
        .padding(BoxDimension {
            left: Dimension::Cells(0.5),
            right: Dimension::Cells(0.5),
            top: Dimension::Cells(0.25),
            bottom: Dimension::Cells(0.25),
        })
        .border(BoxDimension::new(Dimension::Pixels(1.)))
        .border_corners(Some(Corners {
            top_left: SizedPoly {
                width: Dimension::Cells(0.25),
                height: Dimension::Cells(0.25),
                poly: TOP_LEFT_ROUNDED_CORNER,
            },
            top_right: SizedPoly {
                width: Dimension::Cells(0.25),
                height: Dimension::Cells(0.25),
                poly: TOP_RIGHT_ROUNDED_CORNER,
            },
            bottom_left: SizedPoly {
                width: Dimension::Cells(0.25),
                height: Dimension::Cells(0.25),
                poly: BOTTOM_LEFT_ROUNDED_CORNER,
            },
            bottom_right: SizedPoly {
                width: Dimension::Cells(0.25),
                height: Dimension::Cells(0.25),
                poly: BOTTOM_RIGHT_ROUNDED_CORNER,
            },
        }))
        .min_width(Some(Dimension::Pixels(min_width_px.max(0.))))
}

impl Modal for Composer {
    fn perform_assignment(&self, assignment: &KeyAssignment, term_window: &mut TermWindow) -> bool {
        // Route paste into the composer buffer rather than the pane.
        if let KeyAssignment::PasteFrom(source) = assignment {
            self.request_paste(term_window, *source);
            return true;
        }
        false
    }

    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        use KeyModifiers as M;

        match (key, mods) {
            (KeyCode::Escape, M::NONE) => {
                term_window.cancel_modal();
                return Ok(true);
            }
            (KeyCode::Enter, M::CTRL) => {
                self.submit(term_window);
                return Ok(true);
            }
            (KeyCode::Enter, M::NONE) | (KeyCode::Enter, M::SHIFT) => {
                self.reset_confirm();
                self.buffer.borrow_mut().newline();
            }
            (KeyCode::Backspace, M::NONE) => {
                self.reset_confirm();
                self.buffer.borrow_mut().backspace();
            }
            (KeyCode::Delete, M::NONE) => {
                self.reset_confirm();
                self.buffer.borrow_mut().delete_forward();
            }
            (KeyCode::LeftArrow, M::NONE) => self.buffer.borrow_mut().move_left(),
            (KeyCode::RightArrow, M::NONE) => self.buffer.borrow_mut().move_right(),
            (KeyCode::UpArrow, M::NONE) => self.buffer.borrow_mut().move_up(),
            (KeyCode::DownArrow, M::NONE) => self.buffer.borrow_mut().move_down(),
            (KeyCode::Home, M::NONE) => self.buffer.borrow_mut().move_home(),
            (KeyCode::End, M::NONE) => self.buffer.borrow_mut().move_end(),
            (KeyCode::UpArrow, M::ALT) => self.recall_prev(term_window),
            (KeyCode::DownArrow, M::ALT) => self.recall_next(term_window),
            (KeyCode::Char('u'), M::CTRL) => {
                self.reset_confirm();
                self.buffer.borrow_mut().clear();
            }
            (KeyCode::Char('d'), M::ALT) | (KeyCode::Char('D'), M::ALT) => {
                self.reset_confirm();
                self.insert_cwd(term_window);
            }
            (KeyCode::Char('s'), M::ALT) | (KeyCode::Char('S'), M::ALT) => {
                self.reset_confirm();
                self.insert_selection(term_window);
            }
            (KeyCode::Char(c), M::NONE) | (KeyCode::Char(c), M::SHIFT) => {
                if c == '\r' || c == '\n' {
                    self.reset_confirm();
                    self.buffer.borrow_mut().newline();
                } else if c.is_control() {
                    return Ok(false);
                } else {
                    self.reset_confirm();
                    self.buffer.borrow_mut().insert_char(c);
                }
            }
            _ => return Ok(false),
        }

        term_window.invalidate_modal();
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let element = self.compute(term_window)?;
            self.element.borrow_mut().replace(element);
        }
        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}

impl TermWindow {
    /// The docked input strip is active (reserving rows and rendering) only
    /// when the rich-input feature + `docked` are enabled AND the active pane
    /// has been toggled on via the agent-toolbelt button. It is not persistent
    /// chrome: it appears only after the user activates it on an agent pane.
    pub fn docked_input_active(&self) -> bool {
        if !(self.config.rich_input.enabled && self.config.rich_input.docked) {
            return false;
        }
        match self.get_active_pane_or_overlay() {
            Some(pane) => self.docked_input.enabled_panes.contains(&pane.pane_id()),
            None => false,
        }
    }

    /// Re-run layout so the reserved bottom rows are added/removed after the
    /// strip is toggled for a pane. Mirrors `finish_sidebar_resize`.
    fn relayout_for_docked_input(&mut self) {
        if let Some(window) = self.window.as_ref().map(|w| w.clone()) {
            let dims = self.dimensions;
            self.apply_dimensions(&dims, None, &window);
        }
    }

    /// Toggle the docked strip on/off for a specific pane (the agent-toolbelt
    /// button target). Activating focuses it; deactivating releases focus.
    pub fn toggle_docked_input_pane(&mut self, pane_id: PaneId) {
        if self.docked_input.enabled_panes.contains(&pane_id) {
            self.docked_input.enabled_panes.remove(&pane_id);
            self.docked_input.focused = false;
        } else {
            self.docked_input.enabled_panes.insert(pane_id);
            self.docked_input.focused = true;
        }
        self.relayout_for_docked_input();
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Toggle-key entry point: activate the docked strip for the active pane,
    /// but only when it is a detected agent pane (the feature is agent-only).
    pub fn toggle_docked_input(&mut self) {
        if !(self.config.rich_input.enabled && self.config.rich_input.docked) {
            return;
        }
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };
        if !self.pane_is_agent(&pane) {
            return;
        }
        self.toggle_docked_input_pane(pane.pane_id());
    }

    /// True when the docked strip currently owns keyboard focus.
    pub fn docked_input_focused(&self) -> bool {
        self.docked_input_active() && self.docked_input.focused
    }

    /// Fixed pixel height reserved at the bottom of the terminal area for the
    /// docked strip (0 when inactive). Based on `dock_rows` so the reservation
    /// does not change as the user types (which would cause resize churn).
    pub fn docked_input_pixel_height(&self) -> f32 {
        if !self.docked_input_active() {
            return 0.0;
        }
        let cell_h = (self.render_metrics.cell_size.height as f32).max(1.0);
        let rows = self.config.rich_input.dock_rows.clamp(1, 12) as f32;
        // content rows + header + footer + border/padding slack.
        ((rows + 2.0) * cell_h + cell_h).ceil()
    }

    /// Y pixel coordinate of the top of the docked strip band, if active.
    /// Matches the positioning used by [`Self::paint_docked_input`].
    pub fn docked_input_band_top(&self) -> Option<f32> {
        if !self.docked_input_active() {
            return None;
        }
        let strip_h = self.docked_input_pixel_height();
        if strip_h <= 0.0 {
            return None;
        }
        let border = self.get_os_border();
        let bottom_bar_height =
            if self.show_tab_bar && !self.sidebar_is_active() && self.config.tab_bar_at_bottom {
                self.tab_bar_pixel_height().unwrap_or(0.)
            } else {
                0.
            };
        let usable_bottom =
            self.dimensions.pixel_height as f32 - border.bottom.get() as f32 - bottom_bar_height;
        Some(usable_bottom - strip_h)
    }

    /// Insert a (possibly composed/IME) string into the docked buffer.
    pub fn docked_input_insert_str(&mut self, s: &str) {
        self.docked_input.buffer.insert_text(s);
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    fn docked_input_submit(&mut self) {
        let text = self.docked_input.buffer.text();
        if text.trim().is_empty() {
            return;
        }
        let limit = self.config.rich_input.history_limit;
        push_history(&self.composer_history, limit, &text);
        send_text_to_active_pane(self, &text);
        // Keep focus so the user can keep composing, but reset the buffer.
        self.docked_input.buffer.clear();
        self.docked_input.history_pos = None;
    }

    /// Handle a key while the docked strip owns focus. Returns true when the
    /// event was consumed. Mirrors the modal composer's editing keys.
    pub fn docked_input_key_input(
        &mut self,
        key: ::termwiz::input::KeyCode,
        modifiers: ::window::Modifiers,
        context: &dyn WindowOps,
    ) -> bool {
        use ::termwiz::input::KeyCode as KC;
        use ::window::Modifiers as M;

        let ctrl = modifiers.contains(M::CTRL);
        let alt = modifiers.contains(M::ALT);

        match key {
            KC::Escape => {
                self.docked_input.focused = false;
            }
            KC::Enter if ctrl => {
                self.docked_input_submit();
            }
            KC::Enter => self.docked_input.buffer.newline(),
            KC::Backspace => self.docked_input.buffer.backspace(),
            KC::Delete => self.docked_input.buffer.delete_forward(),
            KC::LeftArrow if !alt => self.docked_input.buffer.move_left(),
            KC::RightArrow if !alt => self.docked_input.buffer.move_right(),
            KC::UpArrow if alt => {
                let hist = self.composer_history.borrow();
                history_recall_prev(
                    &mut self.docked_input.buffer,
                    &hist,
                    &mut self.docked_input.history_pos,
                );
            }
            KC::DownArrow if alt => {
                let hist = self.composer_history.borrow();
                history_recall_next(
                    &mut self.docked_input.buffer,
                    &hist,
                    &mut self.docked_input.history_pos,
                );
            }
            KC::UpArrow => self.docked_input.buffer.move_up(),
            KC::DownArrow => self.docked_input.buffer.move_down(),
            KC::Home => self.docked_input.buffer.move_home(),
            KC::End => self.docked_input.buffer.move_end(),
            KC::Char('u') if ctrl => self.docked_input.buffer.clear(),
            KC::Char('d') | KC::Char('D') if alt => {
                if let Some(path) = active_pane_cwd(self) {
                    self.docked_input.buffer.insert_text(&path);
                }
            }
            KC::Char('s') | KC::Char('S') if alt => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    let text = self.selection_text(&pane);
                    if !text.is_empty() {
                        self.docked_input.buffer.insert_text(&text);
                    }
                }
            }
            KC::Char(c) if !ctrl && !alt && !c.is_control() => {
                self.docked_input.buffer.insert_char(c);
            }
            _ => return false,
        }

        context.invalidate();
        true
    }

    /// Paint the docked input strip into the band reserved at the bottom of the
    /// terminal area for the active pane.
    pub fn paint_docked_input(&mut self) -> anyhow::Result<()> {
        if !self.docked_input_active() {
            return Ok(());
        }
        let strip_h = self.docked_input_pixel_height();
        if strip_h <= 0.0 {
            return Ok(());
        }

        let font = self
            .fonts
            .command_palette_font()
            .expect("to resolve command palette font");
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let cell_h = (metrics.cell_size.height as f32).max(1.0);
        let cell_w = self.render_metrics.cell_size.width as f32;

        let fg: InheritableColor = self.config.command_palette_fg_color.to_linear().into();
        let bg_color = self.config.command_palette_bg_color.to_linear();
        let focused = self.docked_input.focused;
        // A brighter border marks focus; dim when the terminal owns input.
        let border_color = if focused {
            self.config.command_palette_fg_color.to_linear()
        } else {
            bg_color
        };

        let max_rows = self.config.rich_input.dock_rows.clamp(1, 12);
        self.docked_input.max_visible_rows = max_rows;
        let body_rows = self.docked_input.buffer.display_rows(max_rows, focused);

        let header = if focused {
            "▌ Input (focused) — Ctrl+Enter send · Esc release"
        } else {
            "▌ Input — Ctrl+Shift+Space to focus"
        };
        let footer = {
            let buffer = &self.docked_input.buffer;
            format!(
                "Enter newline · Alt+D cwd · Alt+S selection   [{} lines, {} chars]",
                buffer.lines.len(),
                buffer.total_chars()
            )
        };

        let dimensions = self.dimensions;
        let (padding_left, padding_top) = self.padding_left_top();
        let border = self.get_os_border();
        let avail_pixel_width = self.terminal_size.cols as f32 * cell_w;

        let bottom_bar_height =
            if self.show_tab_bar && !self.sidebar_is_active() && self.config.tab_bar_at_bottom {
                self.tab_bar_pixel_height().unwrap_or(0.)
            } else {
                0.
            };

        let element = build_composer_element(
            &font,
            header,
            &body_rows,
            &footer,
            fg,
            bg_color,
            border_color,
            avail_pixel_width - 2. * cell_w,
        );

        // The band sits just below the (already shrunken) terminal viewport.
        let usable_bottom =
            dimensions.pixel_height as f32 - border.bottom.get() as f32 - bottom_bar_height;
        let top_pixel_y = (usable_bottom - strip_h).max(padding_top + border.top.get() as f32);

        let gl_state = self.render_state.as_ref().unwrap();
        let computed = self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: cell_h,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: cell_w,
                },
                bounds: euclid::rect(padding_left, top_pixel_y, avail_pixel_width, strip_h),
                metrics: &metrics,
                gl_state,
                zindex: 10,
            },
            &element,
        )?;

        let mut ui_items = computed.ui_items();
        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(&computed, gl_state, None)?;
        self.ui_items.append(&mut ui_items);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer() -> ComposerBuffer {
        ComposerBuffer::new()
    }

    #[test]
    fn insert_and_backspace_single_line() {
        let mut c = buffer();
        for ch in "hello".chars() {
            c.insert_char(ch);
        }
        assert_eq!(c.text(), "hello");
        assert_eq!(c.cursor_col, 5);
        c.backspace();
        assert_eq!(c.text(), "hell");
        assert_eq!(c.cursor_col, 4);
    }

    #[test]
    fn newline_splits_line_and_moves_cursor() {
        let mut c = buffer();
        for ch in "ab".chars() {
            c.insert_char(ch);
        }
        c.move_left();
        c.newline();
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.cursor_line, 1);
        assert_eq!(c.cursor_col, 0);
    }

    #[test]
    fn multiline_paste_normalizes_crlf() {
        let mut c = buffer();
        c.insert_text("one\r\ntwo\rthree");
        assert_eq!(c.text(), "one\ntwo\nthree");
        assert_eq!(c.lines.len(), 3);
        assert_eq!(c.cursor_line, 2);
        assert_eq!(c.cursor_col, char_len("three"));
    }

    #[test]
    fn backspace_at_line_start_joins_previous() {
        let mut c = buffer();
        c.insert_text("a\nb");
        c.cursor_line = 1;
        c.cursor_col = 0;
        c.backspace();
        assert_eq!(c.text(), "ab");
        assert_eq!(c.cursor_line, 0);
        assert_eq!(c.cursor_col, 1);
    }

    #[test]
    fn insert_handles_multibyte_chars() {
        let mut c = buffer();
        c.insert_text("héllo");
        c.move_left();
        c.insert_char('X');
        assert_eq!(c.text(), "héllX o".replace(' ', ""));
    }

    #[test]
    fn clear_resets_buffer() {
        let mut c = buffer();
        c.insert_text("some\ntext");
        c.clear();
        assert_eq!(c.text(), "");
        assert_eq!(c.lines.len(), 1);
        assert_eq!(c.cursor_line, 0);
    }

    #[test]
    fn set_text_places_cursor_at_end() {
        let mut c = buffer();
        c.set_text("line1\nline2");
        assert_eq!(c.cursor_line, 1);
        assert_eq!(c.cursor_col, char_len("line2"));
    }

    #[test]
    fn history_recall_walks_ring() {
        let mut c = buffer();
        let hist = vec!["first".to_string(), "second".to_string()];
        let mut pos = None;
        history_recall_prev(&mut c, &hist, &mut pos);
        assert_eq!(c.text(), "second");
        history_recall_prev(&mut c, &hist, &mut pos);
        assert_eq!(c.text(), "first");
        history_recall_next(&mut c, &hist, &mut pos);
        assert_eq!(c.text(), "second");
        history_recall_next(&mut c, &hist, &mut pos);
        assert_eq!(c.text(), "");
    }
}
