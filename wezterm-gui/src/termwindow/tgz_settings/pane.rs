//! `SettingsPane`: a mux pane with no terminal behind it, and the state and
//! behaviour of the Settings tab.
//!
//! Being a real pane puts Settings in a tab like anything else: it shows in
//! the sidebar, can sit in a split, takes keys and the wheel through the
//! normal pane paths and closes like any other pane. Its lines are blank;
//! `paint.rs` draws the controls over its rectangle, and those are ordinary
//! chrome hit items, so clicks follow the same rules as the sidebar's.
//!
//! Everything a click or a key does is decided here, in [`SettingsState`],
//! so the mouse and the keyboard cannot drift apart.

use super::catalog::{in_section, Kind, Section, Value, CATALOG};
use super::layouts::{self, DraftField, LayoutDraft};
use super::lua_edit::{self, KeyLine, LuaValue, PathStatus};
use super::store;
use config::keyassignment::KeyAssignment;
use config::Config;
use mux::domain::DomainId;
use mux::pane::{
    CachePolicy, CloseReason, ForEachPaneLogicalLine, LogicalLine, Pane, PaneId, WithPaneLines,
};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::{Mux, MuxNotification};
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use rangeset::RangeSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use termwiz::surface::{CursorVisibility, SequenceNo, SEQ_ZERO};
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{
    KeyCode, KeyModifiers, Line, MouseButton, MouseEvent, StableRowIndex, TerminalSize,
};

pub(crate) const TITLE: &str = "Settings";

/// Most entries a picker shows at once.
pub(crate) const PICKER_ROWS: usize = 10;

/// A control of the Settings tab, as a hit item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsHit {
    /// A section in the left column, by index into `Section::ALL`.
    Section(usize),
    /// A switch, by catalog index.
    Toggle(usize),
    /// One variant of a choice: catalog index, variant index.
    Choose(usize, usize),
    /// One step down (`-1`) or up (`1`).
    Step(usize, i8),
    /// A text field or the field of a picker.
    Field(usize),
    /// An entry of the open picker, by its place among the matches.
    Pick(usize),
    /// A swatch of the open colour grid, by index into `color_swatches`.
    Swatch(usize),
    /// A control of the Layouts section.
    Layout(LayoutHit),
    /// The search field at the top.
    Search,
    /// Open the config file in the system's editor for it.
    OpenFile,
    /// A row of the Shortcuts list, by its place in the list: click to give
    /// the action a new key.
    Shortcut(usize),
}

/// A control of the Layouts section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutHit {
    /// Edit `tgz_layouts[i]`.
    Open(usize),
    New,
    /// Type into a field of the form.
    Field(DraftField),
    /// Choose `PaneLayout::ALL[i]`.
    Preset(usize),
    /// Choose `layouts::START_IN[i]`.
    StartIn(usize),
    RemovePane(usize),
    AddPane,
    Save,
    Cancel,
    Delete,
}

/// One row of the Shortcuts list, as the last paint saw it.
#[derive(Clone, Debug)]
pub(crate) struct ShortcutRow {
    pub action: KeyAssignment,
    /// Settings changed this action's keys (its lines are in the block).
    pub custom: bool,
    /// The config file binds the action itself, outside the block: change
    /// it there.
    pub locked: bool,
}

/// The tag that marks an action's lines in the keys block.
pub(crate) fn shortcut_tag(action: &KeyAssignment) -> String {
    format!("{action:?}")
}

/// A Shortcuts row waiting for the keys to bind.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Capture {
    pub row: usize,
    /// Keys already bound to something else, held until Enter confirms.
    pub pending: Option<PendingKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingKey {
    pub key: ::window::KeyCode,
    pub mods: ::window::Modifiers,
    /// What the keys do now, as the palette names it.
    pub replaces: String,
}

/// What a key pressed during capture asks for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CaptureOutcome {
    /// Nothing to write (cancelled, waiting for confirmation, a lone
    /// modifier).
    Nothing,
    /// Bind `action` to these keys in place of its current ones.
    Bind {
        action: KeyAssignment,
        key: ::window::KeyCode,
        mods: ::window::Modifiers,
    },
    /// Leave `action` without keys.
    Unbind { action: KeyAssignment },
}

/// Where each catalog entry stands in the config file, and when that was
/// read. `None` inside: the file has a shape Settings cannot edit.
pub(crate) struct Statuses {
    pub read_at: Instant,
    pub by_setting: Option<Vec<PathStatus>>,
}

/// The list under a font or colour scheme field.
pub(crate) struct Picker {
    pub items: Arc<Vec<String>>,
    /// The list is still being gathered (installed fonts).
    pub loading: bool,
    /// Highlighted match.
    pub selected: usize,
    /// First visible match.
    pub scroll: usize,
}

/// The field being typed into.
pub(crate) struct Editing {
    /// Catalog index.
    pub index: usize,
    pub buffer: String,
    pub picker: Option<Picker>,
    /// A colour field's grid of swatches is open; the highlighted one.
    pub swatch: Option<usize>,
}

impl Editing {
    /// The picker's entries that match what was typed, best first.
    pub(crate) fn matches(&self) -> Vec<&str> {
        match &self.picker {
            Some(picker) => filter_items(&picker.items, &self.buffer),
            None => vec![],
        }
    }
}

/// `items` containing `query`, ignoring case: those that start with it
/// first, each group in the list's own order. Everything for no query.
pub(crate) fn filter_items<'a>(items: &'a [String], query: &str) -> Vec<&'a str> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return items.iter().map(String::as_str).collect();
    }
    let (mut starts, mut contains) = (Vec::new(), Vec::new());
    for item in items {
        let lower = item.to_lowercase();
        if lower.starts_with(&query) {
            starts.push(item.as_str());
        } else if lower.contains(&query) {
            contains.push(item.as_str());
        }
    }
    starts.extend(contains);
    starts
}

/// Installed font families, gathered once on a worker thread: asking the
/// system for every font takes a moment.
static FONT_FAMILIES: Mutex<Option<Arc<Vec<String>>>> = parking_lot::const_mutex(None);
static FONTS_LOADING: AtomicBool = AtomicBool::new(false);

/// The installed font families when they are known. Otherwise starts
/// gathering them and repaints `pane_id` when they are.
pub(crate) fn font_families(pane_id: PaneId) -> Option<Arc<Vec<String>>> {
    if let Some(families) = FONT_FAMILIES.lock().as_ref() {
        return Some(Arc::clone(families));
    }
    if !FONTS_LOADING.swap(true, Ordering::SeqCst) {
        let locator = config::configuration().font_locator;
        std::thread::spawn(move || {
            let mut families: Vec<String> = wezterm_font::locator::new_locator(locator)
                .enumerate_all_fonts()
                .unwrap_or_else(|err| {
                    log::warn!("settings: listing fonts failed: {err:#}");
                    vec![]
                })
                .into_iter()
                .map(|font| font.names().family.clone())
                // Hidden system fonts start with a dot.
                .filter(|family| !family.is_empty() && !family.starts_with('.'))
                .collect();
            // Ships inside the app, so the system does not list it.
            families.push("JetBrains Mono".to_string());
            families.sort_by_key(|family| family.to_lowercase());
            families.dedup();
            *FONT_FAMILIES.lock() = Some(Arc::new(families));
            FONTS_LOADING.store(false, Ordering::SeqCst);
            Mux::notify_from_any_thread(MuxNotification::PaneOutput(pane_id));
        });
    }
    None
}

/// The picker entry that stands for no colour scheme.
pub(crate) const NO_SCHEME: &str = "None (your own colours)";

/// Every colour scheme name, the user's and the built-in ones, after the
/// entry for none.
fn scheme_names(config: &Config) -> Vec<String> {
    let mut names: Vec<String> = config
        .color_schemes
        .keys()
        .chain(config::COLOR_SCHEMES.keys())
        .cloned()
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names.insert(0, NO_SCHEME.to_string());
    names
}

pub(crate) struct SettingsState {
    /// Index into `Section::ALL`.
    pub section: usize,
    /// First visible row of the section.
    pub scroll: usize,
    /// Set by each paint: the furthest `scroll` may go, and how many rows
    /// fit.
    pub max_scroll: usize,
    pub visible_rows: usize,
    /// The row the keyboard is on, by its place in the section.
    pub focus: Option<usize>,
    pub editing: Option<Editing>,
    /// The last thing worth telling: text, and whether it is an error.
    pub message: Option<(String, bool)>,
    pub file: PathBuf,
    pub statuses: Option<Statuses>,
    /// The Shortcuts list as last painted.
    pub shortcuts: Vec<ShortcutRow>,
    /// The key bindings the block holds; read with `statuses`.
    pub key_lines: Option<Vec<KeyLine>>,
    /// A Shortcuts row waiting for its keys.
    pub capture: Option<Capture>,
    /// The layout open in the Layouts form.
    pub draft: Option<LayoutDraft>,
    /// Where the file sets `tgz_layouts` itself (outside the block): read
    /// with `statuses`. `Some(None)`: the form may write it.
    pub layouts_lock: Option<Option<usize>>,
    /// Rows of the Layouts list as last painted (layouts and "New").
    pub layout_rows: usize,
    /// What the search field holds; while it is not empty the rows are the
    /// matching settings of every section.
    pub search: String,
    /// The search field has the keyboard.
    pub search_focused: bool,
}

impl SettingsState {
    fn new() -> Self {
        Self {
            section: 0,
            scroll: 0,
            max_scroll: 0,
            visible_rows: 1,
            focus: None,
            editing: None,
            message: None,
            file: store::config_path(),
            statuses: None,
            shortcuts: Vec::new(),
            key_lines: None,
            capture: None,
            draft: None,
            layouts_lock: None,
            layout_rows: 0,
            search: String::new(),
            search_focused: false,
        }
    }

    /// The search is showing results instead of the chosen section.
    pub(crate) fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }

    /// The catalog entries on show: the search's matches, else the
    /// section's settings.
    pub(crate) fn rows(&self) -> Vec<usize> {
        if self.searching() {
            super::catalog::search(&self.search)
        } else {
            in_section(self.current_section())
        }
    }

    /// The section's own list (Shortcuts, Layouts) is on show, not settings.
    pub(crate) fn list_section(&self) -> Option<Section> {
        match self.current_section() {
            section @ (Section::Shortcuts | Section::Layouts) if !self.searching() => Some(section),
            _ => None,
        }
    }

    /// Change what the search holds; the rows start over at the top.
    fn edit_search(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.search);
        self.scroll = 0;
        self.focus = None;
        self.editing = None;
    }

    /// Keys while the search field has the keyboard. `true` when taken.
    fn search_key(&mut self, key: &KeyCode, mods: KeyModifiers) -> bool {
        let plain = mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT;
        match key {
            KeyCode::Escape => {
                if self.search.is_empty() {
                    self.search_focused = false;
                } else {
                    self.edit_search(String::clear);
                }
            }
            KeyCode::Backspace | KeyCode::Char('\u{8}') | KeyCode::Char('\u{7f}') => {
                self.edit_search(|text| {
                    text.pop();
                });
            }
            // On to the results.
            KeyCode::Enter
            | KeyCode::Char('\r')
            | KeyCode::Char('\n')
            | KeyCode::DownArrow
            | KeyCode::ApplicationDownArrow => {
                self.search_focused = false;
                if !self.rows().is_empty() {
                    self.focus = Some(0);
                }
            }
            KeyCode::Char(c) if plain && !c.is_control() => {
                let c = *c;
                self.edit_search(|text| text.push(c));
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn current_section(&self) -> Section {
        Section::ALL[self.section.min(Section::ALL.len() - 1)]
    }

    fn status(&self, index: usize) -> Option<&PathStatus> {
        self.statuses
            .as_ref()
            .and_then(|s| s.by_setting.as_ref())
            .and_then(|all| all.get(index))
    }

    /// Whether the control of catalog entry `index` can be used: the file
    /// can be edited and does not compute this setting itself.
    fn usable(&self, index: usize) -> bool {
        match self.status(index) {
            Some(PathStatus::Locked { .. }) | None => false,
            Some(_) => true,
        }
    }

    /// Write `value` for catalog entry `index` to the config file.
    pub(crate) fn change(&mut self, index: usize, value: Value) {
        let Some(setting) = CATALOG.get(index) else {
            return;
        };
        self.file = store::config_path();
        let empty = matches!(&value, Value::Text(text) if text.is_empty());
        let in_file_line = match self.status(index) {
            Some(PathStatus::InPlace { line }) => Some(*line),
            _ => None,
        };

        let result = if empty && matches!(setting.kind, Kind::Font) {
            self.message = Some(("A font needs a name.".to_string(), true));
            return;
        } else if !empty
            && matches!(setting.kind, Kind::Color)
            && !matches!(&value, Value::Text(text) if super::catalog::is_color(text))
        {
            self.message = Some((
                "Not a colour. Use #rrggbb, e.g. #58a6ff; clear the field for the default."
                    .to_string(),
                true,
            ));
            return;
        } else if empty {
            match in_file_line {
                Some(line) => {
                    self.message = Some((
                        format!("Remove line {line} of the config file to clear this."),
                        true,
                    ));
                    return;
                }
                None => store::apply(&self.file, setting.path, None),
            }
        } else {
            let config = config::configuration();
            let (path, lua) = setting.lua_write(&value, &config, in_file_line.is_some());
            store::apply(&self.file, path, Some(&lua))
        };

        self.message = match result {
            Ok(()) => None,
            Err(err) => Some((err.message(&self.file), true)),
        };
        // The file changed (or did not): read where things stand again.
        self.statuses = None;
        self.key_lines = None;
        self.layouts_lock = None;
    }

    /// A control of the Layouts section.
    fn layout_hit(&mut self, hit: LayoutHit, config: &Config) {
        if let Some(Some(line)) = self.layouts_lock {
            if hit != LayoutHit::Cancel {
                self.message = Some((
                    format!(
                        "Your config sets tgz_layouts itself on line {line}. Change them there."
                    ),
                    true,
                ));
                return;
            }
        }
        if let LayoutHit::Open(i) = hit {
            self.draft = config.tgz_layouts.get(i).map(LayoutDraft::edit);
            self.focus = None;
            return;
        }
        if hit == LayoutHit::New {
            self.draft = Some(LayoutDraft::new_layout(&config.tgz_layouts));
            self.focus = None;
            return;
        }
        match hit {
            LayoutHit::Save => return self.save_draft(config),
            LayoutHit::Delete => return self.delete_draft(config),
            LayoutHit::Cancel => {
                self.draft = None;
                return;
            }
            _ => {}
        }
        let Some(draft) = self.draft.as_mut() else {
            return;
        };
        match hit {
            LayoutHit::Field(field) => draft.typing = Some(field),
            LayoutHit::Preset(i) => {
                if let Some(layout) = config::keyassignment::PaneLayout::ALL.get(i) {
                    draft.layout = *layout;
                }
            }
            LayoutHit::StartIn(i) => {
                if let Some(cwd) = layouts::START_IN.get(i) {
                    draft.cwd = cwd.clone();
                }
            }
            LayoutHit::RemovePane(i) => draft.remove_pane(i),
            LayoutHit::AddPane => draft.add_pane(),
            _ => {}
        }
    }

    fn save_draft(&mut self, config: &Config) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let saved = match layouts::apply_draft(&config.tgz_layouts, &draft) {
            Ok(saved) => saved,
            Err(why) => {
                self.message = Some((why, true));
                return;
            }
        };
        let name = draft.name.trim().to_string();
        let rename = draft
            .original
            .filter(|old| *old != name)
            .map(|old| (old, name));
        if self.write_layouts(&saved, rename, None) {
            self.draft = None;
        }
    }

    fn delete_draft(&mut self, config: &Config) {
        let Some(original) = self.draft.as_ref().and_then(|d| d.original.clone()) else {
            self.draft = None;
            return;
        };
        let kept: Vec<config::TgzPaneLayout> = config
            .tgz_layouts
            .iter()
            .filter(|layout| layout.name != original)
            .cloned()
            .collect();
        if self.write_layouts(&kept, None, Some(original)) {
            self.draft = None;
        }
    }

    /// Write `layouts` as the whole of `tgz_layouts`, moving or dropping the
    /// keys of a renamed or deleted one. `true` when it was written.
    fn write_layouts(
        &mut self,
        layouts: &[config::TgzPaneLayout],
        rename: Option<(String, String)>,
        deleted: Option<String>,
    ) -> bool {
        self.file = store::config_path();
        let rhs = layouts::layouts_to_lua(layouts);
        let result = store::edit(&self.file, |text| {
            let mut out = if layouts.is_empty() {
                lua_edit::unset_value(text, layouts::PATH)?.unwrap_or_else(|| text.to_string())
            } else {
                lua_edit::set_value(text, layouts::PATH, &LuaValue::Raw(rhs.clone()))?
            };
            if let Some((old, new)) = &rename {
                out = layouts::move_layout_keys(&out, old, Some(new))?;
            }
            if let Some(old) = &deleted {
                out = layouts::move_layout_keys(&out, old, None)?;
            }
            Ok(Some(out))
        });
        let written = result.is_ok();
        self.finish_key_write(result);
        written
    }

    /// Keys for the Layouts section. `true` when taken.
    fn layout_key(&mut self, key: &KeyCode, mods: KeyModifiers, config: &Config) -> bool {
        let plain = mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT;
        let enter = matches!(
            key,
            KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n')
        );
        let backspace = matches!(
            key,
            KeyCode::Backspace | KeyCode::Char('\u{8}') | KeyCode::Char('\u{7f}')
        );
        if let Some(draft) = self.draft.as_mut() {
            if let Some(field) = draft.typing {
                match key {
                    KeyCode::Escape => draft.typing = None,
                    _ if enter => draft.typing = None,
                    _ if backspace => {
                        if let Some(text) = draft.field_mut(field) {
                            text.pop();
                        }
                    }
                    KeyCode::Char(c) if plain && !c.is_control() => {
                        if let Some(text) = draft.field_mut(field) {
                            text.push(*c);
                        }
                    }
                    _ => {}
                }
                return true;
            }
            match key {
                KeyCode::Escape => self.draft = None,
                _ if enter => self.save_draft(config),
                _ => return false,
            }
            return true;
        }
        if enter {
            if let Some(row) = self.focus {
                let hit = if row < config.tgz_layouts.len() {
                    LayoutHit::Open(row)
                } else {
                    LayoutHit::New
                };
                self.layout_hit(hit, config);
            }
            return true;
        }
        false
    }

    /// Start waiting for the keys of Shortcuts row `row`.
    fn start_capture(&mut self, row: usize) {
        let Some(shortcut) = self.shortcuts.get(row) else {
            return;
        };
        self.focus = Some(row);
        if shortcut.locked {
            self.message = Some((
                "Your config file binds this itself in config.keys. Change it there.".to_string(),
                true,
            ));
            return;
        }
        self.capture = Some(Capture { row, pending: None });
    }

    /// Give Shortcuts row `row` its default keys back.
    fn reset_shortcut(&mut self, row: usize) {
        let Some(shortcut) = self.shortcuts.get(row) else {
            return;
        };
        if !shortcut.custom {
            return;
        }
        self.file = store::config_path();
        let result = store::apply_keys(&self.file, &shortcut_tag(&shortcut.action), &[]);
        self.finish_key_write(result);
    }

    /// Note how writing a key change went, and read the file again.
    pub(crate) fn finish_key_write(&mut self, result: Result<(), store::ApplyError>) {
        self.message = match result {
            Ok(()) => None,
            Err(err) => Some((err.message(&self.file), true)),
        };
        self.statuses = None;
        self.key_lines = None;
    }

    /// A key while a Shortcuts row is capturing. `conflict` names what the
    /// keys do now when that is something else; such keys wait for Enter.
    pub(crate) fn capture_key(
        &mut self,
        key: &::window::KeyCode,
        mods: ::window::Modifiers,
        conflict: impl FnOnce(&::window::KeyCode, ::window::Modifiers) -> Option<String>,
    ) -> CaptureOutcome {
        use ::window::{KeyCode as WK, Modifiers as WM};
        let Some(capture) = self.capture.clone() else {
            return CaptureOutcome::Nothing;
        };
        if key.is_modifier() {
            return CaptureOutcome::Nothing;
        }
        let Some(action) = self
            .shortcuts
            .get(capture.row)
            .map(|row| row.action.clone())
        else {
            self.capture = None;
            return CaptureOutcome::Nothing;
        };
        let mods = mods.remove_positional_mods() - WM::LEADER;
        let plain = mods == WM::NONE;
        let is = |c: char| matches!(key, WK::Char(k) if *k == c);
        if plain && is('\u{1b}') {
            // Esc backs out of a pending conflict first, then of capturing.
            match self.capture.as_mut() {
                Some(c) if c.pending.is_some() => c.pending = None,
                _ => self.capture = None,
            }
            return CaptureOutcome::Nothing;
        }
        if plain && (is('\r') || is('\n')) {
            if let Some(pending) = capture.pending {
                self.capture = None;
                return CaptureOutcome::Bind {
                    action,
                    key: pending.key,
                    mods: pending.mods,
                };
            }
        }
        if plain && (is('\u{8}') || is('\u{7f}')) {
            self.capture = None;
            return CaptureOutcome::Unbind { action };
        }
        match conflict(key, mods) {
            Some(replaces) => {
                self.capture = Some(Capture {
                    row: capture.row,
                    pending: Some(PendingKey {
                        key: key.clone(),
                        mods,
                        replaces,
                    }),
                });
                CaptureOutcome::Nothing
            }
            None => {
                self.capture = None;
                CaptureOutcome::Bind {
                    action,
                    key: key.clone(),
                    mods,
                }
            }
        }
    }

    fn open_file(&mut self) {
        if self.file.is_file() {
            wezterm_open_url::open_url(&self.file.to_string_lossy());
        } else {
            self.message = Some((
                "There is no config file yet. It is created with your first change.".to_string(),
                false,
            ));
        }
    }

    /// Start typing into the field of catalog entry `index`.
    fn open_field(&mut self, index: usize, config: &Config, pane_id: PaneId) {
        let Some(setting) = CATALOG.get(index) else {
            return;
        };
        if !self.usable(index) {
            return;
        }
        let current = match (setting.get)(config) {
            Value::Text(text) => text,
            _ => String::new(),
        };
        let (buffer, picker) = match setting.kind {
            // A picker starts empty so the whole list shows; what is set is
            // highlighted in it.
            Kind::Font => {
                let families = font_families(pane_id);
                (
                    String::new(),
                    Some(Picker {
                        loading: families.is_none(),
                        items: families.unwrap_or_default(),
                        selected: 0,
                        scroll: 0,
                    }),
                )
            }
            Kind::Scheme => (
                String::new(),
                Some(Picker {
                    loading: false,
                    items: Arc::new(scheme_names(config)),
                    selected: 0,
                    scroll: 0,
                }),
            ),
            _ => (current.clone(), None),
        };
        let swatch = matches!(setting.kind, Kind::Color).then(|| {
            super::catalog::color_swatches()
                .iter()
                .position(|hex| hex.eq_ignore_ascii_case(current.trim()))
                .unwrap_or(0)
        });
        let mut editing = Editing {
            index,
            buffer,
            picker,
            swatch,
        };
        let at = editing.matches().iter().position(|item| *item == current);
        if let (Some(at), Some(picker)) = (at, editing.picker.as_mut()) {
            picker.selected = at;
            picker.scroll = at.saturating_sub(PICKER_ROWS / 2);
        }
        self.editing = Some(editing);
    }

    /// Take what the open field holds: the highlighted match of a picker,
    /// else what was typed.
    fn commit_edit(&mut self) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let typed = editing.buffer.trim().to_string();
        let value = match &editing.picker {
            Some(picker) => {
                let matches = editing.matches();
                match matches.get(picker.selected) {
                    Some(item) if *item == NO_SCHEME => String::new(),
                    Some(item) => item.to_string(),
                    // An installed font the system did not list still works
                    // by name; a colour scheme has to exist.
                    None if matches!(CATALOG[editing.index].kind, Kind::Font)
                        && !typed.is_empty() =>
                    {
                        typed
                    }
                    None => {
                        self.message = Some(("Nothing matches that.".to_string(), true));
                        return;
                    }
                }
            }
            None => typed,
        };
        self.change(editing.index, Value::Text(value));
    }

    /// A left click on a control.
    pub(crate) fn hit(&mut self, hit: SettingsHit, config: &Config, pane_id: PaneId) {
        self.message = None;
        // A click anywhere but in the open field or its list abandons it.
        if !matches!(&hit, SettingsHit::Pick(_) | SettingsHit::Swatch(_))
            && !matches!((&hit, &self.editing), (SettingsHit::Field(index), Some(e)) if *index == e.index)
        {
            self.editing = None;
        }
        if !matches!(&hit, SettingsHit::Shortcut(_)) {
            self.capture = None;
        }
        self.search_focused = hit == SettingsHit::Search;
        match hit {
            SettingsHit::Search => {}
            SettingsHit::Shortcut(row) => {
                let again = self.capture.as_ref().is_some_and(|c| c.row == row);
                if again {
                    self.capture = None;
                } else {
                    self.start_capture(row);
                }
            }
            SettingsHit::Section(index) => self.show_section(index),
            SettingsHit::Toggle(index) => {
                if let Some(Value::Bool(on)) = CATALOG.get(index).map(|s| (s.get)(config)) {
                    self.change(index, Value::Bool(!on));
                }
            }
            SettingsHit::Choose(index, variant) => {
                if let Some(Kind::Choice(variants)) = CATALOG.get(index).map(|s| s.kind) {
                    if let Some(name) = variants().get(variant) {
                        self.change(index, Value::Text(name.to_string()));
                    }
                }
            }
            SettingsHit::Step(index, direction) => self.step(index, direction, config),
            SettingsHit::Field(index) => {
                if self.editing.is_none() {
                    self.open_field(index, config, pane_id);
                }
            }
            SettingsHit::Layout(hit) => self.layout_hit(hit, config),
            SettingsHit::Swatch(at) => {
                if let Some(hex) = super::catalog::color_swatches().get(at) {
                    if let Some(editing) = self.editing.as_mut() {
                        editing.buffer = hex.clone();
                    }
                    self.commit_edit();
                }
            }
            SettingsHit::Pick(at) => {
                if let Some(picker) = self.editing.as_mut().and_then(|e| e.picker.as_mut()) {
                    picker.selected = at;
                }
                self.commit_edit();
            }
            SettingsHit::OpenFile => self.open_file(),
        }
    }

    fn show_section(&mut self, index: usize) {
        self.section = index.min(Section::ALL.len() - 1);
        self.scroll = 0;
        self.focus = None;
        self.editing = None;
        self.capture = None;
        self.draft = None;
        self.search.clear();
        self.search_focused = false;
    }

    fn step(&mut self, index: usize, direction: i8, config: &Config) {
        if let Some(setting) = CATALOG.get(index) {
            let current = (setting.get)(config);
            if let Some(next) = setting.stepped(&current, direction) {
                self.change(index, next);
            }
        }
    }

    /// Move the picker's highlight by `delta`, keeping it in view.
    fn move_picker(&mut self, delta: isize) {
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let count = editing.matches().len();
        let Some(picker) = editing.picker.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        let selected = (picker.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        picker.selected = selected;
        if selected < picker.scroll {
            picker.scroll = selected;
        } else if selected >= picker.scroll + PICKER_ROWS {
            picker.scroll = selected + 1 - PICKER_ROWS;
        }
    }

    /// Move the colour grid's highlight by `delta` and put its colour in the
    /// field, so Enter takes it.
    fn move_swatch(&mut self, delta: isize) {
        let swatches = super::catalog::color_swatches();
        let Some(editing) = self.editing.as_mut() else {
            return;
        };
        let Some(at) = editing.swatch else {
            return;
        };
        let next = at as isize + delta;
        if next < 0 || next >= swatches.len() as isize {
            return;
        }
        editing.swatch = Some(next as usize);
        editing.buffer = swatches[next as usize].clone();
    }

    /// Move the keyboard's row by `delta`, scrolling it into view.
    fn move_focus(&mut self, delta: isize) {
        let count = match self.list_section() {
            Some(Section::Shortcuts) => self.shortcuts.len(),
            Some(_) if self.draft.is_some() => 0,
            Some(_) => self.layout_rows,
            None => self.rows().len(),
        };
        if count == 0 {
            self.scroll =
                (self.scroll as isize + delta).clamp(0, self.max_scroll as isize) as usize;
            return;
        }
        let focus = match self.focus {
            None if delta < 0 => count - 1,
            None => self.scroll.min(count - 1),
            Some(at) => (at as isize + delta).clamp(0, count as isize - 1) as usize,
        };
        self.focus = Some(focus);
        let visible = self.visible_rows.max(1);
        if focus < self.scroll {
            self.scroll = focus;
        } else if focus >= self.scroll + visible {
            self.scroll = focus + 1 - visible;
        }
    }

    /// A key press. Typing goes to the open field; otherwise the arrows
    /// walk the rows and change the one they are on.
    pub(crate) fn key(
        &mut self,
        key: KeyCode,
        mods: KeyModifiers,
        config: &Config,
        pane_id: PaneId,
    ) {
        let mods = mods.remove_positional_mods();
        let plain = mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT;
        let up = matches!(key, KeyCode::UpArrow | KeyCode::ApplicationUpArrow);
        let down = matches!(key, KeyCode::DownArrow | KeyCode::ApplicationDownArrow);
        let left = matches!(key, KeyCode::LeftArrow | KeyCode::ApplicationLeftArrow);
        let right = matches!(key, KeyCode::RightArrow | KeyCode::ApplicationRightArrow);
        let enter = matches!(
            key,
            KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n')
        );

        if self.list_section() == Some(Section::Layouts) && self.layout_key(&key, mods, config) {
            return;
        }
        if self.editing.as_ref().is_some_and(|e| e.swatch.is_some()) {
            let step = if left {
                Some(-1)
            } else if right {
                Some(1)
            } else if up {
                Some(-(super::catalog::SWATCH_COLUMNS as isize))
            } else if down {
                Some(super::catalog::SWATCH_COLUMNS as isize)
            } else {
                None
            };
            if let Some(step) = step {
                self.move_swatch(step);
                return;
            }
        }
        if self.editing.is_some() {
            match key {
                KeyCode::Escape => self.editing = None,
                _ if enter => self.commit_edit(),
                _ if up => self.move_picker(-1),
                _ if down => self.move_picker(1),
                KeyCode::PageUp => self.move_picker(-(PICKER_ROWS as isize)),
                KeyCode::PageDown => self.move_picker(PICKER_ROWS as isize),
                KeyCode::Backspace | KeyCode::Char('\u{8}') | KeyCode::Char('\u{7f}') => {
                    self.edit_buffer(|buffer| {
                        buffer.pop();
                    });
                }
                KeyCode::Char(c) if plain && !c.is_control() => {
                    self.edit_buffer(|buffer| buffer.push(c));
                }
                _ => {}
            }
            return;
        }

        self.message = None;
        // Letters start a search, as in the system's own settings; a field,
        // a capture or the Layouts form took its keys above.
        if !self.search_focused
            && self.draft.is_none()
            && matches!(key, KeyCode::Char(c) if c.is_alphanumeric())
            && (mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT)
        {
            self.search_focused = true;
        }
        if self.search_focused && self.search_key(&key, mods) {
            return;
        }
        if key == KeyCode::Escape && self.searching() && self.focus.is_none() {
            self.edit_search(String::clear);
            return;
        }
        let rows = self.rows();
        let focused = self.focus.and_then(|at| rows.get(at).copied());
        match key {
            _ if up => self.move_focus(-1),
            _ if down => self.move_focus(1),
            KeyCode::PageUp => self.move_focus(-(self.visible_rows.max(1) as isize)),
            KeyCode::PageDown => self.move_focus(self.visible_rows.max(1) as isize),
            KeyCode::Tab => {
                let count = Section::ALL.len();
                let next = if mods.contains(KeyModifiers::SHIFT) {
                    (self.section + count - 1) % count
                } else {
                    (self.section + 1) % count
                };
                self.show_section(next);
                if !self.rows().is_empty() {
                    self.focus = Some(0);
                }
            }
            KeyCode::Escape => self.focus = None,
            _ if self.list_section() == Some(Section::Shortcuts) => {
                let Some(row) = self.focus else {
                    return;
                };
                if enter || key == KeyCode::Char(' ') {
                    self.start_capture(row);
                } else if matches!(
                    key,
                    KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Char('\u{8}')
                        | KeyCode::Char('\u{7f}')
                ) {
                    self.reset_shortcut(row);
                }
            }
            _ => {
                let Some(index) = focused else {
                    return;
                };
                let setting = &CATALOG[index];
                let space = key == KeyCode::Char(' ');
                if let Some(PathStatus::Locked { .. }) = self.status(index) {
                    if enter {
                        self.open_file();
                    }
                    return;
                }
                if !self.usable(index) {
                    return;
                }
                match (setting.kind, (setting.get)(config)) {
                    (Kind::Toggle, Value::Bool(on)) => {
                        // Left is Off and Right is On, as they are drawn.
                        let want = if enter || space {
                            Some(!on)
                        } else if left {
                            Some(false)
                        } else if right {
                            Some(true)
                        } else {
                            None
                        };
                        if let Some(want) = want.filter(|want| *want != on) {
                            self.change(index, Value::Bool(want));
                        }
                    }
                    (Kind::Choice(_) | Kind::Int { .. } | Kind::Float { .. }, _) => {
                        if left {
                            self.step(index, -1, config);
                        } else if right {
                            self.step(index, 1, config);
                        }
                    }
                    (Kind::Text | Kind::Font | Kind::Scheme | Kind::Color, _) => {
                        if enter || space {
                            self.open_field(index, config, pane_id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Change what is typed in the open field; the picker starts over at
    /// its best match.
    fn edit_buffer(&mut self, edit: impl FnOnce(&mut String)) {
        if let Some(editing) = self.editing.as_mut() {
            edit(&mut editing.buffer);
            if let Some(picker) = editing.picker.as_mut() {
                picker.selected = 0;
                picker.scroll = 0;
            }
        }
    }

    /// The wheel: the open picker's list when there is one, else the rows.
    fn wheel(&mut self, delta: isize) {
        if let Some(editing) = self.editing.as_mut() {
            let count = editing.matches().len();
            if let Some(picker) = editing.picker.as_mut() {
                let furthest = count.saturating_sub(PICKER_ROWS) as isize;
                picker.scroll = (picker.scroll as isize + delta).clamp(0, furthest) as usize;
                return;
            }
        }
        self.scroll = (self.scroll as isize + delta).clamp(0, self.max_scroll as isize) as usize;
    }
}

pub(crate) struct SettingsPane {
    id: PaneId,
    domain_id: DomainId,
    cwd: Option<Url>,
    pub state: Arc<Mutex<SettingsState>>,
    size: Mutex<TerminalSize>,
    dead: AtomicBool,
    writer: Mutex<std::io::Sink>,
}

impl SettingsPane {
    pub(crate) fn new(domain_id: DomainId, size: TerminalSize, cwd: Option<Url>) -> Self {
        Self {
            id: mux::pane::alloc_pane_id(),
            domain_id,
            cwd,
            state: Arc::new(Mutex::new(SettingsState::new())),
            size: Mutex::new(size),
            dead: AtomicBool::new(false),
            writer: Mutex::new(std::io::sink()),
        }
    }

    fn repaint(&self) {
        Mux::notify_from_any_thread(MuxNotification::PaneOutput(self.id));
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

    /// Add composed or pasted text to the field being edited.
    fn insert(&self, text: &str) {
        let mut state = self.state.lock();
        let clean = || text.chars().filter(|c| !c.is_control());
        if state.editing.is_some() {
            state.edit_buffer(|buffer| buffer.extend(clean()));
            drop(state);
            self.repaint();
        } else if let Some(draft) = state.draft.as_mut() {
            if let Some(text_field) = draft.typing.and_then(|field| draft.field_mut(field)) {
                text_field.extend(clean());
                drop(state);
                self.repaint();
            }
        } else if state.search_focused {
            state.edit_search(|search| search.extend(clean()));
            drop(state);
            self.repaint();
        }
    }
}

impl Pane for SettingsPane {
    fn pane_id(&self) -> PaneId {
        self.id
    }

    fn get_cursor_position(&self) -> StableCursorPosition {
        StableCursorPosition {
            x: 0,
            y: 0,
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
            // Equal to the viewport: no scrollback, so no scroll bar.
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
        self.insert(text);
        Ok(())
    }

    fn send_composed_text(&self, text: &str) -> anyhow::Result<()> {
        self.insert(text);
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
        let config = config::configuration();
        self.state.lock().key(key, mods, &config, self.id);
        self.repaint();
        Ok(())
    }

    fn key_up(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        Ok(())
    }

    fn mouse_event(&self, event: MouseEvent) -> anyhow::Result<()> {
        let delta = match event.button {
            MouseButton::WheelUp(n) => -(n.max(1) as isize),
            MouseButton::WheelDown(n) => n.max(1) as isize,
            _ => return Ok(()),
        };
        self.state.lock().wheel(delta);
        self.repaint();
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
        // Wheel and clicks come here rather than starting a text selection
        // on a pane that has no text.
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

    fn capturing(action: KeyAssignment) -> SettingsState {
        let mut state = SettingsState::new();
        state.shortcuts = vec![ShortcutRow {
            action,
            custom: false,
            locked: false,
        }];
        state.capture = Some(Capture {
            row: 0,
            pending: None,
        });
        state
    }

    #[test]
    fn a_free_key_binds_at_once() {
        use ::window::{KeyCode as WK, Modifiers as WM};
        let mut state = capturing(KeyAssignment::BalancePanes);
        let mods = WM::SUPER | WM::ALT;
        assert_eq!(
            state.capture_key(&WK::Char('b'), mods, |_, _| None),
            CaptureOutcome::Bind {
                action: KeyAssignment::BalancePanes,
                key: WK::Char('b'),
                mods,
            }
        );
        assert!(state.capture.is_none());
    }

    #[test]
    fn a_taken_key_waits_for_enter_and_esc_backs_out_a_step_at_a_time() {
        use ::window::{KeyCode as WK, Modifiers as WM};
        let mut state = capturing(KeyAssignment::BalancePanes);
        let taken = |_: &WK, _: WM| Some("New tab".to_string());
        assert_eq!(
            state.capture_key(&WK::Char('t'), WM::SUPER, taken),
            CaptureOutcome::Nothing
        );
        assert_eq!(
            state
                .capture
                .as_ref()
                .and_then(|c| c.pending.as_ref())
                .map(|p| p.replaces.as_str()),
            Some("New tab")
        );
        // Esc drops the pending keys but keeps listening.
        state.capture_key(&WK::Char('\u{1b}'), WM::NONE, |_, _| None);
        assert_eq!(
            state.capture,
            Some(Capture {
                row: 0,
                pending: None
            })
        );
        state.capture_key(&WK::Char('t'), WM::SUPER, taken);
        assert_eq!(
            state.capture_key(&WK::Char('\r'), WM::NONE, |_, _| None),
            CaptureOutcome::Bind {
                action: KeyAssignment::BalancePanes,
                key: WK::Char('t'),
                mods: WM::SUPER,
            }
        );
        // A second Esc on a fresh capture stops capturing.
        let mut state = capturing(KeyAssignment::BalancePanes);
        state.capture_key(&WK::Char('\u{1b}'), WM::NONE, |_, _| None);
        assert!(state.capture.is_none());
    }

    #[test]
    fn backspace_unbinds_and_a_lone_modifier_is_ignored() {
        use ::window::{KeyCode as WK, Modifiers as WM};
        let mut state = capturing(KeyAssignment::BalancePanes);
        assert_eq!(
            state.capture_key(&WK::Shift, WM::SHIFT, |_, _| None),
            CaptureOutcome::Nothing
        );
        assert!(state.capture.is_some());
        assert_eq!(
            state.capture_key(&WK::Char('\u{8}'), WM::NONE, |_, _| None),
            CaptureOutcome::Unbind {
                action: KeyAssignment::BalancePanes
            }
        );
    }

    #[test]
    fn a_locked_row_does_not_capture() {
        let mut state = SettingsState::new();
        state.shortcuts = vec![ShortcutRow {
            action: KeyAssignment::BalancePanes,
            custom: false,
            locked: true,
        }];
        state.start_capture(0);
        assert!(state.capture.is_none());
        assert!(state.message.as_ref().is_some_and(|(_, error)| *error));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_picker_puts_names_that_start_with_the_query_first() {
        let items = names(&["Fira Code", "Cascadia Code", "Menlo", "Code New Roman"]);
        assert_eq!(filter_items(&items, ""), items);
        assert_eq!(
            filter_items(&items, "code"),
            ["Code New Roman", "Fira Code", "Cascadia Code"]
        );
        assert_eq!(filter_items(&items, "  MEN "), ["Menlo"]);
        assert!(filter_items(&items, "zzz").is_empty());
    }

    /// A state that looks like a painted tab: `visible` rows fit and every
    /// setting can be changed.
    fn state(visible: usize) -> SettingsState {
        let mut state = SettingsState::new();
        state.statuses = Some(Statuses {
            read_at: Instant::now(),
            by_setting: Some(vec![PathStatus::Absent; CATALOG.len()]),
        });
        state.visible_rows = visible;
        state.max_scroll = in_section(state.current_section())
            .len()
            .saturating_sub(visible);
        state
    }

    fn press(state: &mut SettingsState, key: KeyCode, mods: KeyModifiers) {
        let config = Config::default_config();
        state.key(key, mods, &config, 0);
    }

    #[test]
    fn arrows_walk_the_rows_and_keep_the_one_they_are_on_in_view() {
        let mut state = state(3);
        let count = in_section(state.current_section()).len();
        assert!(count > 3);

        press(&mut state, KeyCode::DownArrow, KeyModifiers::NONE);
        assert_eq!((state.focus, state.scroll), (Some(0), 0));
        for _ in 0..3 {
            press(&mut state, KeyCode::DownArrow, KeyModifiers::NONE);
        }
        assert_eq!((state.focus, state.scroll), (Some(3), 1));
        // Past the end: stays on the last row.
        for _ in 0..count {
            press(&mut state, KeyCode::DownArrow, KeyModifiers::NONE);
        }
        assert_eq!(state.focus, Some(count - 1));
        assert_eq!(state.scroll, count - 3);
        press(&mut state, KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(state.focus, Some(count - 4));

        press(&mut state, KeyCode::Escape, KeyModifiers::NONE);
        assert_eq!(state.focus, None);
    }

    #[test]
    fn typing_searches_every_section_and_esc_backs_out() {
        let mut state = state(5);
        for c in "opacity".chars() {
            press(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(state.search_focused && state.searching());
        let rows = state.rows();
        assert!(rows
            .iter()
            .any(|&i| CATALOG[i].path == ["window_background_opacity"]));
        // Down goes on to the results, keeping the query.
        press(&mut state, KeyCode::DownArrow, KeyModifiers::NONE);
        assert_eq!((state.search_focused, state.focus), (false, Some(0)));
        // Esc leaves the row, then clears the search.
        press(&mut state, KeyCode::Escape, KeyModifiers::NONE);
        assert_eq!(state.focus, None);
        press(&mut state, KeyCode::Escape, KeyModifiers::NONE);
        assert!(!state.searching());
        assert_eq!(state.rows(), in_section(state.current_section()));
    }

    #[test]
    fn tab_walks_the_sections_both_ways_and_wraps() {
        let mut state = state(5);
        press(&mut state, KeyCode::Tab, KeyModifiers::SHIFT);
        assert_eq!(state.current_section(), Section::Shortcuts);
        // Nothing to focus in a list that is only read.
        assert_eq!(state.focus, None);
        press(&mut state, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(state.current_section(), Section::Appearance);
        assert_eq!(state.focus, Some(0));
    }

    #[test]
    fn a_colour_scheme_field_opens_a_picker_and_typing_narrows_it() {
        let mut state = state(8);
        let config = Config::default_config();
        let index = CATALOG
            .iter()
            .position(|s| s.path == ["color_scheme"])
            .unwrap();
        state.hit(SettingsHit::Field(index), &config, 0);
        let all = state.editing.as_ref().unwrap().matches().len();
        assert!(all > 100, "the built-in schemes are offered: {}", all);

        for c in "dracula".chars() {
            press(&mut state, KeyCode::Char(c), KeyModifiers::NONE);
        }
        let editing = state.editing.as_ref().unwrap();
        let narrowed = editing.matches();
        assert!(!narrowed.is_empty() && narrowed.len() < all);
        assert!(narrowed[0].to_lowercase().starts_with("dracula"));

        // Arrows stay inside the matches; Escape leaves without a change.
        press(&mut state, KeyCode::UpArrow, KeyModifiers::NONE);
        assert_eq!(
            state
                .editing
                .as_ref()
                .unwrap()
                .picker
                .as_ref()
                .unwrap()
                .selected,
            0
        );
        press(&mut state, KeyCode::Escape, KeyModifiers::NONE);
        assert!(state.editing.is_none());

        // A click elsewhere abandons the field too.
        state.hit(SettingsHit::Field(index), &config, 0);
        state.hit(SettingsHit::Section(1), &config, 0);
        assert!(state.editing.is_none());
    }

    #[test]
    fn a_setting_the_file_computes_cannot_be_opened() {
        let mut state = state(8);
        let config = Config::default_config();
        let index = CATALOG.iter().position(|s| s.path == ["font"]).unwrap();
        if let Some(all) = state.statuses.as_mut().and_then(|s| s.by_setting.as_mut()) {
            all[index] = PathStatus::Locked { line: 3 };
        }
        state.hit(SettingsHit::Field(index), &config, 0);
        assert!(state.editing.is_none());
    }
}
