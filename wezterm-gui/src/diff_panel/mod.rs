//! The "Changes" panel: working-copy changes for a pane's directory.
//!
//! Like [`crate::agent_herd`], everything here is deliberately free of GUI,
//! mux and terminal types so it can be unit tested without a window. The
//! painting and the window-state glue live in
//! `termwindow/render/diff_panel.rs`.
//!
//! Three providers produce the same [`ChangeSet`]: Git and Subversion by
//! running their command-line clients and parsing the unified diff, and a
//! snapshot fallback for directories under neither.

pub mod exec;
pub mod geometry;
pub mod git;
pub mod snapshot;
pub mod svn;
pub mod unified;
pub mod view;
pub mod watch;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Lines kept per file. A generated file or a lockfile can change by tens of
/// thousands of lines; nobody reads that in a side panel.
pub const MAX_LINES_PER_FILE: usize = 4000;
/// Files read in full because the VCS has no diff for them (untracked,
/// unversioned). Beyond this they are listed without content.
pub const MAX_UNTRACKED_FILES: usize = 200;
/// How long after its last write a file still counts as just changed.
pub const FRESH_WINDOW: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeSource {
    Git { branch: Option<String> },
    Svn,
    Snapshot,
}

impl ChangeSource {
    pub fn label(&self) -> String {
        match self {
            Self::Git {
                branch: Some(branch),
            } => format!("Git \u{00b7} {branch}"),
            Self::Git { branch: None } => "Git".to_string(),
            Self::Svn => "SVN".to_string(),
            Self::Snapshot => "Snapshot".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
}

impl FileStatus {
    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Conflicted => "C",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// Path relative to the change set's root, with `/` separators.
    pub path: String,
    /// Where a renamed file came from.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: usize,
    pub removed: usize,
    pub hunks: Vec<Hunk>,
    /// Why there is no (or only part of a) diff: "binary", "too large", ...
    pub note: Option<String>,
    /// When the file on disk was last written; `None` when it is gone.
    pub modified: Option<SystemTime>,
    /// Written within [`FRESH_WINDOW`] of the scan: what the agent is
    /// touching right now.
    pub fresh: bool,
}

impl FileChange {
    pub fn new(path: impl Into<String>, status: FileStatus) -> Self {
        Self {
            path: path.into(),
            old_path: None,
            status,
            added: 0,
            removed: 0,
            hunks: Vec::new(),
            note: None,
            modified: None,
            fresh: false,
        }
    }

    /// A file with no previous version: every line of `text` is an addition.
    pub fn all_added(path: impl Into<String>, text: &str) -> Self {
        let mut file = Self::new(path, FileStatus::Added);
        let lines: Vec<DiffLine> = text
            .lines()
            .enumerate()
            .map(|(idx, line)| DiffLine {
                kind: LineKind::Added,
                old_no: None,
                new_no: Some(idx as u32 + 1),
                text: line.to_string(),
            })
            .collect();
        file.added = lines.len();
        if !lines.is_empty() {
            file.hunks.push(Hunk {
                old_start: 0,
                new_start: 1,
                lines,
            });
        }
        file.cap_lines();
        file
    }

    /// Drop everything past [`MAX_LINES_PER_FILE`] and say so.
    pub fn cap_lines(&mut self) {
        let mut budget = MAX_LINES_PER_FILE;
        let mut truncated = false;
        self.hunks.retain_mut(|hunk| {
            if budget == 0 {
                truncated = true;
                return false;
            }
            if hunk.lines.len() > budget {
                hunk.lines.truncate(budget);
                truncated = true;
            }
            budget -= hunk.lines.len();
            true
        });
        if truncated && self.note.is_none() {
            self.note = Some(format!("showing the first {MAX_LINES_PER_FILE} lines"));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub source: ChangeSource,
    pub root: PathBuf,
    pub files: Vec<FileChange>,
    /// Something the reader should know about the whole set.
    pub note: Option<String>,
}

impl ChangeSet {
    pub fn totals(&self) -> (usize, usize) {
        self.files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed))
    }
}

/// Put the most recently written files first, so a file an agent has just
/// touched rises to the top. Files that no longer exist keep their relative
/// order after the dated ones.
pub fn order_by_recency(set: &mut ChangeSet, now: SystemTime) {
    for file in &mut set.files {
        file.modified = std::fs::metadata(set.root.join(&file.path))
            .and_then(|meta| meta.modified())
            .ok();
        file.fresh = file
            .modified
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age <= FRESH_WINDOW);
    }
    // Stable, so files written in the same instant stay in provider order.
    set.files.sort_by(|a, b| match (a.modified, b.modified) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// What a scan found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    Changes(ChangeSet),
    /// No change set can be produced here, and why.
    Unavailable(String),
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_file_bytes: usize,
    pub snapshot_max_files: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vcs {
    Git,
    Svn,
}

/// The nearest ancestor of `dir` (itself included) that is a Git or
/// Subversion working copy root.
pub fn detect_vcs(dir: &Path) -> Option<(Vcs, PathBuf)> {
    let mut current = Some(dir);
    while let Some(dir) = current {
        // `.git` is a directory in a normal clone and a file in a worktree or
        // submodule; either marks the root.
        if dir.join(".git").exists() {
            return Some((Vcs::Git, dir.to_path_buf()));
        }
        if dir.join(".svn").is_dir() {
            return Some((Vcs::Svn, dir.to_path_buf()));
        }
        current = dir.parent();
    }
    None
}

/// Why a file's content is not shown, or its text.
pub fn read_text_file(path: &Path, max_bytes: usize) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    if meta.len() as usize > max_bytes {
        return Err("too large to show".to_string());
    }
    let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
    if looks_binary(&bytes) {
        return Err("binary".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// The working-copy changes for `dir`, from whichever provider applies.
///
/// `dir` is a path this process can open; `runner` says where the directory's
/// version-control client lives, which need not be this machine.
///
/// Blocking: runs child processes and walks the filesystem. Never call it on
/// the GUI thread.
pub fn scan(dir: &Path, runner: &exec::Runner, snapshot_store: &Path, limits: Limits) -> Scan {
    if !dir.is_dir() {
        return Scan::Unavailable("This pane's directory is not available".to_string());
    }
    let mut scan = match detect_vcs(dir) {
        Some((Vcs::Git, root)) => git::scan(&root, runner, limits),
        Some((Vcs::Svn, root)) => svn::scan(&root, runner, limits),
        None => snapshot::scan(dir, snapshot_store, limits),
    };
    if let Scan::Changes(set) = &mut scan {
        order_by_recency(set, SystemTime::now());
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_working_copy_root_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("vendor/inner");
        let deep = inner.join("src/lib");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        std::fs::create_dir_all(inner.join(".svn")).unwrap();

        assert_eq!(detect_vcs(&deep), Some((Vcs::Svn, inner.clone())));
        assert_eq!(
            detect_vcs(&outer.join("vendor")),
            Some((Vcs::Git, outer.clone()))
        );
    }

    #[test]
    fn a_git_file_marks_a_worktree_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            detect_vcs(tmp.path()),
            Some((Vcs::Git, tmp.path().to_path_buf()))
        );
    }

    #[test]
    fn the_most_recently_written_file_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        for (name, age) in [("old.txt", 3600), ("new.txt", 5), ("mid.txt", 600)] {
            let path = tmp.path().join(name);
            std::fs::write(&path, "x").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(age)).unwrap();
        }
        let mut set = ChangeSet {
            source: ChangeSource::Snapshot,
            root: tmp.path().to_path_buf(),
            files: ["old.txt", "gone.txt", "new.txt", "mid.txt"]
                .into_iter()
                .map(|path| FileChange::new(*path, FileStatus::Modified))
                .collect(),
            note: None,
        };
        order_by_recency(&mut set, now);
        let order: Vec<(&str, bool)> = set
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.fresh))
            .collect();
        assert_eq!(
            order,
            vec![
                ("new.txt", true),
                ("mid.txt", false),
                ("old.txt", false),
                ("gone.txt", false),
            ]
        );
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let file = FileChange::all_added("notes.txt", "one\ntwo\n");
        assert_eq!(file.status, FileStatus::Added);
        assert_eq!((file.added, file.removed), (2, 0));
        assert_eq!(file.hunks[0].lines[1].new_no, Some(2));
        assert_eq!(file.hunks[0].lines[1].text, "two");

        assert!(FileChange::all_added("empty", "").hunks.is_empty());
    }

    #[test]
    fn a_huge_file_is_capped_and_says_so() {
        let text = "x\n".repeat(MAX_LINES_PER_FILE + 10);
        let file = FileChange::all_added("big", &text);
        assert_eq!(file.hunks[0].lines.len(), MAX_LINES_PER_FILE);
        // The count still reports what changed, not what is shown.
        assert_eq!(file.added, MAX_LINES_PER_FILE + 10);
        assert!(file.note.unwrap().contains("first"));
    }

    #[test]
    fn binary_and_oversized_files_are_not_read_as_text() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("a.bin");
        std::fs::write(&bin, [1u8, 0, 2]).unwrap();
        assert_eq!(read_text_file(&bin, 1024).unwrap_err(), "binary");

        let big = tmp.path().join("big.txt");
        std::fs::write(&big, "x".repeat(100)).unwrap();
        assert_eq!(read_text_file(&big, 10).unwrap_err(), "too large to show");
        assert_eq!(read_text_file(&big, 1000).unwrap().len(), 100);
    }
}
