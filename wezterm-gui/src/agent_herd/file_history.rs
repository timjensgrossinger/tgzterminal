//! A file as it was before an agent session first changed it.
//!
//! Claude Code backs up every file its edit tools are about to change, once
//! per session and turn, under
//! `<claude dir>/file-history/<session id>/<name>@v<N>`, where `<name>` is the
//! first 16 hex digits of the SHA-256 of the file's absolute path as the
//! session spelled it. `@v1` is the file before the session's first edit; a
//! file the session created has later versions but no `@v1`. Files changed
//! through the shell are not backed up, and neither is anything when the
//! user turned checkpointing off: both come back as [`Baseline::Untracked`].
//!
//! Only Claude keeps such a record; every other vendor is always untracked.
//!
//! Pure apart from the reads; no GUI, mux or terminal types.

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What the session's own record says a file was before it touched it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// Its content then, and when that copy was taken.
    Content(Vec<u8>, SystemTime),
    /// It did not exist: the session created it at about this time.
    Created(SystemTime),
    /// It was backed up, but the copy is larger than the caller will read.
    TooLarge(SystemTime),
    /// The session kept no copy.
    Untracked,
}

impl Baseline {
    /// When the session first touched the file, if it kept a record.
    pub fn taken_at(&self) -> Option<SystemTime> {
        match self {
            Self::Content(_, at) | Self::Created(at) | Self::TooLarge(at) => Some(*at),
            Self::Untracked => None,
        }
    }
}

/// The backups one Claude session kept, listed once so each file costs a set
/// lookup rather than a directory read.
#[derive(Clone, Debug, Default)]
pub struct ClaudeFileHistory {
    dir: PathBuf,
    names: HashSet<String>,
}

/// `<claude dir>` for a transcript at `<claude dir>/projects/<project>/<id>.jsonl`.
pub fn claude_dir_of_transcript(transcript: &Path) -> Option<&Path> {
    transcript.parent()?.parent()?.parent()
}

fn backup_name(abs_path: &str) -> String {
    Sha256::digest(abs_path.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl ClaudeFileHistory {
    /// The backups of `session_id` under `claude_dir`. Empty, not an error,
    /// when the session kept none.
    pub fn open(claude_dir: &Path, session_id: &str) -> Self {
        // A session id is a uuid; anything that could leave the directory is
        // not one.
        if session_id.is_empty() || session_id.contains(['/', '\\', '.']) {
            return Self::default();
        }
        let dir = claude_dir.join("file-history").join(session_id);
        let names = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Self { dir, names }
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// `abs_path` (spelled as the session spelled it) before the session's
    /// first edit. Reads at most `max_bytes`.
    pub fn baseline(&self, abs_path: &str, max_bytes: usize) -> Baseline {
        if self.names.is_empty() {
            return Baseline::Untracked;
        }
        let name = backup_name(abs_path);
        let first = format!("{name}@v1");
        if self.names.contains(&first) {
            let path = self.dir.join(&first);
            let Ok(meta) = std::fs::metadata(&path) else {
                return Baseline::Untracked;
            };
            let at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if meta.len() as usize > max_bytes {
                return Baseline::TooLarge(at);
            }
            return match std::fs::read(&path) {
                Ok(bytes) => Baseline::Content(bytes, at),
                Err(_) => Baseline::Untracked,
            };
        }
        // Later versions with no first one: the file did not exist when the
        // session first wrote it. The oldest one dates that.
        let prefix = format!("{name}@v");
        let created = self
            .names
            .iter()
            .filter(|candidate| candidate.starts_with(&prefix))
            .filter_map(|candidate| std::fs::metadata(self.dir.join(candidate)).ok())
            .filter_map(|meta| meta.modified().ok())
            .min();
        match created {
            Some(at) => Baseline::Created(at),
            None => Baseline::Untracked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(files: &[(&str, &str)]) -> (tempfile::TempDir, ClaudeFileHistory) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("file-history/sid-1");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
        let history = ClaudeFileHistory::open(tmp.path(), "sid-1");
        (tmp, history)
    }

    #[test]
    fn the_backup_name_is_the_path_digest() {
        // Checked against a real Claude Code backup.
        assert_eq!(
            backup_name(
                "/Users/tim.grossinger/Documents/tgzterminal/wezterm-gui/src/termwindow/render/sidebar.rs"
            ),
            "2977014a39b8c1f9"
        );
    }

    #[test]
    fn the_first_version_is_the_file_before_the_session() {
        let name = backup_name("/w/a.rs");
        let (_tmp, history) = history(&[
            (&format!("{name}@v1"), "before\n"),
            (&format!("{name}@v2"), "between\n"),
        ]);
        match history.baseline("/w/a.rs", 1024) {
            Baseline::Content(bytes, _) => assert_eq!(bytes, b"before\n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn later_versions_without_a_first_mean_the_session_created_it() {
        let name = backup_name("/w/new.rs");
        let (_tmp, history) = history(&[(&format!("{name}@v2"), "x")]);
        assert!(matches!(
            history.baseline("/w/new.rs", 1024),
            Baseline::Created(_)
        ));
    }

    #[test]
    fn unknown_files_oversized_copies_and_missing_sessions() {
        let name = backup_name("/w/big.bin");
        let (tmp, history) = history(&[(&format!("{name}@v1"), "0123456789")]);
        assert_eq!(history.baseline("/w/other.rs", 1024), Baseline::Untracked);
        assert!(matches!(
            history.baseline("/w/big.bin", 4),
            Baseline::TooLarge(_)
        ));
        let none = ClaudeFileHistory::open(tmp.path(), "no-such-session");
        assert!(none.is_empty());
        assert_eq!(none.baseline("/w/big.bin", 1024), Baseline::Untracked);
        assert!(ClaudeFileHistory::open(tmp.path(), "../sid-1").is_empty());
    }

    #[test]
    fn the_claude_dir_is_three_levels_above_the_transcript() {
        assert_eq!(
            claude_dir_of_transcript(Path::new("/h/.claude/projects/-w/abc.jsonl")),
            Some(Path::new("/h/.claude"))
        );
    }
}
