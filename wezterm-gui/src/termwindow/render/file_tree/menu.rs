//! The Worktree panel's right-click menu, and the actions it (and the
//! panel's keys) run: open, reveal, copy or paste paths, `cd` a shell there,
//! new file or folder, rename, move to trash.
//!
//! Naming happens in place: a new or renamed row turns into a text field,
//! as in a file manager, rather than in a dialog.

use super::*;
use crate::file_tree::ops::{self, ShellKind};
use crate::file_tree::{join_rel, trash};
use crate::termwindow::render::sidebar::SidebarDropdownRow;
use crate::termwindow::FileTreeMenuAction as Action;
use config::keyassignment::ClipboardCopyDestination;

/// The `rel` of the stand-in row a new file or folder is named in. Not a
/// path anything on disk can have.
pub(super) const EDIT_ROW: &str = "\0new";

/// Width of the menu, in pixels at 1x.
const MENU_W: f32 = 230.;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EditKind {
    Rename(String),
    NewFile(String),
    NewFolder(String),
}

/// A row being named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditState {
    pub kind: EditKind,
    pub text: String,
    /// Caret position, in chars.
    pub cursor: usize,
    /// The name is being written to disk.
    pub busy: bool,
}

impl EditState {
    fn insert(&mut self, text: &str) {
        let at = self
            .text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(at, _)| at);
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        self.cursor += clean.chars().count();
        self.text.insert_str(at, &clean);
    }

    fn remove(&mut self, before: bool) {
        let len = self.text.chars().count();
        let at = if before {
            match self.cursor.checked_sub(1) {
                Some(at) => at,
                None => return,
            }
        } else if self.cursor < len {
            self.cursor
        } else {
            return;
        };
        self.text = self
            .text
            .chars()
            .enumerate()
            .filter(|(index, _)| *index != at)
            .map(|(_, c)| c)
            .collect();
        self.cursor = at;
    }

    /// The row this field stands in: the renamed row, or [`EDIT_ROW`].
    pub(super) fn row_rel(&self) -> &str {
        match &self.kind {
            EditKind::Rename(rel) => rel,
            _ => EDIT_ROW,
        }
    }
}

/// Put the stand-in row of a new file or folder at the top of its folder,
/// where its name is typed.
pub(super) fn insert_edit_row(rows: &mut Vec<Row>, edit: &EditState) {
    let (dir, kind) = match &edit.kind {
        EditKind::Rename(_) => return,
        EditKind::NewFile(dir) => (dir, EntryKind::File),
        EditKind::NewFolder(dir) => (dir, EntryKind::Dir),
    };
    let (at, depth) = if dir.is_empty() {
        (0, 0)
    } else {
        match tree_view::index_of(rows, dir) {
            Some(at) => (at + 1, rows[at].depth + 1),
            None => (0, 0),
        }
    };
    rows.insert(
        at,
        Row {
            rel: EDIT_ROW.to_string(),
            highlight: Vec::new(),
            name: edit.text.clone(),
            depth,
            kind,
            symlink: false,
            state: if kind == EntryKind::Dir {
                RowState::Collapsed
            } else {
                RowState::File
            },
        },
    );
}

/// The menu's rows for a right-click on `targets` (none: the empty area
/// below the rows).
fn menu_rows(targets: &[(String, EntryKind)]) -> Vec<(&'static str, Action, bool)> {
    let reveal = if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Show in File Manager"
    };
    let one_file = matches!(targets, [(_, EntryKind::File)]);
    let one_dir = matches!(targets, [(_, EntryKind::Dir)]);
    let mut rows = Vec::new();
    if targets.is_empty() {
        rows.push(("New File\u{2026}", Action::NewFile, false));
        rows.push(("New Folder\u{2026}", Action::NewFolder, false));
        rows.push(("cd Here", Action::CdHere, true));
        rows.push((reveal, Action::Reveal, false));
        rows.push(("Copy Path", Action::CopyPath, true));
        return rows;
    }
    if one_file {
        rows.push(("Open", Action::Open, false));
        rows.push(("Open in New Tab", Action::OpenInTab, false));
    }
    if one_file || one_dir {
        rows.push(("Open with Default App", Action::OpenWithSystem, false));
    }
    rows.push((reveal, Action::Reveal, !rows.is_empty()));
    rows.push(("Copy Path", Action::CopyPath, true));
    rows.push(("Copy Relative Path", Action::CopyRelativePath, false));
    rows.push(("Paste Path into Shell", Action::PastePath, false));
    if one_file || one_dir {
        rows.push(("cd Here", Action::CdHere, false));
        rows.push(("New File\u{2026}", Action::NewFile, true));
        rows.push(("New Folder\u{2026}", Action::NewFolder, false));
        rows.push(("Rename\u{2026}", Action::Rename, false));
    }
    rows.push(("Move to Trash", Action::Trash, !(one_file || one_dir)));
    rows
}

pub(super) struct MenuState {
    tab_id: TabId,
    x: f32,
    y: f32,
    /// What the menu acts on, frozen when it opened.
    targets: Vec<(String, EntryKind)>,
    rows: Vec<(&'static str, Action, bool)>,
}

impl crate::TermWindow {
    /// Right-click on row `index` (or on the empty body, `None`): select the
    /// row unless it already is, and open the menu at the pointer.
    pub(super) fn file_tree_open_menu(&mut self, index: Option<usize>, event: &MouseEvent) {
        let Some(tab_id) = self.file_tree_active_tab() else {
            return;
        };
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        view.edit = None;
        let targets = match index.and_then(|index| view.rows.get(index)).cloned() {
            Some(row) if row.rel == EDIT_ROW => return,
            Some(row) => {
                if !view.is_marked(&row.rel) {
                    view.select_only(Some(row.rel.clone()));
                }
                view.marked_rows()
            }
            None => Vec::new(),
        };
        self.file_tree.focused = true;
        self.file_tree.menu = Some(MenuState {
            tab_id,
            x: event.coords.x as f32,
            y: event.coords.y as f32,
            rows: menu_rows(&targets),
            targets,
        });
    }

    pub fn file_tree_menu_open(&self) -> bool {
        self.file_tree.menu.is_some()
    }

    pub fn close_file_tree_menu(&mut self) -> bool {
        self.file_tree.menu.take().is_some()
    }

    pub fn paint_file_tree_menu(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some(menu) = self.file_tree.menu.as_ref() else {
            return Ok(());
        };
        if !self.file_tree_shown() || self.file_tree_active_tab() != Some(menu.tab_id) {
            self.file_tree.menu = None;
            return Ok(());
        }
        let (x, y) = (menu.x, menu.y);
        let rows: Vec<SidebarDropdownRow> = menu
            .rows
            .iter()
            .map(|(label, action, divider_above)| SidebarDropdownRow {
                label: label.to_string(),
                dot_color: None,
                checkbox: None,
                divider_above: *divider_above,
                indent: false,
                trailing_chevron: false,
                item_type: UIItemType::FileTreeMenuItem(*action),
            })
            .collect();
        self.paint_sidebar_dropdown(layers, x, y, MENU_W, true, &rows, None, None)
    }

    pub fn mouse_event_file_tree_menu_item(
        &mut self,
        action: Action,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(CursorIcon::Default));
        if event.kind != WMEK::Release(MousePress::Left) {
            return;
        }
        self.pressed_ui_item = None;
        if let Some(menu) = self.file_tree.menu.take() {
            self.file_tree_run(menu.tab_id, action, menu.targets);
        }
        context.invalidate();
    }

    /// The folder new files go in for `targets`: the folder itself, a
    /// file's folder, or the root.
    fn file_tree_folder_of(targets: &[(String, EntryKind)]) -> String {
        match targets.first() {
            Some((rel, EntryKind::Dir)) => rel.clone(),
            Some((rel, EntryKind::File)) => parent_rel(rel).unwrap_or("").to_string(),
            None => String::new(),
        }
    }

    /// Run `action` on `targets` (relative paths; empty means the root).
    pub(super) fn file_tree_run(
        &mut self,
        tab_id: TabId,
        action: Action,
        targets: Vec<(String, EntryKind)>,
    ) {
        let Some(root) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| tree.root.clone())
        else {
            return;
        };
        let abs = |rel: &str| Tree::new(root.clone()).abs_path(rel);
        let first = targets
            .first()
            .map(|(rel, _)| rel.clone())
            .unwrap_or_default();
        match action {
            Action::Open => self.file_tree_open_file(tab_id, &first, false),
            Action::OpenInTab => self.file_tree_open_file(tab_id, &first, true),
            Action::OpenWithSystem => {
                wezterm_open_url::open_url(&abs(&first).to_string_lossy());
            }
            Action::Reveal => reveal(&abs(&first)),
            Action::CopyPath | Action::CopyRelativePath => {
                let text = if targets.is_empty() {
                    root.to_string_lossy().to_string()
                } else {
                    targets
                        .iter()
                        .map(|(rel, _)| match action {
                            Action::CopyPath => abs(rel).to_string_lossy().to_string(),
                            _ => rel.clone(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.copy_to_clipboard(
                    ClipboardCopyDestination::ClipboardAndPrimarySelection,
                    text,
                );
            }
            Action::PastePath => {
                if let Some(target) = self.file_tree_target(tab_id) {
                    self.file_tree_paste_paths(tab_id, &targets, target.pane_id());
                }
            }
            Action::CdHere => {
                let dir = match targets.first() {
                    Some((rel, EntryKind::Dir)) => rel.clone(),
                    _ => Self::file_tree_folder_of(&targets),
                };
                self.file_tree_cd(tab_id, &dir);
            }
            Action::NewFile | Action::NewFolder => {
                let dir = Self::file_tree_folder_of(&targets);
                self.file_tree_start_new(tab_id, dir, action == Action::NewFolder);
            }
            Action::Rename => {
                if !first.is_empty() {
                    self.file_tree_start_rename(tab_id, &first);
                }
            }
            Action::Trash => self.file_tree_trash(tab_id, targets),
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Type a `cd` to `dir` into the target shell, or, when an agent runs
    /// there, open a shell in `dir` beside it instead of typing into the
    /// agent's prompt.
    fn file_tree_cd(&mut self, tab_id: TabId, dir: &str) {
        let Some(pane) = self.file_tree_target(tab_id) else {
            return;
        };
        let Some(native) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| tree.abs_path(dir).to_string_lossy().to_string())
        else {
            return;
        };
        let mux = Mux::get();
        let distro = mux
            .get_domain(pane.domain_id())
            .and_then(|domain| wsl_paths::distro_for_domain(domain.domain_name(), &self.config));
        let path = match distro.as_deref() {
            Some(distro) => wsl_paths::windows_to_wsl(&native, distro).unwrap_or(native),
            None => native,
        };
        let is_agent = self.agent_herd_state.try_borrow().is_ok_and(|herd| {
            herd.agents
                .iter()
                .any(|agent| agent.pane_id == Some(pane.pane_id()))
        });
        self.file_tree.focused = false;
        if is_agent {
            let spawn = SpawnCommand {
                cwd: Some(PathBuf::from(path)),
                domain: SpawnTabDomain::CurrentPaneDomain,
                ..Default::default()
            };
            let pane_id = pane.pane_id();
            let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
            promise::spawn::spawn(async move {
                let request = SplitRequest {
                    direction: SplitDirection::Horizontal,
                    target_is_second: true,
                    size: MuxSplitSize::Percent(50),
                    top_level: false,
                };
                match split_spawn(spawn, pane_id, request, term_config).await {
                    Ok((tab, pane)) => tab.set_active_pane(&pane),
                    Err(err) => log::error!("file tree: could not open a shell: {err:#}"),
                }
            })
            .detach();
            return;
        }
        let shell = if distro.is_some() {
            ShellKind::Posix
        } else {
            self.pane_command_basename(&pane)
                .map(|name| ShellKind::from_program(&name))
                .unwrap_or(ShellKind::Posix)
        };
        let text = ops::cd_command(&path, shell);
        if let Err(err) = pane.writer().write_all(text.as_bytes()) {
            log::warn!(
                "file tree: could not type into pane {}: {err:#}",
                pane.pane_id()
            );
            return;
        }
        if let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) {
            tab.set_active_pane(&pane);
        }
    }

    fn file_tree_start_new(&mut self, tab_id: TabId, dir: String, folder: bool) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        if view.filter.is_some() {
            view.filter = None;
        }
        if !dir.is_empty() && view.expanded.insert(dir.clone()) {
            view.expanded_changed_at = Some(Instant::now());
            view.scanned_at = None;
        }
        view.edit = Some(EditState {
            kind: if folder {
                EditKind::NewFolder(dir)
            } else {
                EditKind::NewFile(dir)
            },
            text: String::new(),
            cursor: 0,
            busy: false,
        });
        view.rebuild();
        view.select_only(Some(EDIT_ROW.to_string()));
        self.file_tree_scroll_to_selected(tab_id);
        self.file_tree.focused = true;
    }

    fn file_tree_start_rename(&mut self, tab_id: TabId, rel: &str) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        // The caret before the extension, where a rename usually goes.
        let stem = match name.rfind('.') {
            Some(0) | None => name.chars().count(),
            Some(dot) => name[..dot].chars().count(),
        };
        view.edit = Some(EditState {
            kind: EditKind::Rename(rel.to_string()),
            text: name,
            cursor: stem,
            busy: false,
        });
        view.select_only(Some(rel.to_string()));
        self.file_tree_scroll_to_selected(tab_id);
        self.file_tree.focused = true;
    }

    fn file_tree_scroll_to_selected(&mut self, tab_id: TabId) {
        let visible = self.file_tree.visible_rows.get();
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            if let Some(index) = view.selected_index() {
                view.scroll = tree_view::scroll_to_show(view.scroll, index, visible);
            }
        }
    }

    /// Keys while a row is being named. Every key is taken.
    pub(super) fn file_tree_edit_key(&mut self, tab_id: TabId, key: &KeyCode) -> bool {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return false;
        };
        let Some(edit) = view.edit.as_mut() else {
            return false;
        };
        if edit.busy {
            return true;
        }
        let len = edit.text.chars().count();
        match key {
            KeyCode::Escape => self.file_tree_cancel_edit(tab_id),
            KeyCode::Enter => self.file_tree_commit_edit(tab_id),
            KeyCode::Backspace => edit.remove(true),
            KeyCode::Delete => edit.remove(false),
            KeyCode::LeftArrow => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::RightArrow => edit.cursor = (edit.cursor + 1).min(len),
            KeyCode::Home => edit.cursor = 0,
            KeyCode::End => edit.cursor = len,
            KeyCode::Char(c) if !c.is_control() => edit.insert(&c.to_string()),
            _ => {}
        }
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            view.rebuild();
        }
        true
    }

    /// Composed text while a row is being named.
    pub(super) fn file_tree_edit_insert(&mut self, tab_id: TabId, text: &str) -> bool {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return false;
        };
        let Some(edit) = view.edit.as_mut().filter(|edit| !edit.busy) else {
            return false;
        };
        edit.insert(text);
        view.rebuild();
        true
    }

    pub(super) fn file_tree_cancel_edit(&mut self, tab_id: TabId) {
        if let Some(view) = self.file_tree.views.get_mut(&tab_id) {
            if let Some(edit) = view.edit.take() {
                let back = match edit.kind {
                    EditKind::Rename(rel) => Some(rel),
                    EditKind::NewFile(dir) | EditKind::NewFolder(dir) => {
                        Some(dir).filter(|dir| !dir.is_empty())
                    }
                };
                view.rebuild();
                view.select_only(back);
            }
        }
    }

    /// Write the name typed: create the file or folder, or rename the row.
    fn file_tree_commit_edit(&mut self, tab_id: TabId) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        let Some(root) = view.tree.as_ref().map(|tree| tree.root.clone()) else {
            return;
        };
        let Some(edit) = view.edit.as_mut() else {
            return;
        };
        let name = edit.text.clone();
        let planned = match &edit.kind {
            EditKind::Rename(rel) if rel.rsplit('/').next() == Some(name.as_str()) => {
                // Unchanged: nothing to write.
                self.file_tree_cancel_edit(tab_id);
                return;
            }
            EditKind::Rename(rel) => {
                ops::renamed(rel, &name).map(|to| (Some(rel.clone()), to, false))
            }
            EditKind::NewFile(dir) => {
                ops::validate_name(&name).map(|()| (None, join_rel(dir, &name), false))
            }
            EditKind::NewFolder(dir) => {
                ops::validate_name(&name).map(|()| (None, join_rel(dir, &name), true))
            }
        };
        let (from, to, folder) = match planned {
            Ok(planned) => planned,
            Err(reason) => {
                drag::file_tree_toast("Not saved", reason);
                return;
            }
        };
        edit.busy = true;
        let Some(window) = self.window.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let outcome = match &from {
                Some(from) => ops::apply_rename(&root, from, &to),
                None => ops::create(&root, &to, folder),
            };
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                term_window.file_tree_edit_done(tab_id, from, to, outcome);
            })));
        });
    }

    fn file_tree_edit_done(
        &mut self,
        tab_id: TabId,
        from: Option<String>,
        to: String,
        outcome: Result<(), String>,
    ) {
        let Some(view) = self.file_tree.views.get_mut(&tab_id) else {
            return;
        };
        if let Err(reason) = outcome {
            // Keep the field, so the name can be fixed.
            if let Some(edit) = view.edit.as_mut() {
                edit.busy = false;
            }
            drag::file_tree_toast("Not saved", reason);
        } else {
            view.edit = None;
            // A renamed folder keeps its open folders, under the new name.
            if let Some(from) = &from {
                let moved: Vec<String> = view
                    .expanded
                    .iter()
                    .filter(|rel| *rel == from || rel.starts_with(&format!("{from}/")))
                    .cloned()
                    .collect();
                for rel in moved {
                    view.expanded.remove(&rel);
                    view.expanded.insert(format!("{to}{}", &rel[from.len()..]));
                }
                view.expanded_changed_at = Some(Instant::now());
            }
            view.select_only(Some(to.clone()));
            view.reveal(&to);
        }
        view.scanned_at = None;
        view.rebuild();
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Move `targets` to the trash, asking first for more than one.
    fn file_tree_trash(&mut self, tab_id: TabId, targets: Vec<(String, EntryKind)>) {
        let targets = ops::outermost(&targets);
        if targets.is_empty() {
            return;
        }
        let Some(root) = self
            .file_tree
            .views
            .get(&tab_id)
            .and_then(|view| view.tree.as_ref())
            .map(|tree| tree.root.clone())
        else {
            return;
        };
        let count = targets.len();
        let message = match targets.as_slice() {
            [(rel, _)] => format!(
                "Move {} to the Trash?",
                rel.rsplit('/').next().unwrap_or(rel)
            ),
            _ => format!("Move {count} items to the Trash?"),
        };
        let run = move |term_window: &mut crate::TermWindow| {
            let Some(window) = term_window.window.clone() else {
                return;
            };
            std::thread::spawn(move || {
                let tree = Tree::new(root);
                let failures: Vec<String> = targets
                    .iter()
                    .filter_map(|(rel, _)| {
                        trash::move_to_trash(&tree.abs_path(rel))
                            .err()
                            .map(|reason| format!("{rel}: {reason}"))
                    })
                    .collect();
                window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    if !failures.is_empty() {
                        drag::file_tree_toast("Not moved to the Trash", failures.join("\n"));
                    }
                    if let Some(view) = term_window.file_tree.views.get_mut(&tab_id) {
                        view.select_only(None);
                        view.scanned_at = None;
                    }
                    if let Some(window) = term_window.window.as_ref() {
                        window.invalidate();
                    }
                })));
            });
        };
        if count > 1 {
            self.file_tree_confirm(message, run);
        } else {
            run(self);
        }
    }
}

/// Show `path` selected in the platform's file manager.
fn reveal(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let result = if cfg!(target_os = "macos") {
            std::process::Command::new("/usr/bin/open")
                .arg("-R")
                .arg(&path)
                .status()
        } else if cfg!(windows) {
            let mut select = std::ffi::OsString::from("/select,");
            select.push(&path);
            std::process::Command::new("explorer.exe")
                .arg(select)
                .status()
        } else {
            // No portable "select this file": open its folder.
            let dir = if path.is_dir() {
                path.clone()
            } else {
                path.parent()
                    .map(|dir| dir.to_path_buf())
                    .unwrap_or(path.clone())
            };
            wezterm_open_url::open_url(&dir.to_string_lossy());
            return;
        };
        if let Err(err) = result {
            log::warn!("file tree: could not reveal {}: {err:#}", path.display());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, cursor: usize) -> EditState {
        EditState {
            kind: EditKind::NewFile(String::new()),
            text: text.into(),
            cursor,
            busy: false,
        }
    }

    #[test]
    fn the_field_edits_at_the_caret() {
        let mut field = edit("main.rs", 4);
        field.insert("_x");
        assert_eq!((field.text.as_str(), field.cursor), ("main_x.rs", 6));
        field.remove(true);
        assert_eq!((field.text.as_str(), field.cursor), ("main_.rs", 5));
        field.remove(false);
        assert_eq!((field.text.as_str(), field.cursor), ("main_rs", 5));
        let mut field = edit("ü", 0);
        field.remove(true);
        assert_eq!(field.text, "ü");
        field.cursor = 1;
        field.remove(false);
        assert_eq!(field.text, "ü");
        field.insert("\tä");
        assert_eq!(field.text, "üä");
    }

    #[test]
    fn a_new_row_is_named_at_the_top_of_its_folder() {
        let row = |rel: &str, depth| Row {
            rel: rel.into(),
            highlight: Vec::new(),
            name: rel.rsplit('/').next().unwrap().into(),
            depth,
            kind: EntryKind::Dir,
            symlink: false,
            state: RowState::Expanded,
        };
        let mut rows = vec![row("a", 0), row("a/b", 1), row("c", 0)];
        let mut field = edit("x", 1);
        field.kind = EditKind::NewFolder("a".into());
        insert_edit_row(&mut rows, &field);
        assert_eq!(rows[1].rel, EDIT_ROW);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].name, "x");
        field.kind = EditKind::NewFile(String::new());
        insert_edit_row(&mut rows, &field);
        assert_eq!((rows[0].rel.as_str(), rows[0].depth), (EDIT_ROW, 0));
    }

    #[test]
    fn the_menu_fits_what_was_clicked() {
        let actions = |targets: &[(String, EntryKind)]| -> Vec<Action> {
            menu_rows(targets)
                .into_iter()
                .map(|(_, action, _)| action)
                .collect()
        };
        let file = vec![("a.rs".to_string(), EntryKind::File)];
        assert!(actions(&file).contains(&Action::Open));
        assert!(actions(&file).contains(&Action::Rename));
        let dir = vec![("src".to_string(), EntryKind::Dir)];
        assert!(!actions(&dir).contains(&Action::Open));
        assert!(actions(&dir).contains(&Action::CdHere));
        let many = vec![file[0].clone(), dir[0].clone()];
        let many = actions(&many);
        assert!(many.contains(&Action::Trash) && many.contains(&Action::CopyPath));
        assert!(!many.contains(&Action::Rename) && !many.contains(&Action::Open));
        let empty = actions(&[]);
        assert_eq!(empty[0], Action::NewFile);
        assert!(!empty.contains(&Action::Trash));
    }
}
