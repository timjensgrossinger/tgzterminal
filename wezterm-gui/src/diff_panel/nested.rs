//! Working copies *below* a directory that is itself in none.
//!
//! A flat workspace — `workspaceTrunk/` holding one Subversion checkout per
//! module, or a folder of Git clones — has no `.svn` or `.git` of its own, so
//! the upward search in [`super::detect_vcs`] finds nothing and the snapshot
//! fallback gives up on the thousands of files under it. Every module is a
//! working copy, though; collecting them and showing their changes together
//! answers the question the panel exists for.

use super::exec::Runner;
use super::{git, snapshot, svn, ChangeSet, ChangeSource, Limits, Scan, Vcs};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Directories examined before the search gives up, whatever the depth. A
/// pane sitting in a home directory must not walk the whole disk on every
/// refresh.
const MAX_DIRS_VISITED: usize = 2000;
/// Working copies scanned at once. Each scan is two child processes; more in
/// parallel mostly contends for the same disk.
const PARALLEL_SCANS: usize = 4;
/// How long one search's answer is reused. The panel refreshes every couple
/// of seconds, and modules are checked out far less often than files change.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);
/// Per-module notes repeated in the set's own note before the rest are
/// counted instead.
const MAX_NOTES: usize = 3;

/// The working copies found below a directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovery {
    /// In breadth-first, then name, order.
    pub roots: Vec<(Vcs, PathBuf)>,
    /// The search stopped at a limit; there may be more.
    pub truncated: bool,
}

/// Find working-copy roots up to `max_depth` levels below `dir` (its children
/// are level 1). A root is not searched further: a module's own
/// subdirectories belong to it.
///
/// Hidden directories and the ones a snapshot never records (`node_modules`,
/// `target`, …) are skipped, and so are symlinks, which could lead out of the
/// tree or round in a circle.
pub fn discover(dir: &Path, max_depth: usize, max_roots: usize) -> Discovery {
    let mut found = Discovery::default();
    if max_depth == 0 || max_roots == 0 {
        return found;
    }
    let mut queue = VecDeque::from([(dir.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    while let Some((current, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        let mut children: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            })
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                (!name.starts_with('.') && !snapshot::SKIPPED_DIRS.contains(&name.as_str()))
                    .then(|| (name, entry.path()))
            })
            .collect();
        children.sort();
        for (_, child) in children {
            visited += 1;
            if visited > MAX_DIRS_VISITED {
                found.truncated = true;
                return found;
            }
            // Same markers as the upward search: `.git` may be a file (a
            // worktree), `.svn` must be a directory.
            let vcs = if child.join(".git").exists() {
                Some(Vcs::Git)
            } else if child.join(".svn").is_dir() {
                Some(Vcs::Svn)
            } else {
                None
            };
            match vcs {
                Some(vcs) => {
                    if found.roots.len() == max_roots {
                        found.truncated = true;
                        return found;
                    }
                    found.roots.push((vcs, child));
                }
                None if depth + 1 < max_depth => queue.push_back((child, depth + 1)),
                None => {}
            }
        }
    }
    found
}

/// Whether `dir` is a workspace of the working copies in `found`, rather than
/// a project of its own that happens to hold a checkout (a vendored clone, a
/// tool checked out under `third_party/`).
///
/// A workspace has at least two working copies, or no files of its own beside
/// the one it has. The home directory never counts: a few clones under `~`
/// would otherwise cost two client runs each on every refresh, and a
/// recursive watch of the whole home.
pub fn is_workspace(dir: &Path, found: &Discovery) -> bool {
    if found.roots.is_empty() {
        return false;
    }
    let home = dirs_next::home_dir();
    if home.as_deref() == Some(dir) {
        return false;
    }
    found.roots.len() >= 2 || !has_own_files(dir)
}

/// `dir` directly holds a regular file that is not hidden.
fn has_own_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
    })
}

type DiscoveryKey = (PathBuf, usize, usize);

static DISCOVERY_CACHE: LazyLock<Mutex<HashMap<DiscoveryKey, (Instant, Discovery)>>> =
    LazyLock::new(Default::default);

/// [`discover`], reusing an answer younger than [`DISCOVERY_TTL`].
pub fn discover_cached(dir: &Path, max_depth: usize, max_roots: usize) -> Discovery {
    let key = (dir.to_path_buf(), max_depth, max_roots);
    let now = Instant::now();
    {
        let cache = DISCOVERY_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, found)) = cache.get(&key) {
            if now.duration_since(*at) < DISCOVERY_TTL {
                return found.clone();
            }
        }
    }
    // Not locked across the walk: over a WSL share it can take a while, and
    // other panes' scans must not queue behind it.
    let found = discover(dir, max_depth, max_roots);
    let mut cache = DISCOVERY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.retain(|_, (at, _)| now.duration_since(*at) < DISCOVERY_TTL);
    cache.insert(key, (now, found.clone()));
    found
}

/// Scan every working copy in `found` and present them as one change set
/// rooted at `dir`.
///
/// Each working copy gets a runner of its own, so one module's client
/// fallback never decides another's. Blocking, like [`super::scan`].
pub fn scan_all(dir: &Path, found: &Discovery, runner: &Runner, limits: Limits) -> Scan {
    let count = found.roots.len();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Scan>>> = Mutex::new(vec![None; count]);
    // `Runner` is not `Sync`: each worker takes a template of its own.
    let templates: Vec<Runner> = (0..PARALLEL_SCANS.min(count))
        .map(|_| runner.fork())
        .collect();
    let (next, results_ref) = (&next, &results);
    std::thread::scope(|scope| {
        for template in templates {
            scope.spawn(move || loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                let Some((vcs, root)) = found.roots.get(idx) else {
                    break;
                };
                let runner = template.fork();
                let scan = match vcs {
                    Vcs::Git => git::scan(root, &runner, limits),
                    Vcs::Svn => svn::scan(root, &runner, limits),
                };
                results_ref
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())[idx] = Some(scan);
            });
        }
    });
    let results = results
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .into_iter()
        .map(|scan| scan.unwrap_or_else(|| Scan::Unavailable("not scanned".to_string())))
        .collect();
    merge(dir, &found.roots, results, found.truncated)
}

/// `root` relative to `dir`, with `/` separators: the prefix its files get.
fn relative_prefix(dir: &Path, root: &Path) -> String {
    root.strip_prefix(dir)
        .unwrap_or(root)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Combine one scan per working copy into a single change set rooted at
/// `dir`. Paths are prefixed with each working copy's place under `dir`, so
/// everything keyed by a root-relative path — recency, collapsed files, the
/// file index — works unchanged.
fn merge(dir: &Path, roots: &[(Vcs, PathBuf)], results: Vec<Scan>, truncated: bool) -> Scan {
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let mut failures = Vec::new();
    let (mut git_roots, mut svn_roots) = (0usize, 0usize);
    for ((vcs, root), scan) in roots.iter().zip(results) {
        match vcs {
            Vcs::Git => git_roots += 1,
            Vcs::Svn => svn_roots += 1,
        }
        let prefix = relative_prefix(dir, root);
        match scan {
            Scan::Changes(set) => {
                if let Some(note) = set.note {
                    notes.push(format!("{prefix}: {note}"));
                }
                for mut file in set.files {
                    file.path = format!("{prefix}/{}", file.path);
                    file.old_path = file.old_path.map(|old| format!("{prefix}/{old}"));
                    files.push(file);
                }
            }
            Scan::Unavailable(reason) => failures.push((prefix, reason)),
        }
    }
    if !roots.is_empty() && failures.len() == roots.len() {
        // Usually one cause for all of them (the client is not installed).
        let first = &failures[0].1;
        return Scan::Unavailable(if failures.iter().all(|(_, reason)| reason == first) {
            first.clone()
        } else {
            format!(
                "None of the {} working copies here could be read: {first}",
                roots.len()
            )
        });
    }
    let mut lines: Vec<String> = failures
        .into_iter()
        .map(|(prefix, reason)| format!("{prefix}: {reason}"))
        .chain(notes)
        .collect();
    let hidden = lines.len().saturating_sub(MAX_NOTES);
    lines.truncate(MAX_NOTES);
    if hidden > 0 {
        lines.push(format!("{hidden} more"));
    }
    if truncated {
        lines.insert(
            0,
            format!(
                "Showing the first {} working copies below this folder",
                roots.len()
            ),
        );
    }
    Scan::Changes(ChangeSet {
        source: ChangeSource::Multi {
            git: git_roots,
            svn: svn_roots,
        },
        root: dir.to_path_buf(),
        files,
        note: (!lines.is_empty()).then(|| lines.join("; ")),
    })
}

#[cfg(test)]
mod tests {
    use super::super::{FileChange, FileStatus};
    use super::*;

    fn tree(paths: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for path in paths {
            std::fs::create_dir_all(tmp.path().join(path)).unwrap();
        }
        tmp
    }

    #[test]
    fn modules_below_a_flat_workspace_are_found() {
        let tmp = tree(&[
            "ServiceLayerFile/.svn",
            "ServiceLayerFile/src/sub/.svn",
            "CDP4JClient/.svn",
            "group/Deep/.svn",
            "tool/.git",
            "plain/src",
            "node_modules/pkg/.git",
            ".hidden/.svn",
        ]);
        let found = discover(tmp.path(), 3, 64);
        let names: Vec<(Vcs, String)> = found
            .roots
            .iter()
            .map(|(vcs, root)| (*vcs, relative_prefix(tmp.path(), root)))
            .collect();
        // Breadth first, by name; nothing below a root; skipped and hidden
        // directories left alone.
        assert_eq!(
            names,
            vec![
                (Vcs::Svn, "CDP4JClient".to_string()),
                (Vcs::Svn, "ServiceLayerFile".to_string()),
                (Vcs::Git, "tool".to_string()),
                (Vcs::Svn, "group/Deep".to_string()),
            ]
        );
        assert!(!found.truncated);
    }

    #[test]
    fn the_search_stops_at_its_limits() {
        let tmp = tree(&[
            "a/.svn",
            "b/.svn",
            "c/.svn",
            "x/y/z/w/v/.svn",
            "x/y/z/u/.svn",
        ]);
        let found = discover(tmp.path(), 3, 2);
        assert_eq!(found.roots.len(), 2);
        assert!(found.truncated);
        // From `x`, `u` is three levels down and `v` four: past the depth.
        let found = discover(&tmp.path().join("x"), 3, 64);
        assert_eq!(found.roots, vec![(Vcs::Svn, tmp.path().join("x/y/z/u"))]);
        assert!(discover(tmp.path(), 0, 64).roots.is_empty());
    }

    #[test]
    fn merged_paths_carry_their_working_copy_prefix() {
        let dir = Path::new("/ws");
        let roots = vec![
            (Vcs::Svn, PathBuf::from("/ws/ServiceLayerFile")),
            (Vcs::Svn, PathBuf::from("/ws/group/Deep")),
            (Vcs::Git, PathBuf::from("/ws/tool")),
        ];
        let mut renamed = FileChange::new("New.java", FileStatus::Renamed);
        renamed.old_path = Some("Old.java".to_string());
        let set = |files: Vec<FileChange>, note: Option<&str>| {
            Scan::Changes(ChangeSet {
                source: ChangeSource::Svn,
                root: PathBuf::new(),
                files,
                note: note.map(str::to_string),
            })
        };
        let results = vec![
            set(
                vec![FileChange::new("src/A.java", FileStatus::Modified)],
                None,
            ),
            set(vec![renamed], Some("diff too large")),
            Scan::Unavailable("git is not installed".to_string()),
        ];
        let Scan::Changes(merged) = merge(dir, &roots, results, false) else {
            panic!("expected changes");
        };
        assert_eq!(merged.source, ChangeSource::Multi { git: 1, svn: 2 });
        assert_eq!(merged.root, PathBuf::from("/ws"));
        let paths: Vec<&str> = merged.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["ServiceLayerFile/src/A.java", "group/Deep/New.java"]
        );
        assert_eq!(
            merged.files[1].old_path.as_deref(),
            Some("group/Deep/Old.java")
        );
        assert_eq!(
            merged.note.as_deref(),
            Some("tool: git is not installed; group/Deep: diff too large")
        );
    }

    #[test]
    fn a_folder_of_clones_shows_every_modules_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let git = |dir: &Path, args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .is_ok_and(|out| out.status.success())
        };
        for module in ["modA", "modB"] {
            let dir = ws.join(module);
            std::fs::create_dir_all(&dir).unwrap();
            if !git(&dir, &["init", "-q"]) {
                eprintln!("git not installed; skipping");
                return;
            }
            std::fs::write(dir.join("f.txt"), "one\n").unwrap();
            assert!(git(&dir, &["add", "f.txt"]));
            assert!(git(&dir, &["commit", "-q", "-m", "init"]));
        }
        std::fs::write(ws.join("modB/f.txt"), "two\n").unwrap();

        let limits = Limits {
            max_file_bytes: 1024 * 1024,
            snapshot_max_files: 1000,
            nested_max_depth: 3,
            nested_max_roots: 64,
        };
        let store = tmp.path().join("store");
        let Scan::Changes(set) = super::super::scan(&ws, &Runner::host(), &store, limits) else {
            panic!("scan unavailable");
        };
        assert_eq!(set.source, ChangeSource::Multi { git: 2, svn: 0 });
        assert_eq!(set.source.label(), "Git \u{00d7}2");
        let paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["modB/f.txt"]);
        // Recency found the file through the prefixed path.
        assert!(set.files[0].modified.is_some());
    }

    #[test]
    fn a_project_holding_one_checkout_is_not_a_workspace() {
        let tmp = tree(&["third_party/tool/.git", "src"]);
        std::fs::write(tmp.path().join("Cargo.toml"), "x").unwrap();
        let found = discover(tmp.path(), 3, 64);
        assert_eq!(found.roots.len(), 1);
        // The project's own files are what the snapshot is for.
        assert!(!is_workspace(tmp.path(), &found));
        // Without files of its own it is a workspace of that one checkout.
        std::fs::remove_file(tmp.path().join("Cargo.toml")).unwrap();
        assert!(is_workspace(tmp.path(), &found));
        // Two checkouts always make one.
        std::fs::create_dir_all(tmp.path().join("other/.svn")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "x").unwrap();
        assert!(is_workspace(tmp.path(), &discover(tmp.path(), 3, 64)));
        assert!(!is_workspace(tmp.path(), &Discovery::default()));
    }

    #[test]
    fn one_shared_failure_is_reported_once() {
        let roots = vec![
            (Vcs::Svn, PathBuf::from("/ws/a")),
            (Vcs::Svn, PathBuf::from("/ws/b")),
        ];
        let reason = "This is a Subversion working copy, but svn is not installed";
        let results = vec![
            Scan::Unavailable(reason.to_string()),
            Scan::Unavailable(reason.to_string()),
        ];
        assert_eq!(
            merge(Path::new("/ws"), &roots, results, false),
            Scan::Unavailable(reason.to_string())
        );
    }
}
