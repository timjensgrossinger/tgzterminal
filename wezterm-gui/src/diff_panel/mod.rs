//! The "Changes" panel: working-copy changes for a pane's directory.
//!
//! Like [`crate::agent_herd`], everything here is deliberately free of GUI,
//! mux and terminal types so it can be unit tested without a window. The
//! painting and the window-state glue live in
//! `termwindow/render/diff_panel.rs`.
//!
//! Three providers produce the same [`ChangeSet`]: Git and Subversion by
//! running their command-line clients and parsing the unified diff, and a
//! snapshot fallback for directories under neither. [`session`] builds one
//! from what a tab's agent sessions touched instead, without reading the
//! rest of the tree.

pub mod exec;
pub mod geometry;
pub mod git;
pub mod nested;
pub mod session;
pub mod snapshot;
pub mod svn;
pub mod unified;
pub mod view;
pub mod watch;

use crate::agent_herd::touched::{Touch, TouchedPaths};
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
    Git {
        branch: Option<String>,
    },
    Svn,
    Snapshot,
    /// Several working copies below a directory that is in none itself; see
    /// [`nested`]. The counts are working copies per client.
    Multi {
        git: usize,
        svn: usize,
    },
    /// What the agent sessions of a tab changed; see [`session`].
    Session {
        agents: usize,
    },
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
            Self::Multi { git, svn } => {
                let mut parts = Vec::new();
                if *git > 0 {
                    parts.push(format!("Git \u{00d7}{git}"));
                }
                if *svn > 0 {
                    parts.push(format!("SVN \u{00d7}{svn}"));
                }
                parts.join(" \u{00b7} ")
            }
            Self::Session { agents: 1 } => "Session".to_string(),
            Self::Session { agents } => format!("Session \u{00b7} {agents} agents"),
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
    /// How the pane's agent session touched this file, when the panel was
    /// told which session that is; see [`mark_session`].
    pub touched: Option<Touch>,
    /// The agents that touched it, by label; empty when none is known.
    pub agents: Vec<String>,
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
            touched: None,
            agents: Vec::new(),
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

/// Mark every file in `set` that the session's transcript says it touched,
/// adding `label` to its agents; returns how many there are.
///
/// `root_view` is `set.root` as the *session* saw it: for an agent inside a
/// WSL distro that is the Linux path (`/mnt/c/...`, `/home/...`), not the
/// path this process opened. A renamed file counts when either name was
/// touched.
pub fn mark_session(
    set: &mut ChangeSet,
    root_view: &str,
    touched: &TouchedPaths,
    label: &str,
) -> usize {
    let root = root_view.trim_end_matches(['/', '\\']);
    let sep = if root.contains('\\') && !root.contains('/') {
        '\\'
    } else {
        '/'
    };
    let touch = |path: &str| {
        let path = path.trim_end_matches('/');
        touched.touch_of(&format!("{root}{sep}{path}"))
    };
    let mut count = 0;
    for file in &mut set.files {
        let this = touch(&file.path).max(file.old_path.as_deref().and_then(touch));
        if this.is_some() {
            file.touched = file.touched.max(this);
            if !file.agents.iter().any(|agent| agent == label) {
                file.agents.push(label.to_string());
            }
            count += 1;
        }
    }
    count
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
    /// Levels below the pane directory searched for working copies when it is
    /// in none itself; `0` disables the search.
    pub nested_max_depth: usize,
    /// Most working copies that search collects.
    pub nested_max_roots: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
        // Before the snapshot: a flat workspace of module checkouts is in no
        // working copy itself, and far too big to snapshot.
        None => {
            let found =
                nested::discover_cached(dir, limits.nested_max_depth, limits.nested_max_roots);
            if nested::is_workspace(dir, &found) {
                nested::scan_all(dir, &found, runner, limits)
            } else {
                snapshot::scan(dir, snapshot_store, limits)
            }
        }
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
    fn marking_tags_the_files_a_session_touched() {
        let line = |name: &str, input: serde_json::Value| {
            serde_json::json!({
                "type": "assistant",
                "cwd": "/mnt/c/ws/CDP4JClient",
                "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] },
            })
            .to_string()
        };
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("s.jsonl");
        std::fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                line(
                    "Edit",
                    serde_json::json!({ "file_path": "/mnt/c/ws/CDP4JClient/src/A.java" })
                ),
                line("Bash", serde_json::json!({ "command": "svn mv src/Old.java src/New.java; cat texts.properties" })),
            ),
        )
        .unwrap();
        let touched = crate::agent_herd::touched::touched_by(&transcript).unwrap();

        let mut renamed = FileChange::new("CDP4JClient/src/New.java", FileStatus::Renamed);
        renamed.old_path = Some("CDP4JClient/src/Old.java".to_string());
        let mut set = ChangeSet {
            source: ChangeSource::Multi { git: 0, svn: 2 },
            // What this process opened; the session saw `/mnt/c/ws`.
            root: PathBuf::from(r"C:\ws"),
            files: vec![
                FileChange::new("CDP4JClient/src/A.java", FileStatus::Modified),
                renamed,
                FileChange::new("CDP4JClient/texts.properties", FileStatus::Modified),
                FileChange::new("Other/B.java", FileStatus::Modified),
            ],
            note: None,
        };
        assert_eq!(mark_session(&mut set, "/mnt/c/ws", &touched, "Claude"), 3);
        let shown: Vec<(&str, Option<Touch>, usize)> = set
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.touched, file.agents.len()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("CDP4JClient/src/A.java", Some(Touch::Edited), 1),
                ("CDP4JClient/src/New.java", Some(Touch::Edited), 1),
                ("CDP4JClient/texts.properties", Some(Touch::Mentioned), 1),
                ("Other/B.java", None, 0),
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
