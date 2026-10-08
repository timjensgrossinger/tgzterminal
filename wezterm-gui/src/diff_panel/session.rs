//! What the agent sessions of one tab changed, and nothing else.
//!
//! The other providers diff a whole working copy and then, at best, mark the
//! files a session touched. This one starts from the sessions instead: the
//! files their transcripts name (see [`crate::agent_herd::touched`]) are the
//! only ones read, so a directory of any size costs what the agents did in
//! it, and one in no working copy at all needs no snapshot baseline.
//!
//! Each file is compared with the oldest record of it from before an agent
//! touched it:
//! - a Claude session's own backup (see [`crate::agent_herd::file_history`]),
//!   which shows the agent's lines only, even in a file that already had
//!   uncommitted changes of the user's;
//! - otherwise the working copy's base, through the Git or Subversion client,
//!   narrowed to these files.
//!
//! A file only *named* in a shell command is shown when a client says it
//! changed: a `cat` is not an edit.
//!
//! Blocking: reads transcripts and files and runs clients. Never call it on
//! the GUI thread.

use super::exec::Runner;
use super::snapshot::diff_text;
use super::{
    detect_vcs, git, looks_binary, order_by_recency, svn, ChangeSet, ChangeSource, FileChange,
    FileStatus, Limits, Scan, Vcs,
};
use crate::agent_herd::file_history::{claude_dir_of_transcript, Baseline, ClaudeFileHistory};
use crate::agent_herd::touched::{path_key, touched_by, Touch};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Most files one session scan considers. A session that touched more has
/// run a code generator or a mass rename, and a side panel is not where that
/// is read.
pub const MAX_SESSION_FILES: usize = 2000;

/// Turns a path as a session spelled it into one this process can open.
pub type ToHost = Box<dyn Fn(&str) -> Option<PathBuf> + Send>;

/// One agent session whose changes are shown.
pub struct SessionSource {
    /// How the session is named next to its files.
    pub label: String,
    pub session_id: String,
    pub transcript: PathBuf,
    /// Whether the transcript is Claude's, whose sessions back files up.
    pub claude: bool,
    /// The project the session works in, as the session sees it: files
    /// outside it (its plan files, its memory) are not the project's changes.
    pub project_view: String,
    pub to_host: ToHost,
}

/// One touched file, gathered from every session that touched it.
struct Touched {
    host: PathBuf,
    touch: Touch,
    labels: Vec<String>,
    /// `(source index, path as that source spelled it)`.
    spellings: Vec<(usize, String)>,
}

/// The file now: gone, its text, or why it cannot be shown.
enum Current {
    Missing,
    Text(String),
    Unreadable(String),
}

fn current(path: &Path, max_bytes: usize) -> Current {
    match std::fs::metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Current::Missing,
        Err(err) => Current::Unreadable(err.to_string()),
        Ok(meta) if !meta.is_file() => Current::Unreadable("not a regular file".to_string()),
        Ok(meta) if meta.len() as usize > max_bytes => {
            Current::Unreadable("too large to show".to_string())
        }
        Ok(_) => match std::fs::read(path) {
            Ok(bytes) if looks_binary(&bytes) => Current::Unreadable("binary".to_string()),
            Ok(bytes) => Current::Text(String::from_utf8_lossy(&bytes).into_owned()),
            Err(err) => Current::Unreadable(err.to_string()),
        },
    }
}

fn is_under(key: &str, root_key: &str) -> bool {
    !root_key.is_empty()
        && (key == root_key
            || key
                .strip_prefix(root_key)
                .is_some_and(|rest| rest.starts_with('/'))
            || root_key == "/")
}

/// The deepest directory every one of `paths` is in.
fn common_ancestor(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter();
    let mut common: PathBuf = iter.next()?.clone();
    for path in iter {
        while !path.starts_with(&common) {
            if !common.pop() {
                return None;
            }
        }
    }
    Some(common)
}

/// `path` relative to `root` with `/` separators, or whole when it is not
/// under `root`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The file compared with a copy from before the agents, or `None` when
/// there is nothing to show (unchanged, or created and removed again).
fn against_baseline(path: String, baseline: Baseline, now: Current) -> Option<FileChange> {
    match baseline {
        Baseline::Content(before, _) => {
            if looks_binary(&before) {
                let mut file = FileChange::new(path, FileStatus::Modified);
                file.note = Some("binary".to_string());
                return match now {
                    Current::Missing => {
                        file.status = FileStatus::Deleted;
                        file.note = None;
                        Some(file)
                    }
                    _ => Some(file),
                };
            }
            let before = String::from_utf8_lossy(&before).into_owned();
            let (status, after) = match now {
                Current::Missing => (FileStatus::Deleted, String::new()),
                Current::Text(text) => (FileStatus::Modified, text),
                Current::Unreadable(reason) => {
                    let mut file = FileChange::new(path, FileStatus::Modified);
                    file.note = Some(reason);
                    return Some(file);
                }
            };
            if before == after {
                return None;
            }
            let (hunks, added, removed) = diff_text(&before, &after);
            let mut file = FileChange::new(path, status);
            file.hunks = hunks;
            file.added = added;
            file.removed = removed;
            file.cap_lines();
            Some(file)
        }
        Baseline::Created(_) => match now {
            Current::Missing => None,
            Current::Text(text) => Some(FileChange::all_added(path, &text)),
            Current::Unreadable(reason) => {
                let mut file = FileChange::new(path, FileStatus::Added);
                file.note = Some(reason);
                Some(file)
            }
        },
        Baseline::TooLarge(_) => {
            let mut file = FileChange::new(path, FileStatus::Modified);
            if matches!(now, Current::Missing) {
                file.status = FileStatus::Deleted;
            } else {
                file.note = Some("too large to show".to_string());
            }
            Some(file)
        }
        Baseline::Untracked => None,
    }
}

/// Every change the sessions in `sources` made, as one change set.
///
/// `runner` says where version-control clients live; each working copy gets
/// a fork of it.
pub fn scan_session(sources: &[SessionSource], runner: &Runner, limits: Limits) -> Scan {
    let mut notes: Vec<String> = Vec::new();
    let mut touched: BTreeMap<String, Touched> = BTreeMap::new();
    let mut outside: std::collections::HashSet<String> = Default::default();
    let mut capped = false;
    let mut roots: Vec<PathBuf> = Vec::new();

    for (idx, source) in sources.iter().enumerate() {
        if let Some(root) = (source.to_host)(&source.project_view) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let paths = match touched_by(&source.transcript) {
            Ok(paths) => paths,
            Err(err) => {
                log::debug!(
                    "session changes: cannot read {}: {err}",
                    source.transcript.display()
                );
                continue;
            }
        };
        let project = path_key(&source.project_view);
        for (key, spelled, touch) in paths.iter() {
            if !is_under(key, &project) {
                outside.insert(key.to_string());
                continue;
            }
            let Some(host) = (source.to_host)(spelled) else {
                continue;
            };
            let host_key = path_key(&host.to_string_lossy());
            if !touched.contains_key(&host_key) && touched.len() >= MAX_SESSION_FILES {
                capped = true;
                continue;
            }
            let entry = touched.entry(host_key).or_insert_with(|| Touched {
                host,
                touch,
                labels: Vec::new(),
                spellings: Vec::new(),
            });
            entry.touch = entry.touch.max(touch);
            if !entry.labels.contains(&source.label) {
                entry.labels.push(source.label.clone());
            }
            entry.spellings.push((idx, spelled.to_string()));
        }
    }

    let Some(root) = common_ancestor(&roots) else {
        return Scan::Unavailable("The agents in this tab have not said where they work".into());
    };

    // Claude's backups, opened once per session that keeps them.
    let histories: Vec<Option<ClaudeFileHistory>> = sources
        .iter()
        .map(|source| {
            if !source.claude {
                return None;
            }
            let dir = claude_dir_of_transcript(&source.transcript)?;
            Some(ClaudeFileHistory::open(dir, &source.session_id)).filter(|h| !h.is_empty())
        })
        .collect();

    let mut files: Vec<FileChange> = Vec::new();
    // Files no session backed up, by the working copy they are in.
    let mut by_copy: HashMap<(Vcs, PathBuf), Vec<String>> = HashMap::new();
    let mut vcs_of_dir: HashMap<PathBuf, Option<(Vcs, PathBuf)>> = HashMap::new();
    let mut pending: HashMap<String, Touched> = HashMap::new();

    for (host_key, entry) in touched {
        let baseline = entry
            .spellings
            .iter()
            .filter_map(|(idx, spelled)| {
                let history = histories.get(*idx)?.as_ref()?;
                Some(history.baseline(spelled, limits.max_file_bytes))
            })
            .filter(|baseline| baseline.taken_at().is_some())
            .min_by_key(|baseline| baseline.taken_at().unwrap_or(SystemTime::UNIX_EPOCH));
        if let Some(baseline) = baseline {
            let now = current(&entry.host, limits.max_file_bytes);
            if let Some(mut file) = against_baseline(relative(&root, &entry.host), baseline, now) {
                file.touched = Some(entry.touch);
                file.agents = entry.labels.clone();
                files.push(file);
            }
            continue;
        }
        let dir = entry
            .host
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let copy = vcs_of_dir
            .entry(dir.clone())
            .or_insert_with(|| detect_vcs(&dir))
            .clone();
        match copy {
            Some((vcs, copy_root)) => {
                by_copy
                    .entry((vcs, copy_root.clone()))
                    .or_default()
                    .push(relative(&copy_root, &entry.host));
                pending.insert(host_key, entry);
            }
            None if entry.touch == Touch::Edited && entry.host.is_file() => {
                let mut file = FileChange::new(relative(&root, &entry.host), FileStatus::Modified);
                file.note = Some("no earlier copy to compare with".to_string());
                file.touched = Some(entry.touch);
                file.agents = entry.labels.clone();
                files.push(file);
            }
            // Gone, or only named in a command: nothing to say it changed.
            None => {}
        }
    }

    let mut copies: Vec<((Vcs, PathBuf), Vec<String>)> = by_copy.into_iter().collect();
    copies.sort_by(|a, b| a.0 .1.cmp(&b.0 .1));
    for ((vcs, copy_root), paths) in copies {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let scan = match vcs {
            Vcs::Git => git::scan_paths(&copy_root, &runner.fork(), &paths, limits),
            Vcs::Svn => svn::scan_paths(&copy_root, &runner.fork(), &paths, limits),
        };
        match scan {
            Scan::Changes(set) => {
                if let Some(note) = set.note {
                    notes.push(note);
                }
                for mut file in set.files {
                    let host = copy_root.join(&file.path);
                    let key = path_key(&host.to_string_lossy());
                    let old_key = file
                        .old_path
                        .as_ref()
                        .map(|old| path_key(&copy_root.join(old).to_string_lossy()));
                    let entry = pending
                        .get(&key)
                        .or_else(|| old_key.and_then(|old| pending.get(&old)));
                    let Some(entry) = entry else {
                        continue;
                    };
                    file.touched = Some(entry.touch);
                    file.agents = entry.labels.clone();
                    file.path = relative(&root, &host);
                    file.old_path = file
                        .old_path
                        .map(|old| relative(&root, &copy_root.join(old)));
                    files.push(file);
                }
            }
            Scan::Unavailable(reason) => {
                notes.push(format!("{}: {reason}", copy_root.display()));
            }
        }
    }

    for file in &mut files {
        if file.touched == Some(Touch::Mentioned) && file.note.is_none() {
            file.note = Some("named in a shell command".to_string());
        }
    }
    if capped {
        notes.push(format!(
            "the agents touched more than {MAX_SESSION_FILES} files; showing the first"
        ));
    }
    if !outside.is_empty() {
        notes.push(format!(
            "{} touched {} outside the project not shown",
            outside.len(),
            if outside.len() == 1 { "file" } else { "files" }
        ));
    }
    let mut set = ChangeSet {
        source: ChangeSource::Session {
            agents: sources.len(),
        },
        root,
        files,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    };
    order_by_recency(&mut set, SystemTime::now());
    Scan::Changes(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_panel::LineKind;
    use std::time::Duration;

    fn limits() -> Limits {
        Limits {
            max_file_bytes: 1024 * 1024,
            snapshot_max_files: 10,
            nested_max_depth: 0,
            nested_max_roots: 0,
        }
    }

    fn backup_name(path: &str) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(path.as_bytes())
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A Claude session under `claude/` editing `edits` and running
    /// `commands`, with `backups` as `(path, version, content, age)`.
    struct Fixture {
        claude: PathBuf,
    }

    impl Fixture {
        fn new(tmp: &Path) -> Self {
            Self {
                claude: tmp.join("claude"),
            }
        }

        fn session(
            &self,
            sid: &str,
            cwd: &Path,
            edits: &[&Path],
            commands: &[&str],
            backups: &[(&Path, u32, Option<&str>, u64)],
        ) -> SessionSource {
            let project = self.claude.join("projects/p");
            std::fs::create_dir_all(&project).unwrap();
            let line = |name: &str, input: serde_json::Value| {
                serde_json::json!({
                    "type": "assistant",
                    "cwd": cwd.to_string_lossy(),
                    "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] },
                })
                .to_string()
            };
            let mut text = String::new();
            for path in edits {
                text += &line(
                    "Edit",
                    serde_json::json!({ "file_path": path.to_string_lossy() }),
                );
                text += "\n";
            }
            for command in commands {
                text += &line("Bash", serde_json::json!({ "command": command }));
                text += "\n";
            }
            let transcript = project.join(format!("{sid}.jsonl"));
            std::fs::write(&transcript, text).unwrap();

            let history = self.claude.join("file-history").join(sid);
            std::fs::create_dir_all(&history).unwrap();
            for (path, version, content, age) in backups {
                let name = format!("{}@v{version}", backup_name(&path.to_string_lossy()));
                let file = history.join(name);
                std::fs::write(&file, content.unwrap_or("")).unwrap();
                std::fs::File::options()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(*age))
                    .unwrap();
            }
            SessionSource {
                label: sid.to_string(),
                session_id: sid.to_string(),
                transcript,
                claude: true,
                project_view: cwd.to_string_lossy().into_owned(),
                to_host: Box::new(|path| Some(PathBuf::from(path))),
            }
        }
    }

    fn changes(sources: &[SessionSource]) -> ChangeSet {
        match scan_session(sources, &Runner::host(), limits()) {
            Scan::Changes(set) => set,
            Scan::Unavailable(reason) => panic!("unavailable: {reason}"),
        }
    }

    fn file<'a>(set: &'a ChangeSet, path: &str) -> &'a FileChange {
        set.files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} not in {:?}", set.files))
    }

    #[test]
    fn only_the_agents_lines_show_in_a_file_the_user_had_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = work.join("a.txt");
        // The user's own uncommitted line is already in the backup.
        std::fs::write(&a, "base\nuser\nagent\n").unwrap();
        let fx = Fixture::new(tmp.path());
        let source = fx.session(
            "s1",
            &work,
            &[&a],
            &[],
            &[(&a, 1, Some("base\nuser\n"), 60)],
        );

        let set = changes(&[source]);
        assert_eq!(set.source, ChangeSource::Session { agents: 1 });
        let a = file(&set, "a.txt");
        assert_eq!((a.status, a.added, a.removed), (FileStatus::Modified, 1, 0));
        let added: Vec<&str> = a.hunks[0]
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::Added)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(added, vec!["agent"]);
        assert_eq!(a.agents, vec!["s1".to_string()]);
    }

    #[test]
    fn created_reverted_and_merely_named_files() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (new, gone, same, read, edited) = (
            work.join("new.txt"),
            work.join("gone.txt"),
            work.join("same.txt"),
            work.join("read.txt"),
            work.join("shell.txt"),
        );
        std::fs::write(&new, "fresh\n").unwrap();
        std::fs::write(&same, "kept\n").unwrap();
        std::fs::write(&read, "data\n").unwrap();
        std::fs::write(&edited, "changed\n").unwrap();
        let fx = Fixture::new(tmp.path());
        let source = fx.session(
            "s1",
            &work,
            &[&new, &gone, &same],
            &["cat read.txt", "sed -i s/a/b/ shell.txt"],
            &[
                (&new, 2, Some("fresh\n"), 30),
                (&gone, 2, Some("x\n"), 30),
                (&same, 1, Some("kept\n"), 30),
            ],
        );
        let set = changes(&[source]);
        let mut shown: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        shown.sort();
        // Created then removed, unchanged, and only read: all left out. The
        // shell edit has no copy to compare with and says so.
        assert_eq!(shown, vec!["new.txt", "shell.txt"]);
        assert_eq!(file(&set, "new.txt").status, FileStatus::Added);
        assert!(file(&set, "shell.txt")
            .note
            .as_deref()
            .unwrap()
            .contains("no earlier copy"));
    }

    #[test]
    fn the_oldest_backup_wins_and_every_agent_is_named() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let a = work.join("a.txt");
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        let fx = Fixture::new(tmp.path());
        let first = fx.session("s1", &work, &[&a], &[], &[(&a, 1, Some("one\n"), 600)]);
        let second = fx.session("s2", &work, &[&a], &[], &[(&a, 1, Some("one\ntwo\n"), 60)]);

        let set = changes(&[first, second]);
        let a = file(&set, "a.txt");
        assert_eq!((a.added, a.removed), (2, 0));
        assert_eq!(a.agents, vec!["s1".to_string(), "s2".to_string()]);
        assert_eq!(set.source.label(), "Session \u{00b7} 2 agents");
    }

    #[test]
    fn files_outside_the_project_are_counted_not_shown() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        let plans = tmp.path().join("plans");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&plans).unwrap();
        let plan = plans.join("plan.md");
        std::fs::write(&plan, "x\n").unwrap();
        let fx = Fixture::new(tmp.path());
        let source = fx.session("s1", &work, &[&plan], &[], &[(&plan, 2, None, 5)]);
        let set = changes(&[source]);
        assert!(set.files.is_empty());
        assert!(set
            .note
            .unwrap()
            .contains("1 touched file outside the project"));
    }

    #[test]
    fn a_tree_too_big_to_snapshot_still_shows_the_session() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        // More files than `snapshot_max_files`, and no working copy.
        for idx in 0..50 {
            std::fs::write(work.join(format!("f{idx}.txt")), "x\n").unwrap();
        }
        let a = work.join("f7.txt");
        std::fs::write(&a, "x\ny\n").unwrap();
        let fx = Fixture::new(tmp.path());
        let source = fx.session("s1", &work, &[&a], &[], &[(&a, 1, Some("x\n"), 5)]);
        let set = changes(&[source]);
        assert_eq!(set.files.len(), 1);
        assert_eq!(file(&set, "f7.txt").added, 1);
    }

    #[test]
    fn the_common_root_holds_every_project() {
        assert_eq!(
            common_ancestor(&[PathBuf::from("/w/a/x"), PathBuf::from("/w/b")]),
            Some(PathBuf::from("/w"))
        );
        assert!(is_under("/w/a/b.rs", "/w/a"));
        assert!(!is_under("/w/ab/b.rs", "/w/a"));
        assert!(!is_under("/w/a/b.rs", ""));
    }
}
