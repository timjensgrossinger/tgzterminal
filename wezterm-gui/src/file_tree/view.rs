//! Turning a [`Tree`] into the flat list of rows the panel scrolls.

use super::{join_rel, EntryKind, Listing, Tree};
use std::collections::HashSet;

/// How a row's chevron reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    File,
    Collapsed,
    Expanded,
    /// Open, but its listing has not come back yet.
    Loading,
    /// Open, but it could not be read.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Relative to the tree's root.
    pub rel: String,
    /// Characters of `name` the filter matched, as char offsets.
    pub highlight: Vec<usize>,
    pub name: String,
    /// 0 for the root's own entries.
    pub depth: usize,
    pub kind: EntryKind,
    pub symlink: bool,
    pub state: RowState,
}

/// Every visible row: the root's entries, and inside each folder in
/// `expanded` its own, depth first.
pub fn flatten(tree: &Tree, expanded: &HashSet<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    push_children(tree, expanded, "", 0, &mut rows);
    rows
}

fn push_children(
    tree: &Tree,
    expanded: &HashSet<String>,
    dir: &str,
    depth: usize,
    rows: &mut Vec<Row>,
) {
    let Some(Listing::Loaded(entries)) = tree.listing(dir) else {
        return;
    };
    for entry in entries {
        let rel = join_rel(dir, &entry.name);
        let state = match entry.kind {
            EntryKind::File => RowState::File,
            EntryKind::Dir if !expanded.contains(&rel) => RowState::Collapsed,
            EntryKind::Dir => match tree.listing(&rel) {
                None => RowState::Loading,
                Some(Listing::Failed(_)) => RowState::Failed,
                Some(Listing::Loaded(_)) => RowState::Expanded,
            },
        };
        rows.push(Row {
            rel: rel.clone(),
            highlight: Vec::new(),
            name: entry.name.clone(),
            depth,
            kind: entry.kind,
            symlink: entry.symlink,
            state,
        });
        if state == RowState::Expanded {
            push_children(tree, expanded, &rel, depth + 1, rows);
        }
    }
}

/// Open folders that still need reading: the root, then any expanded folder
/// that is on show and not listed yet.
pub fn unlisted(tree: &Tree, rows: &[Row]) -> Vec<String> {
    let mut wanted = Vec::new();
    if !tree.is_listed("") {
        wanted.push(String::new());
    }
    wanted.extend(
        rows.iter()
            .filter(|row| row.state == RowState::Loading)
            .map(|row| row.rel.clone()),
    );
    wanted
}

/// Drop the open folders their parent's listing shows are gone. A folder
/// whose parent is closed or not read yet is kept, so reopening the parent
/// brings its children back as they were.
pub fn prune_expanded(tree: &Tree, expanded: &mut HashSet<String>) {
    expanded.retain(|rel| {
        let Some(parent) = super::parent_rel(rel) else {
            return false;
        };
        let name = rel.rsplit('/').next().unwrap_or(rel);
        match tree.listing(parent) {
            Some(Listing::Loaded(entries)) => entries
                .iter()
                .any(|entry| entry.name == name && entry.kind == EntryKind::Dir),
            _ => true,
        }
    });
}

/// The row showing `rel`, if it is on show.
pub fn index_of(rows: &[Row], rel: &str) -> Option<usize> {
    rows.iter().position(|row| row.rel == rel)
}

/// The scroll offset that keeps row `index` in a window of `visible` rows,
/// moving as little as possible.
pub fn scroll_to_show(scroll: usize, index: usize, visible: usize) -> usize {
    if visible == 0 || index < scroll {
        index
    } else if index >= scroll + visible {
        index + 1 - visible
    } else {
        scroll
    }
}

/// The rows from `anchor` to `to`, both included, in row order: a
/// Shift-click's selection. Just `to` when the anchor is not on show.
pub fn range(rows: &[Row], anchor: &str, to: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (index_of(rows, anchor), index_of(rows, to)) else {
        return index_of(rows, to)
            .map(|at| vec![rows[at].rel.clone()])
            .unwrap_or_default();
    };
    let (start, end) = (a.min(b), a.max(b));
    rows[start..=end]
        .iter()
        .map(|row| row.rel.clone())
        .collect()
}

/// Where the scroll bar's thumb sits on a track of `track_h` pixels:
/// `(offset from the track's top, height)`. `None` when everything fits.
pub fn thumb(
    track_h: f32,
    total: usize,
    visible: usize,
    scroll: usize,
    min_h: f32,
) -> Option<(f32, f32)> {
    if visible == 0 || total <= visible || track_h <= 0. {
        return None;
    }
    let height = (track_h * visible as f32 / total as f32)
        .max(min_h)
        .min(track_h);
    let max_scroll = (total - visible) as f32;
    let offset = (track_h - height) * (scroll as f32 / max_scroll).min(1.);
    Some((offset, height))
}

/// The scroll offset that puts the thumb's top `offset` pixels down its
/// track: the inverse of [`thumb`].
pub fn scroll_for_thumb(
    track_h: f32,
    thumb_h: f32,
    total: usize,
    visible: usize,
    offset: f32,
) -> usize {
    let room = track_h - thumb_h;
    if room <= 0. || total <= visible {
        return 0;
    }
    let max_scroll = (total - visible) as f32;
    ((offset / room).clamp(0., 1.) * max_scroll).round() as usize
}

/// What a navigation key asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Nav {
    Select(usize),
    /// Open or close this folder.
    Toggle(String),
    Nothing,
}

/// Up or down by `delta` rows, stopping at either end. With nothing
/// selected, down starts at the top and up at the bottom.
pub fn moved(selected: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let last = len as isize - 1;
    let next = match selected {
        Some(at) => at as isize + delta,
        None if delta < 0 => last,
        None => 0,
    };
    Some(next.clamp(0, last) as usize)
}

/// Left: close an open folder, otherwise go to the folder holding the row.
pub fn left(rows: &[Row], selected: usize) -> Nav {
    let Some(row) = rows.get(selected) else {
        return Nav::Nothing;
    };
    if matches!(
        row.state,
        RowState::Expanded | RowState::Loading | RowState::Failed
    ) {
        return Nav::Toggle(row.rel.clone());
    }
    match super::parent_rel(&row.rel)
        .filter(|parent| !parent.is_empty())
        .and_then(|parent| index_of(rows, parent))
    {
        Some(at) => Nav::Select(at),
        None => Nav::Nothing,
    }
}

/// Right: open a closed folder, otherwise step into an open one.
pub fn right(rows: &[Row], selected: usize) -> Nav {
    let Some(row) = rows.get(selected) else {
        return Nav::Nothing;
    };
    match row.state {
        RowState::Collapsed => Nav::Toggle(row.rel.clone()),
        RowState::Expanded
            if rows
                .get(selected + 1)
                .is_some_and(|next| next.depth > row.depth) =>
        {
            Nav::Select(selected + 1)
        }
        _ => Nav::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_tree::Entry;
    use std::path::PathBuf;

    fn tree() -> Tree {
        let mut tree = Tree::new(PathBuf::from("/repo"));
        tree.set_listing(
            String::new(),
            Listing::Loaded(vec![
                Entry::new("docs", EntryKind::Dir),
                Entry::new("src", EntryKind::Dir),
                Entry::new("Cargo.toml", EntryKind::File),
            ]),
        );
        tree.set_listing(
            "src".into(),
            Listing::Loaded(vec![
                Entry::new("ui", EntryKind::Dir),
                Entry::new("main.rs", EntryKind::File),
            ]),
        );
        tree.set_listing(
            "src/ui".into(),
            Listing::Loaded(vec![Entry::new("mod.rs", EntryKind::File)]),
        );
        tree
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn shape(rows: &[Row]) -> Vec<(String, usize, RowState)> {
        rows.iter()
            .map(|row| (row.rel.clone(), row.depth, row.state))
            .collect()
    }

    #[test]
    fn collapsed_tree_shows_the_root_entries() {
        let rows = flatten(&tree(), &HashSet::new());
        assert_eq!(
            shape(&rows),
            [
                ("docs".into(), 0, RowState::Collapsed),
                ("src".into(), 0, RowState::Collapsed),
                ("Cargo.toml".into(), 0, RowState::File),
            ]
        );
    }

    #[test]
    fn expanded_folders_nest_depth_first() {
        let rows = flatten(&tree(), &set(&["src", "src/ui"]));
        assert_eq!(
            shape(&rows),
            [
                ("docs".into(), 0, RowState::Collapsed),
                ("src".into(), 0, RowState::Expanded),
                ("src/ui".into(), 1, RowState::Expanded),
                ("src/ui/mod.rs".into(), 2, RowState::File),
                ("src/main.rs".into(), 1, RowState::File),
                ("Cargo.toml".into(), 0, RowState::File),
            ]
        );
    }

    #[test]
    fn a_closed_parent_hides_its_open_children() {
        let rows = flatten(&tree(), &set(&["src/ui"]));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn unread_and_unreadable_folders() {
        let mut tree = tree();
        tree.set_listing("docs".into(), Listing::Failed("denied".into()));
        let rows = flatten(&tree, &set(&["docs", "src"]));
        assert_eq!(rows[0].state, RowState::Failed);
        let tree = super::tests::tree();
        let rows = flatten(&tree, &set(&["docs"]));
        assert_eq!(rows[0].state, RowState::Loading);
        assert_eq!(unlisted(&tree, &rows), ["docs"]);
        assert_eq!(unlisted(&Tree::new(PathBuf::from("/x")), &[]), [""]);
    }

    #[test]
    fn up_and_down_stop_at_the_ends() {
        assert_eq!(moved(None, 1, 3), Some(0));
        assert_eq!(moved(None, -1, 3), Some(2));
        assert_eq!(moved(Some(2), 1, 3), Some(2));
        assert_eq!(moved(Some(1), -10, 3), Some(0));
        assert_eq!(moved(Some(0), 1, 0), None);
    }

    #[test]
    fn left_closes_then_climbs_and_right_opens_then_descends() {
        let expanded = set(&["src", "src/ui"]);
        let rows = flatten(&tree(), &expanded);
        // src/ui/mod.rs -> its folder; src/ui -> closed; docs -> nowhere.
        assert_eq!(left(&rows, 3), Nav::Select(2));
        assert_eq!(left(&rows, 2), Nav::Toggle("src/ui".into()));
        assert_eq!(left(&rows, 0), Nav::Nothing);
        assert_eq!(right(&rows, 0), Nav::Toggle("docs".into()));
        assert_eq!(right(&rows, 1), Nav::Select(2));
        assert_eq!(right(&rows, 5), Nav::Nothing);
    }

    #[test]
    fn shift_ranges_run_either_way() {
        let rows = flatten(&tree(), &set(&["src"]));
        // docs, src, src/ui, src/main.rs, Cargo.toml
        assert_eq!(range(&rows, "src/ui", "docs"), ["docs", "src", "src/ui"]);
        assert_eq!(range(&rows, "docs", "src"), ["docs", "src"]);
        assert_eq!(range(&rows, "gone", "src"), ["src"]);
        assert!(range(&rows, "docs", "gone").is_empty());
    }

    #[test]
    fn the_thumb_and_the_scroll_offset_agree() {
        assert_eq!(thumb(100., 10, 10, 0, 8.), None);
        let (offset, height) = thumb(100., 40, 10, 0, 8.).unwrap();
        assert_eq!((offset, height), (0., 25.));
        let (offset, _) = thumb(100., 40, 10, 30, 8.).unwrap();
        assert_eq!(offset, 75.);
        assert_eq!(scroll_for_thumb(100., 25., 40, 10, 75.), 30);
        assert_eq!(scroll_for_thumb(100., 25., 40, 10, 37.5), 15);
        assert_eq!(scroll_for_thumb(100., 25., 40, 10, -20.), 0);
        assert_eq!(scroll_for_thumb(100., 25., 40, 10, 500.), 30);
        // A tiny thumb is kept at its minimum height.
        assert_eq!(thumb(100., 10_000, 10, 0, 8.).unwrap().1, 8.);
    }

    #[test]
    fn scrolling_keeps_the_row_in_view() {
        assert_eq!(scroll_to_show(10, 4, 5), 4);
        assert_eq!(scroll_to_show(10, 12, 5), 10);
        assert_eq!(scroll_to_show(10, 17, 5), 13);
        assert_eq!(scroll_to_show(3, 7, 0), 7);
    }

    #[test]
    fn pruning_drops_only_folders_that_are_gone() {
        let tree = tree();
        let mut expanded = set(&["src", "src/ui", "gone", "docs/deep", ""]);
        prune_expanded(&tree, &mut expanded);
        // `docs/deep`: `docs` has not been read, so nothing is known yet.
        assert_eq!(expanded, set(&["src", "src/ui", "docs/deep"]));
    }
}
