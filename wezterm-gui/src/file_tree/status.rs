//! Marks beside the tree's rows: what git says about each file, and which
//! files the agents in the tab touched. Both roll up onto folders, so a
//! closed folder still says something inside it changed.

use super::parent_rel;
use crate::agent_herd::touched::{path_key, Touch, TouchedPaths};
use std::collections::HashMap;

/// A file's git status, in rising order of how much it wants attention: a
/// folder shows the strongest mark among its contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Badge {
    Untracked,
    Added,
    Renamed,
    Modified,
    Deleted,
    Conflict,
}

impl Badge {
    pub fn letter(self) -> &'static str {
        match self {
            Badge::Untracked => "U",
            Badge::Added => "A",
            Badge::Renamed => "R",
            Badge::Modified => "M",
            Badge::Deleted => "D",
            Badge::Conflict => "!",
        }
    }

    fn from_xy(x: u8, y: u8) -> Option<Self> {
        Some(match (x, y) {
            (b'?', b'?') => Badge::Untracked,
            (b'!', b'!') => return None,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => Badge::Conflict,
            (b'D', _) | (_, b'D') => Badge::Deleted,
            (b'M', _) | (_, b'M') | (b'T', _) | (_, b'T') => Badge::Modified,
            (b'R', _) | (b'C', _) => Badge::Renamed,
            (b'A', _) => Badge::Added,
            _ => return None,
        })
    }
}

/// `git status` for the tree's root, which is the repository's top level.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitStatus {
    files: HashMap<String, Badge>,
    dirs: HashMap<String, Badge>,
    /// Folders git reports as untracked as a whole.
    untracked_dirs: Vec<String>,
}

impl GitStatus {
    /// Read `git status --porcelain=v1 -z` output.
    pub fn parse(output: &str) -> Self {
        let mut status = Self::default();
        let mut records = output.split('\0').filter(|record| !record.is_empty());
        while let Some(record) = records.next() {
            let bytes = record.as_bytes();
            if bytes.len() < 4 {
                continue;
            }
            let (x, y) = (bytes[0], bytes[1]);
            let path = &record[3..];
            // A rename or copy is followed by the name it had.
            if matches!(x, b'R' | b'C') {
                records.next();
            }
            let Some(badge) = Badge::from_xy(x, y) else {
                continue;
            };
            if let Some(dir) = path.strip_suffix('/') {
                status.untracked_dirs.push(dir.to_string());
                status.roll_up(dir, badge, true);
            } else {
                status.files.insert(path.to_string(), badge);
                status.roll_up(path, badge, false);
            }
        }
        status
    }

    fn roll_up(&mut self, rel: &str, badge: Badge, include_self: bool) {
        if include_self {
            raise(&mut self.dirs, rel, badge);
        }
        let mut current = rel;
        while let Some(parent) = parent_rel(current).filter(|parent| !parent.is_empty()) {
            raise(&mut self.dirs, parent, badge);
            current = parent;
        }
    }

    pub fn file(&self, rel: &str) -> Option<Badge> {
        if let Some(badge) = self.files.get(rel) {
            return Some(*badge);
        }
        self.untracked_dirs
            .iter()
            .any(|dir| {
                rel.strip_prefix(dir.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            .then_some(Badge::Untracked)
    }

    pub fn dir(&self, rel: &str) -> Option<Badge> {
        self.dirs.get(rel).copied().or_else(|| self.file(rel))
    }
}

fn raise<K: AsRef<str>, V: Ord + Copy>(map: &mut HashMap<String, V>, key: K, value: V) {
    let entry = map.entry(key.as_ref().to_string()).or_insert(value);
    *entry = (*entry).max(value);
}

/// Files the tab's agents touched, by the folded path keys of
/// [`path_key`], with their folders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentMarks {
    /// The tree's root as each session sees it (a Linux path for a session
    /// inside a WSL distro).
    root_views: Vec<String>,
    files: HashMap<String, Touch>,
    dirs: HashMap<String, Touch>,
}

impl AgentMarks {
    /// Add what one session touched under `root_view`.
    pub fn add(&mut self, root_view: &str, touched: &TouchedPaths) {
        let root_key = path_key(root_view);
        for (key, _, touch) in touched.iter() {
            let Some(tail) = key
                .strip_prefix(root_key.as_str())
                .and_then(|tail| tail.strip_prefix('/'))
            else {
                continue;
            };
            raise(&mut self.files, key, touch);
            let mut current = tail;
            while let Some(parent) = parent_rel(current).filter(|parent| !parent.is_empty()) {
                raise(&mut self.dirs, format!("{root_key}/{parent}"), touch);
                current = parent;
            }
        }
        if !self.root_views.iter().any(|known| known == root_view) {
            self.root_views.push(root_view.to_string());
        }
    }

    /// How the agents touched `rel` (for a folder: anything inside it).
    pub fn of(&self, rel: &str, dir: bool) -> Option<Touch> {
        let map = if dir { &self.dirs } else { &self.files };
        if map.is_empty() {
            return None;
        }
        self.root_views
            .iter()
            .filter_map(|root| map.get(&path_key(&format!("{root}/{rel}"))).copied())
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_badges_roll_up_onto_folders() {
        let out = " M src/main.rs\0A  src/ui/new.rs\0?? scratch/\0R  docs/b.md\0docs/a.md\0D  gone.txt\0UU src/ui/merge.rs\0";
        let status = GitStatus::parse(out);
        assert_eq!(status.file("src/main.rs"), Some(Badge::Modified));
        assert_eq!(status.file("src/ui/new.rs"), Some(Badge::Added));
        assert_eq!(status.file("docs/b.md"), Some(Badge::Renamed));
        // The old name of a rename is not a record of its own.
        assert_eq!(status.file("docs/a.md"), None);
        assert_eq!(status.file("scratch/deep/x.txt"), Some(Badge::Untracked));
        assert_eq!(status.dir("scratch"), Some(Badge::Untracked));
        assert_eq!(status.dir("src/ui"), Some(Badge::Conflict));
        assert_eq!(status.dir("src"), Some(Badge::Conflict));
        assert_eq!(status.dir("docs"), Some(Badge::Renamed));
        assert_eq!(status.file("Cargo.toml"), None);
        assert_eq!(status.dir("scratchpad"), None);
    }

    #[test]
    fn modified_beats_added_in_a_folder() {
        let status = GitStatus::parse("A  a/x\0 M a/y\0");
        assert_eq!(status.dir("a"), Some(Badge::Modified));
    }

    #[test]
    fn short_or_ignored_records_are_skipped() {
        let status = GitStatus::parse("!! target/\0x\0");
        assert_eq!(status, GitStatus::default());
    }

    #[test]
    fn agent_marks_match_files_and_their_folders() {
        let mut touched = TouchedPaths::default();
        touched.insert_for_test("/repo/src/ui/mod.rs", Touch::Edited);
        touched.insert_for_test("/repo/README.md", Touch::Mentioned);
        touched.insert_for_test("/elsewhere/x.rs", Touch::Edited);
        let mut marks = AgentMarks::default();
        marks.add("/repo", &touched);
        assert_eq!(marks.of("src/ui/mod.rs", false), Some(Touch::Edited));
        assert_eq!(marks.of("src", true), Some(Touch::Edited));
        assert_eq!(marks.of("src/ui", true), Some(Touch::Edited));
        assert_eq!(marks.of("README.md", false), Some(Touch::Mentioned));
        assert_eq!(marks.of("src/main.rs", false), None);
        assert_eq!(marks.of("x.rs", false), None);
    }

    #[test]
    fn agent_marks_fold_windows_paths() {
        let mut touched = TouchedPaths::default();
        touched.insert_for_test(r"C:\Repo\Src\Main.rs", Touch::Edited);
        let mut marks = AgentMarks::default();
        marks.add(r"C:\Repo", &touched);
        assert_eq!(marks.of("Src/Main.rs", false), Some(Touch::Edited));
        assert_eq!(marks.of("Src", true), Some(Touch::Edited));
    }
}
