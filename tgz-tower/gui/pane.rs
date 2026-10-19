//! `TowerPane`: a mux pane with no terminal behind it.
//!
//! Being a real `Pane` buys the whole host for free: it sits in a tab, can be
//! split next to real terminals, gets keys, IME text, paste, mouse and wheel
//! through the normal pane paths, and closes like any other pane. Its lines
//! are blank; `glue::paint` draws the GUI over its rectangle instead.

use super::model::{empty_snapshot, Effect, Input, Key, Model};
use mux::domain::DomainId;
use mux::pane::{
    CachePolicy, CloseReason, ForEachPaneLogicalLine, LogicalLine, Pane, PaneId, WithPaneLines,
};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::{Mux, MuxNotification};
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use rangeset::RangeSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use termwiz::surface::{CursorVisibility, SequenceNo, SEQ_ZERO};
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{
    KeyCode, KeyModifiers, Line, MouseButton, MouseEvent, MouseEventKind, StableRowIndex,
    TerminalSize,
};

pub const TITLE: &str = "Tower";

pub struct TowerPane {
    id: PaneId,
    domain_id: DomainId,
    cwd: Option<Url>,
    pub model: Arc<Mutex<Model>>,
    size: Mutex<TerminalSize>,
    dead: AtomicBool,
    writer: Mutex<std::io::Sink>,
    effects: Mutex<Vec<Effect>>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TowerPane {
    pub fn new(domain_id: DomainId, size: TerminalSize, cwd: Option<Url>) -> Self {
        let workdir = cwd
            .as_ref()
            .and_then(|u| u.to_file_path().ok())
            .map(|p| p.display().to_string());
        Self {
            id: mux::pane::alloc_pane_id(),
            domain_id,
            cwd,
            model: Arc::new(Mutex::new({
                let mut m = Model::new(empty_snapshot());
                m.workdir = workdir;
                m
            })),
            size: Mutex::new(size),
            dead: AtomicBool::new(false),
            writer: Mutex::new(std::io::sink()),
            effects: Mutex::new(Vec::new()),
        }
    }

    pub fn take_effects(&self) -> Vec<Effect> {
        std::mem::take(&mut *self.effects.lock())
    }

    pub fn repaint(&self) {
        Mux::notify_from_any_thread(MuxNotification::PaneOutput(self.id));
    }

    fn feed(&self, input: Input) {
        let fx = self.model.lock().apply(input, now_ms());
        if fx.is_empty() {
            return;
        }
        let mut repaint = false;
        let mut queued = self.effects.lock();
        for e in fx {
            match e {
                Effect::Repaint => repaint = true,
                other => {
                    queued.push(other);
                    repaint = true;
                }
            }
        }
        drop(queued);
        if repaint {
            self.repaint();
        }
    }

    fn blank_lines(&self, lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        let size = *self.size.lock();
        let rows = size.rows as StableRowIndex;
        let start = lines.start.max(0).min(rows);
        let end = lines.end.max(start).min(rows);
        let out = (start..end)
            .map(|_| Line::with_width(size.cols, SEQ_ZERO))
            .collect();
        (start, out)
    }
}

/// Map a terminal key to a Tower key; `None` lets nothing happen.
pub fn map_key(key: KeyCode, mods: KeyModifiers) -> Option<Key> {
    let mods = mods.remove_positional_mods();
    let plain = mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT;
    let ctrl = mods.contains(KeyModifiers::CTRL) || mods.contains(KeyModifiers::SUPER);
    Some(match key {
        KeyCode::Escape => Key::Escape,
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => Key::BackTab,
        KeyCode::Tab => Key::Tab,
        KeyCode::Enter => Key::Enter { ctrl },
        KeyCode::Char('\r') | KeyCode::Char('\n') => Key::Enter { ctrl },
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Char('\u{8}') | KeyCode::Char('\u{7f}') => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::LeftArrow | KeyCode::ApplicationLeftArrow => Key::Left,
        KeyCode::RightArrow | KeyCode::ApplicationRightArrow => Key::Right,
        KeyCode::UpArrow | KeyCode::ApplicationUpArrow => Key::Up,
        KeyCode::DownArrow | KeyCode::ApplicationDownArrow => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Char('a') | KeyCode::Char('A') if mods == KeyModifiers::CTRL => Key::Home,
        KeyCode::Char('e') | KeyCode::Char('E') if mods == KeyModifiers::CTRL => Key::End,
        KeyCode::Char(c) if plain && !c.is_control() => Key::Char(c),
        _ => return None,
    })
}

/// Map a pane mouse event to window-pixel input given the frame geometry.
pub fn map_mouse(ev: &MouseEvent, origin: (f32, f32), cell: (f32, f32)) -> Option<Input> {
    let x = origin.0 + ev.x as f32 * cell.0 + ev.x_pixel_offset as f32;
    let y = origin.1 + ev.y as f32 * cell.1 + ev.y_pixel_offset as f32;
    match (ev.kind, ev.button) {
        (_, MouseButton::WheelUp(n)) => Some(Input::Wheel {
            x,
            y,
            dx: 0.0,
            dy: n as f32,
        }),
        (_, MouseButton::WheelDown(n)) => Some(Input::Wheel {
            x,
            y,
            dx: 0.0,
            dy: -(n as f32),
        }),
        (_, MouseButton::WheelLeft(n)) => Some(Input::Wheel {
            x,
            y,
            dx: n as f32,
            dy: 0.0,
        }),
        (_, MouseButton::WheelRight(n)) => Some(Input::Wheel {
            x,
            y,
            dx: -(n as f32),
            dy: 0.0,
        }),
        (MouseEventKind::Press, MouseButton::Left) => Some(Input::Down { x, y }),
        (MouseEventKind::Release, MouseButton::Left) => Some(Input::Up { x, y }),
        (MouseEventKind::Move, _) => Some(Input::Move { x, y }),
        _ => None,
    }
}

impl Pane for TowerPane {
    fn pane_id(&self) -> PaneId {
        self.id
    }

    fn get_cursor_position(&self) -> StableCursorPosition {
        // The IME candidate window follows the caret of the focused field.
        let (x, y) = self.model.lock().caret_cell.unwrap_or((0, 0));
        StableCursorPosition {
            x,
            y,
            shape: Default::default(),
            visibility: CursorVisibility::Hidden,
        }
    }

    fn get_current_seqno(&self) -> SequenceNo {
        SEQ_ZERO
    }

    fn get_changed_since(
        &self,
        _lines: Range<StableRowIndex>,
        _seqno: SequenceNo,
    ) -> RangeSet<StableRowIndex> {
        RangeSet::new()
    }

    fn get_lines(&self, lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        self.blank_lines(lines)
    }

    fn with_lines_mut(&self, lines: Range<StableRowIndex>, with_lines: &mut dyn WithPaneLines) {
        let (first, mut lines) = self.blank_lines(lines);
        let mut refs: Vec<&mut Line> = lines.iter_mut().collect();
        with_lines.with_lines_mut(first, &mut refs);
    }

    fn for_each_logical_line_in_stable_range_mut(
        &self,
        _lines: Range<StableRowIndex>,
        _for_line: &mut dyn ForEachPaneLogicalLine,
    ) {
    }

    fn get_logical_lines(&self, _lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        vec![]
    }

    fn get_dimensions(&self) -> RenderableDimensions {
        let size = *self.size.lock();
        RenderableDimensions {
            cols: size.cols,
            viewport_rows: size.rows,
            // Equal to the viewport: no scrollback, so no scrollbar.
            scrollback_rows: size.rows,
            physical_top: 0,
            scrollback_top: 0,
            dpi: size.dpi,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
            reverse_video: false,
        }
    }

    fn get_title(&self) -> String {
        TITLE.to_string()
    }

    fn send_paste(&self, text: &str) -> anyhow::Result<()> {
        // Into the focused field; ignored otherwise (`cli send-text` too).
        self.feed(Input::Paste(text.to_string()));
        Ok(())
    }

    fn send_composed_text(&self, text: &str) -> anyhow::Result<()> {
        self.feed(Input::Text(text.to_string()));
        Ok(())
    }

    fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
        Ok(None)
    }

    fn writer(&self) -> MappedMutexGuard<'_, dyn std::io::Write> {
        MutexGuard::map(self.writer.lock(), |w| {
            let w: &mut dyn std::io::Write = w;
            w
        })
    }

    fn resize(&self, size: TerminalSize) -> anyhow::Result<()> {
        *self.size.lock() = size;
        Ok(())
    }

    fn key_down(&self, key: KeyCode, mods: KeyModifiers) -> anyhow::Result<()> {
        if let Some(key) = map_key(key, mods) {
            self.feed(Input::Key(key));
        }
        Ok(())
    }

    fn key_up(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        Ok(())
    }

    fn mouse_event(&self, event: MouseEvent) -> anyhow::Result<()> {
        let (origin, cell) = {
            let m = self.model.lock();
            (m.frame.origin, m.frame.cell)
        };
        if let Some(input) = map_mouse(&event, origin, cell) {
            self.feed(input);
        }
        Ok(())
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Relaxed)
    }

    fn kill(&self) {
        self.dead.store(true, Ordering::Relaxed);
    }

    fn palette(&self) -> ColorPalette {
        config::configuration().resolved_palette.clone().into()
    }

    fn domain_id(&self) -> DomainId {
        self.domain_id
    }

    fn can_close_without_prompting(&self, _reason: CloseReason) -> bool {
        true
    }

    fn is_mouse_grabbed(&self) -> bool {
        true
    }

    fn is_alt_screen_active(&self) -> bool {
        false
    }

    fn get_current_working_dir(&self, _policy: CachePolicy) -> Option<Url> {
        self.cwd.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_navigation() {
        assert_eq!(
            map_key(KeyCode::Char('3'), KeyModifiers::NONE),
            Some(Key::Char('3'))
        );
        assert_eq!(
            map_key(KeyCode::Tab, KeyModifiers::SHIFT),
            Some(Key::BackTab)
        );
        assert_eq!(
            map_key(KeyCode::Escape, KeyModifiers::NONE),
            Some(Key::Escape)
        );
        assert_eq!(
            map_key(KeyCode::Char('r'), KeyModifiers::CTRL),
            None,
            "chords are not ours"
        );
    }

    #[test]
    fn mouse_maps_cells_plus_offsets_to_window_pixels() {
        let ev = MouseEvent {
            kind: MouseEventKind::Press,
            x: 3,
            y: 2,
            x_pixel_offset: 4,
            y_pixel_offset: 5,
            button: MouseButton::Left,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            map_mouse(&ev, (100.0, 50.0), (10.0, 20.0)),
            Some(Input::Down { x: 134.0, y: 95.0 })
        );
        let wheel = MouseEvent {
            button: MouseButton::WheelDown(2),
            ..ev
        };
        assert!(matches!(
            map_mouse(&wheel, (0.0, 0.0), (1.0, 1.0)),
            Some(Input::Wheel { dy, .. }) if dy == -2.0
        ));
    }
}
