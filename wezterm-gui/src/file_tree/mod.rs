//! The Worktree panel's model: a directory tree listed one folder at a time,
//! as folders are opened.
//!
//! Deliberately free of GUI, mux and terminal types, like `diff_panel`, so it
//! can be tested on its own. Listing runs off the GUI thread
//! ([`list::list_dir`]); the window glue only stores what comes back and
//! flattens it into rows ([`view::flatten`]).
//!
//! Paths inside the tree are relative to its root, `/`-separated whatever the
//! platform, with `""` for the root itself.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::PathBuf;

pub mod editor;
pub mod filter;
pub mod list;
pub mod ops;
pub mod preview;
pub mod status;
pub mod trash;
pub mod view;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Reached through a symbolic link; `kind` is what the link points at.
    pub symlink: bool,
}

impl Entry {
    #[cfg(test)]
    pub fn new(name: impl Into<String>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            kind,
            symlink: false,
        }
    }
}

/// What reading one folder produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    Loaded(Vec<Entry>),
    /// The folder could not be read; the text says why.
    Failed(String),
}

/// The folders read so far under one root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub root: PathBuf,
    dirs: HashMap<String, Listing>,
}

impl Tree {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            dirs: HashMap::new(),
        }
    }

    pub fn listing(&self, rel: &str) -> Option<&Listing> {
        self.dirs.get(rel)
    }

    pub fn is_listed(&self, rel: &str) -> bool {
        self.dirs.contains_key(rel)
    }

    pub fn set_listing(&mut self, rel: String, listing: Listing) {
        self.dirs.insert(rel, listing);
    }

    /// `rel` as a path on this machine.
    pub fn abs_path(&self, rel: &str) -> PathBuf {
        let mut path = self.root.clone();
        for part in rel.split('/').filter(|part| !part.is_empty()) {
            path.push(part);
        }
        path
    }
}

/// The relative path of `name` inside the folder `parent`.
pub fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// The folder holding `rel`: `None` for the root, `""` for a top-level entry.
pub fn parent_rel(rel: &str) -> Option<&str> {
    if rel.is_empty() {
        return None;
    }
    Some(rel.rsplit_once('/').map_or("", |(parent, _)| parent))
}

/// Folders first, then by name as people read it: case folded, and runs of
/// digits compared by value so `file2` sorts before `file10`.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let kind = |entry: &Entry| (entry.kind != EntryKind::Dir) as u8;
        kind(a)
            .cmp(&kind(b))
            .then_with(|| natural_cmp(&a.name, &b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (xt, yt) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn folders_come_first_then_natural_order() {
        let mut entries = vec![
            Entry::new("file10.rs", EntryKind::File),
            Entry::new("README.md", EntryKind::File),
            Entry::new("src", EntryKind::Dir),
            Entry::new("file2.rs", EntryKind::File),
            Entry::new("Docs", EntryKind::Dir),
            Entry::new("alpha", EntryKind::File),
        ];
        sort_entries(&mut entries);
        assert_eq!(
            names(&entries),
            ["Docs", "src", "alpha", "file2.rs", "file10.rs", "README.md"]
        );
    }

    #[test]
    fn leading_zeros_do_not_change_the_value() {
        assert_eq!(natural_cmp("v007", "v7"), Ordering::Equal);
        assert_eq!(natural_cmp("v9", "v010"), Ordering::Less);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(join_rel("", "src"), "src");
        assert_eq!(join_rel("src", "main.rs"), "src/main.rs");
        assert_eq!(parent_rel(""), None);
        assert_eq!(parent_rel("src"), Some(""));
        assert_eq!(parent_rel("src/a/b.rs"), Some("src/a"));
    }

    #[test]
    fn abs_path_joins_components() {
        let tree = Tree::new(PathBuf::from("/repo"));
        assert_eq!(tree.abs_path(""), PathBuf::from("/repo"));
        assert_eq!(
            tree.abs_path("src/main.rs"),
            PathBuf::from("/repo").join("src").join("main.rs")
        );
    }
}
