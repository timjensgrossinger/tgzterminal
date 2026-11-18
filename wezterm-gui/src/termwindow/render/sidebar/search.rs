//! The results under the sidebar's search field (see `tgz_search`): what
//! the window offers as candidates, the dropdown, and acting on a pick.
//!
//! Candidates come from caches only (commands, layouts, the ssh host cache,
//! the session scan), so typing never touches the disk. The session scan is
//! kicked when the field gets focus and refreshes the results when it lands.
//! A child of `sidebar` so it can use its dropdown painter.

use super::*;
use crate::termwindow::tgz_search::{self, Candidate, Row, Source, Target};
use crate::TermWindow;
use parking_lot::Mutex;
use std::path::PathBuf;
use wezterm_term::StableRowIndex;

/// Widest the results grow, in logical pixels.
const SEARCH_RESULTS_MAX_W: f32 = 560.;
/// Narrowest, even beside a thin sidebar.
const SEARCH_RESULTS_MIN_W: f32 = 320.;
/// A project's file list is read again once it is this old.
const SEARCH_INDEX_TTL: Duration = Duration::from_secs(30);
/// File results asked of the worker.
const SEARCH_FILE_HITS: usize = 50;
/// Shortest query the scrollback is searched for: fewer characters match
/// most of every screen.
const SCROLLBACK_MIN_CHARS: usize = 3;
/// Scrollback hits taken from one pane, and from all of them.
const SCROLLBACK_PER_PANE: u32 = 5;
const SCROLLBACK_MAX_HITS: usize = 30;
/// Characters of a scrollback line shown around its match.
const SCROLLBACK_PREVIEW_CHARS: usize = 90;

/// The last project file list a search built: its root, when, and the
/// paths. Shared by the workers so typing reuses it; the lock is held while
/// one is built, so keystrokes arriving meanwhile wait for it instead of
/// each reading the tree.
static SEARCH_INDEX: Mutex<Option<(PathBuf, Instant, Arc<Vec<String>>)>> = Mutex::new(None);

impl TermWindow {
    /// Everything the search can find right now, from caches only.
    fn sidebar_search_candidates(&self) -> Vec<Candidate> {
        use config::keyassignment::{KeyAssignment, PaneLayout};
        let mut out = vec![];

        // Tabs, matched on what their rows show.
        for entry in self.tab_bar.items() {
            if let TabBarItem::Tab { tab_idx, .. } = entry.item {
                let (title, metadata, _) = self.sidebar_tab_labels(tab_idx, &entry.title);
                out.push(Candidate {
                    source: Source::Tab,
                    haystack: format!("{title} {}", metadata.join(" ")),
                    label: title,
                    detail: format!("tab {}", tab_idx + 1),
                    target: Target::Tab(tab_idx),
                });
            }
        }

        let wanted = self.config.sidebar_search.clone();
        // Commands, as the palette lists them; layouts have a section of
        // their own.
        let commands = if wanted.commands {
            crate::commands::CommandDef::actions_for_palette_and_menubar(&self.config)
        } else {
            vec![]
        };
        for command in commands {
            if matches!(
                command.action,
                KeyAssignment::ApplyPaneLayout(_) | KeyAssignment::OpenPaneLayout(_)
            ) {
                continue;
            }
            out.push(Candidate {
                source: Source::Command,
                haystack: format!("{} {}", command.brief, command.doc),
                detail: self.shortcut_text(&command.action).unwrap_or_default(),
                label: command.brief.to_string(),
                target: Target::Command(command.action),
            });
        }

        for layout in PaneLayout::ALL.iter().copied().filter(|_| wanted.commands) {
            let label = format!("Layout: {}", layout.label());
            out.push(Candidate {
                source: Source::Layout,
                haystack: label.clone(),
                detail: self
                    .shortcut_text(&KeyAssignment::ApplyPaneLayout(layout))
                    .unwrap_or_default(),
                label,
                target: Target::Preset(layout),
            });
        }
        for layout in self.config.tgz_layouts.iter().filter(|_| wanted.commands) {
            out.push(Candidate {
                source: Source::Layout,
                haystack: format!("{} {}", layout.name, layout.layout.label()),
                detail: self
                    .shortcut_text(&KeyAssignment::OpenPaneLayout(layout.name.clone()))
                    .unwrap_or_else(|| "new tab".to_string()),
                label: layout.name.clone(),
                target: Target::UserLayout(layout.name.clone()),
            });
        }

        for entry in self
            .ssh_quick_launch_entries()
            .iter()
            .filter(|_| wanted.commands)
        {
            out.push(Candidate {
                source: Source::Ssh,
                haystack: format!("{} {}", entry.label, entry.domain_name),
                detail: format!("{:?}", entry.transport).to_lowercase(),
                label: entry.label.clone(),
                target: Target::Ssh(entry.domain_name.clone()),
            });
        }

        // Past sessions: title, first prompt, folder and branch are searched.
        if let Some(scan) = self
            .agent_session_cache
            .as_ref()
            .filter(|_| wanted.sessions)
        {
            for session in scan.sessions.iter() {
                let cwd = session.cwd.to_string_lossy();
                out.push(Candidate {
                    source: Source::Session,
                    haystack: format!(
                        "{} · {} · {} · {} · {}",
                        session.label,
                        session.first_prompt.as_deref().unwrap_or(""),
                        session.project,
                        cwd,
                        session.git_branch.as_deref().unwrap_or(""),
                    ),
                    detail: format!("{} \u{b7} {}", session.adapter_id, session.project),
                    label: session.label.clone(),
                    target: Target::Session {
                        adapter_id: session.adapter_id.clone(),
                        session_id: session.session_id.clone(),
                    },
                });
            }
        }
        out
    }

    /// Recompute the results after the query (or a cache) changed.
    pub(crate) fn refresh_sidebar_search(&mut self) {
        let Some(query) = self.sidebar_search.as_ref().map(|s| s.query.clone()) else {
            return;
        };
        let (scope, needle) = tgz_search::parse_query(&query);
        let rows = tgz_search::rank(scope, needle, &self.sidebar_search_candidates());
        let sessions_at = self
            .agent_session_cache
            .as_ref()
            .map(|scan| scan.scanned_at);
        let generation = match self.sidebar_search.as_mut() {
            Some(state) => {
                state.selected = tgz_search::step_selection(&rows, None, 1);
                state.rows = rows;
                state.scroll = 0;
                state.sessions_at = sessions_at;
                state.generation += 1;
                state.generation
            }
            None => return,
        };
        if scope.includes(Source::File) && !needle.is_empty() && self.config.sidebar_search.files {
            self.search_files(scope, needle.to_string(), generation);
        }
        if scope.includes(Source::Scrollback)
            && needle.chars().count() >= SCROLLBACK_MIN_CHARS
            && self.config.sidebar_search.scrollback
        {
            self.search_scrollback(scope, needle.to_string(), generation);
        }
    }

    /// Search the scrollback of this window's local panes for `needle`, in
    /// memory, as the Search overlay does. Nothing is kept beyond the
    /// results list, and each preview is masked like the screen.
    fn search_scrollback(&mut self, scope: tgz_search::Scope, needle: String, generation: u64) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let mux = Mux::get();
        let Some(mux_window) = mux.get_window(self.mux_window_id) else {
            return;
        };
        let local = |pane: &Arc<dyn Pane>| {
            mux.get_domain(pane.domain_id())
                .is_some_and(|domain| domain.downcast_ref::<mux::domain::LocalDomain>().is_some())
        };
        let panes: Vec<(Arc<dyn Pane>, String)> = (0..mux_window.count_tabs())
            .filter_map(|idx| mux_window.get_tab_at_idx(idx).cloned())
            .flat_map(|tab| tab.iter_panes_ignoring_zoom())
            .map(|positioned| positioned.pane)
            .filter(|pane| local(pane))
            .map(|pane| {
                let title = pane.get_title();
                (pane, title)
            })
            .collect();
        drop(mux_window);
        let rules = self.secret_rules();
        let cap = if scope == tgz_search::Scope::All {
            tgz_search::MIXED_CAP
        } else {
            tgz_search::NARROW_CAP.min(SCROLLBACK_MAX_HITS)
        };
        promise::spawn::spawn(async move {
            let mut hits: Vec<Candidate> = vec![];
            for (pane, title) in panes {
                if hits.len() >= SCROLLBACK_MAX_HITS {
                    break;
                }
                let dims = pane.get_dimensions();
                let range =
                    dims.scrollback_top..dims.physical_top + dims.viewport_rows as StableRowIndex;
                let found = match pane
                    .search(
                        mux::pane::Pattern::CaseInSensitiveString(needle.clone()),
                        range,
                        Some(SCROLLBACK_PER_PANE),
                    )
                    .await
                {
                    Ok(found) => found,
                    Err(err) => {
                        log::debug!("search: scrollback of pane {}: {err:#}", pane.pane_id());
                        continue;
                    }
                };
                for result in found {
                    let (_, lines) = pane.get_lines(result.start_y..result.start_y + 1);
                    let Some(line) = lines.first() else {
                        continue;
                    };
                    let text = line.as_str().to_string();
                    let (masked, _) = rules.redact(&text);
                    hits.push(Candidate {
                        source: Source::Scrollback,
                        label: scrollback_preview(&masked, result.start_x),
                        detail: title.clone(),
                        haystack: String::new(),
                        target: Target::Scrollback {
                            pane_id: pane.pane_id(),
                            row: result.start_y,
                        },
                    });
                }
            }
            log::debug!("search: {} scrollback hits", hits.len());
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                let Some(state) = term_window.sidebar_search.as_mut() else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                let selected = state.selected.and_then(|at| state.rows.get(at).cloned());
                let rows = std::mem::take(&mut state.rows);
                state.rows = tgz_search::with_section(rows, Source::Scrollback, &hits, cap);
                state.selected = selected
                    .and_then(|row| state.rows.iter().position(|r| *r == row))
                    .or_else(|| tgz_search::step_selection(&state.rows, None, 1));
                if let Some(window) = term_window.window.as_ref() {
                    window.invalidate();
                }
            })));
        })
        .detach();
    }

    /// Match `needle` against the active pane's project files on a worker;
    /// the hits join the results if the query is still `generation`.
    fn search_files(&mut self, scope: tgz_search::Scope, needle: String, generation: u64) {
        let Some(pane) = self.get_active_pane_no_overlay() else {
            return;
        };
        // A remote tree cannot be listed from here.
        if self.file_tree_needs_picker(&pane) {
            return;
        }
        let Ok(target) = self.diff_panel_target(&pane) else {
            return;
        };
        let Some(window) = self.window.clone() else {
            return;
        };
        let other_distro = Self::default_wsl_distro();
        let options = self.file_tree_list_options();
        let cap = if scope == tgz_search::Scope::All {
            tgz_search::MIXED_CAP
        } else {
            tgz_search::NARROW_CAP
        };
        let spawned = std::thread::Builder::new()
            .name("search-files".into())
            .spawn(move || {
                use crate::termwindow::render::diff_panel::settle_scan_target;
                let Ok((dir, runner)) = settle_scan_target(target, other_distro) else {
                    return;
                };
                let root = crate::diff_panel::detect_vcs(&dir)
                    .map(|(_, root)| root)
                    .unwrap_or(dir);
                let index = {
                    let mut cached = SEARCH_INDEX.lock();
                    match cached.as_ref() {
                        Some((at, built, index))
                            if *at == root && built.elapsed() < SEARCH_INDEX_TTL =>
                        {
                            Arc::clone(index)
                        }
                        _ => {
                            let is_git = root.join(".git").exists();
                            let (paths, _truncated) = crate::file_tree::list::path_index(
                                &root,
                                options,
                                is_git.then_some(&runner),
                            );
                            let paths = Arc::new(paths);
                            *cached = Some((root.clone(), Instant::now(), Arc::clone(&paths)));
                            paths
                        }
                    }
                };
                let folder = root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let hits: Vec<Candidate> =
                    crate::file_tree::filter::matches(&index, &needle, SEARCH_FILE_HITS)
                        .into_iter()
                        .map(|hit| Candidate {
                            source: Source::File,
                            label: hit.rel.clone(),
                            detail: folder.clone(),
                            haystack: hit.rel.clone(),
                            target: Target::File {
                                root: root.clone(),
                                rel: hit.rel,
                            },
                        })
                        .collect();
                window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    let Some(state) = term_window.sidebar_search.as_mut() else {
                        return;
                    };
                    if state.generation != generation {
                        return;
                    }
                    let selected = state.selected.and_then(|at| state.rows.get(at).cloned());
                    let rows = std::mem::take(&mut state.rows);
                    state.rows = tgz_search::with_section(rows, Source::File, &hits, cap);
                    // Keep the highlight on the row it was on, else the first.
                    state.selected = selected
                        .and_then(|row| state.rows.iter().position(|r| *r == row))
                        .or_else(|| tgz_search::step_selection(&state.rows, None, 1));
                    if let Some(window) = term_window.window.as_ref() {
                        window.invalidate();
                    }
                })));
            });
        if let Err(err) = spawned {
            log::warn!("search: could not start the file search: {err:#}");
        }
    }

    /// Move the highlighted result. `false` when there are none, so the key
    /// can go on to the sidebar instead.
    pub(crate) fn move_sidebar_search_selection(&mut self, step: isize) -> bool {
        let Some(state) = self.sidebar_search.as_mut() else {
            return false;
        };
        match tgz_search::step_selection(&state.rows, state.selected, step) {
            Some(next) => {
                state.selected = Some(next);
                true
            }
            None => false,
        }
    }

    /// Act on result `index` and close the search.
    pub(crate) fn activate_sidebar_search_row(&mut self, index: usize) {
        let Some(Row::Hit { target, .. }) = self
            .sidebar_search
            .as_ref()
            .and_then(|state| state.rows.get(index))
            .cloned()
        else {
            return;
        };
        self.sidebar_search = None;
        match target {
            Target::Tab(idx) => {
                self.activate_tab(idx as isize).ok();
            }
            Target::Command(action) => {
                if let Some(pane) = self.get_active_pane_or_overlay() {
                    if let Err(err) = self.perform_key_assignment(&pane, &action) {
                        log::error!("search: {action:?}: {err:#}");
                    }
                }
            }
            Target::Preset(layout) => self.apply_pane_layout(layout),
            Target::UserLayout(name) => self.open_user_pane_layout(&name),
            Target::Ssh(domain) => self.spawn_ssh_quick_launch_entry(&domain),
            Target::Session {
                adapter_id,
                session_id,
            } => self.resume_agent_session(usize::MAX, &adapter_id, &session_id, None),
            Target::File { root, rel } => self.open_search_file(root, rel),
            Target::Scrollback { pane_id, row } => self.show_search_scrollback(pane_id, row),
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// Open a file a search found, in the editor the Worktree uses.
    fn open_search_file(&mut self, root: PathBuf, rel: String) {
        let Some(pane) = self.get_active_pane_no_overlay() else {
            return;
        };
        let Some(tab) = Mux::get().get_active_tab_for_window(self.mux_window_id) else {
            return;
        };
        // Paths from the index use `/`; a Windows share wants its own.
        let path = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        self.open_file_in_editor(tab.tab_id(), pane, root, path, false);
    }

    /// Show a scrollback hit: its pane, scrolled so the line sits a third of
    /// the way down.
    fn show_search_scrollback(&mut self, pane_id: PaneId, row: StableRowIndex) {
        if !self.activate_sidebar_pane(pane_id) {
            return;
        }
        let Some(pane) = Mux::get().get_pane(pane_id) else {
            return;
        };
        let dims = pane.get_dimensions();
        let top = row - (dims.viewport_rows as StableRowIndex / 3);
        self.set_viewport(pane_id, Some(top), dims);
    }

    /// The results, hanging from the search field.
    pub(crate) fn paint_sidebar_search_results(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let Some((x, bottom, field_w)) = self.sidebar_search_anchor else {
            return Ok(());
        };
        // Only under a field painted this frame: a sidebar that hid itself
        // leaves the anchor behind.
        if !self
            .ui_items
            .iter()
            .any(|item| item.item_type == UIItemType::SidebarSearch)
        {
            return Ok(());
        }
        // A session scan that landed since the results were built.
        let sessions_at = self
            .agent_session_cache
            .as_ref()
            .map(|scan| scan.scanned_at);
        if self
            .sidebar_search
            .as_ref()
            .is_some_and(|state| !state.query.trim().is_empty() && state.sessions_at != sessions_at)
        {
            let selected = self.sidebar_search.as_ref().and_then(|s| s.selected);
            self.refresh_sidebar_search();
            if let (Some(state), Some(selected)) = (self.sidebar_search.as_mut(), selected) {
                if state.rows.get(selected).is_some_and(Row::is_hit) {
                    state.selected = Some(selected);
                }
            }
        }
        let Some(state) = self.sidebar_search.as_ref() else {
            return Ok(());
        };
        if state.rows.is_empty() {
            return Ok(());
        }

        let dpi_scale = (self.dimensions.dpi as f32 / 96.).clamp(1., 2.5);
        let width = (field_w / dpi_scale).clamp(SEARCH_RESULTS_MIN_W, SEARCH_RESULTS_MAX_W);
        let rows: Vec<SidebarDropdownRow> = state
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| match row {
                Row::Heading(source) => SidebarDropdownRow {
                    label: source.heading().to_string(),
                    dot_color: None,
                    checkbox: None,
                    divider_above: i > 0,
                    indent: false,
                    trailing_chevron: false,
                    trailing_hint: None,
                    widget: None,
                    inert: true,
                    item_type: UIItemType::SidebarDropdownBody,
                },
                Row::Hit { label, detail, .. } => SidebarDropdownRow {
                    label: label.clone(),
                    dot_color: None,
                    checkbox: None,
                    divider_above: false,
                    indent: true,
                    trailing_chevron: false,
                    trailing_hint: (!detail.is_empty()).then(|| detail.clone()),
                    widget: None,
                    inert: false,
                    item_type: UIItemType::SidebarSearchHit(i),
                },
            })
            .collect();

        // Keep the highlighted row in view: as many rows as fit below the
        // field, scrolled so the selection is among them.
        let cell_h = self.render_metrics.cell_size.height as f32;
        let row_h = (cell_h + 12. * dpi_scale).max(PANE_COPY_MENU_ROW_H * dpi_scale);
        let fit = (((self.dimensions.pixel_height as f32 - bottom) / row_h).floor() as usize)
            .saturating_sub(1)
            .max(3);
        let selected = state.selected;
        let mut scroll = state.scroll.min(rows.len().saturating_sub(fit));
        if let Some(selected) = selected {
            if selected < scroll {
                scroll = selected.saturating_sub(1);
            } else if selected >= scroll + fit {
                scroll = selected + 1 - fit;
            }
        }
        if let Some(state) = self.sidebar_search.as_mut() {
            state.scroll = scroll;
        }
        self.dropdown_key_selected = selected.map(UIItemType::SidebarSearchHit);
        self.paint_sidebar_dropdown(layers, x, bottom, width, true, &rows, Some(scroll), None, 0)
    }
}

/// A scrollback line cut to a preview around the match at `column`, with an
/// ellipsis where it was cut.
fn scrollback_preview(line: &str, column: usize) -> String {
    let chars: Vec<char> = line.trim_end().chars().collect();
    if chars.len() <= SCROLLBACK_PREVIEW_CHARS {
        return chars.iter().collect::<String>().trim_start().to_string();
    }
    let start = column
        .saturating_sub(SCROLLBACK_PREVIEW_CHARS / 3)
        .min(chars.len() - SCROLLBACK_PREVIEW_CHARS);
    let end = start + SCROLLBACK_PREVIEW_CHARS;
    let mut out = String::new();
    if start > 0 {
        out.push('\u{2026}');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_scrollback_line_is_cut_around_its_match() {
        assert_eq!(scrollback_preview("  short line  ", 2), "short line");
        let line = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        let preview = scrollback_preview(&line, 200);
        assert!(preview.contains("needle"), "{}", preview);
        assert!(preview.starts_with('\u{2026}') && preview.ends_with('\u{2026}'));
        assert_eq!(preview.chars().count(), SCROLLBACK_PREVIEW_CHARS + 2);
        // A match near the end keeps a full window, not a short tail.
        let tail = scrollback_preview(&line, 405);
        assert!(tail.ends_with('b') && tail.chars().count() == SCROLLBACK_PREVIEW_CHARS + 1);
    }
}
