//! Fuzzy filtering of the tree: every file under the root is matched against
//! the query, and the matches are shown as a tree of their own, with every
//! folder on the way to a match open.

use super::{join_rel, sort_entries, Entry, EntryKind, Listing, Tree};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Most matches shown: past this many the query is not narrowing anything.
pub const MAX_MATCHES: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub rel: String,
    pub score: u32,
    /// Matched characters of the file's name, as char offsets into it.
    pub name_chars: Vec<usize>,
}

/// The files of `index` matching `query`, best first; at most `limit`.
pub fn matches(index: &[String], query: &str, limit: usize) -> Vec<Match> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    // Score everything, then work out highlights for the few that are shown:
    // finding the matched characters costs several times a plain score.
    let mut scored: Vec<(u32, &String)> = index
        .iter()
        .filter_map(|rel| {
            let score = pattern.score(Utf32Str::new(rel, &mut buf), &mut matcher)?;
            Some((score, rel))
        })
        .collect();
    // Best score first; among equals, the shorter (shallower) path.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.len().cmp(&b.1.len()))
            .then_with(|| a.1.cmp(b.1))
    });
    scored.truncate(limit);
    let mut indices = Vec::new();
    scored
        .into_iter()
        .map(|(score, rel)| {
            indices.clear();
            pattern.indices(Utf32Str::new(rel, &mut buf), &mut matcher, &mut indices);
            let name_start = rel.rfind('/').map_or(0, |at| rel[..=at].chars().count());
            let mut name_chars: Vec<usize> = indices
                .iter()
                .filter_map(|at| (*at as usize).checked_sub(name_start))
                .collect();
            name_chars.sort_unstable();
            name_chars.dedup();
            Match {
                rel: rel.clone(),
                score,
                name_chars,
            }
        })
        .collect()
}

/// A tree holding only `matches` and the folders leading to them, and the
/// set of those folders, all open.
pub fn match_tree(root: PathBuf, matches: &[Match]) -> (Tree, HashSet<String>) {
    let mut children: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    children.entry(String::new()).or_default();
    for found in matches {
        let parts: Vec<&str> = found.rel.split('/').collect();
        let mut parent = String::new();
        for (depth, name) in parts.iter().enumerate() {
            let is_file = depth + 1 == parts.len();
            let rel = join_rel(&parent, name);
            if seen.insert(rel.clone()) {
                let kind = if is_file {
                    EntryKind::File
                } else {
                    EntryKind::Dir
                };
                children.entry(parent.clone()).or_default().push(Entry {
                    name: name.to_string(),
                    kind,
                    symlink: false,
                });
                if !is_file {
                    children.entry(rel.clone()).or_default();
                }
            }
            parent = rel;
        }
    }
    let mut tree = Tree::new(root);
    let mut open = HashSet::new();
    for (dir, mut entries) in children {
        sort_entries(&mut entries);
        if !dir.is_empty() {
            open.insert(dir.clone());
        }
        tree.set_listing(dir, Listing::Loaded(entries));
    }
    (tree, open)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_tree::view::flatten;

    fn index() -> Vec<String> {
        [
            "Cargo.toml",
            "src/main.rs",
            "src/termwindow/render/file_tree.rs",
            "src/file_tree/view.rs",
            "docs/TREE.md",
        ]
        .iter()
        .map(|rel| rel.to_string())
        .collect()
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(matches(&index(), "  ", 10).is_empty());
    }

    #[test]
    fn matches_are_fuzzy_and_ranked() {
        let found = matches(&index(), "filetree", 10);
        let rels: Vec<&str> = found.iter().map(|m| m.rel.as_str()).collect();
        assert!(rels.contains(&"src/termwindow/render/file_tree.rs"));
        assert!(rels.contains(&"src/file_tree/view.rs"));
        assert!(!rels.contains(&"Cargo.toml"));
        assert_eq!(matches(&index(), "tree", 1).len(), 1);
    }

    #[test]
    fn name_highlights_are_offsets_into_the_name() {
        let found = matches(&index(), "main", 10);
        let main = found.iter().find(|m| m.rel == "src/main.rs").unwrap();
        assert_eq!(main.name_chars, [0, 1, 2, 3]);
    }

    #[test]
    fn smart_case() {
        assert!(matches(&index(), "TREE", 10)
            .iter()
            .all(|m| m.rel == "docs/TREE.md"));
    }

    #[test]
    fn the_match_tree_opens_every_folder_on_the_way() {
        let found = matches(&index(), "view", 10);
        let (tree, open) = match_tree(PathBuf::from("/r"), &found);
        let rows = flatten(&tree, &open);
        let rels: Vec<&str> = rows.iter().map(|row| row.rel.as_str()).collect();
        assert_eq!(rels, ["src", "src/file_tree", "src/file_tree/view.rs"]);
    }
}
