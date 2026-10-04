//! Snapshot provider: for a directory under neither Git nor Subversion.
//!
//! With no version control there is no "before" to compare with, so the first
//! scan records one: a manifest of every file plus a copy of each text file
//! small enough to diff. Later scans compare the directory with that
//! baseline. It can only show changes made after the baseline was taken.
//!
//! The copies are of the user's own files and stay in the user's own data
//! directory, readable by the user alone.

use super::{
    looks_binary, ChangeSet, ChangeSource, DiffLine, FileChange, FileStatus, Hunk, Limits,
    LineKind, Scan,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST: &str = "manifest.json";
const BLOBS: &str = "blobs";
/// Unchanged lines shown either side of a change, as `diff -u` does.
const CONTEXT_LINES: usize = 3;
/// Baselines not touched for this long are removed by [`prune`].
pub const BASELINE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Directories never worth recording, even when no ignore file says so.
const SKIPPED_DIRS: [&str; 8] = [
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    ".DS_Store",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    size: u64,
    /// Modification time in nanoseconds since the epoch; with `size`, the
    /// cheap test for "certainly unchanged".
    mtime: u128,
    hash: String,
    /// Whether a copy of the content is kept under `blobs/<hash>`.
    stored: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    root: PathBuf,
    files: BTreeMap<String, Entry>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Where the baseline for `root` lives under `base`.
pub fn store_for(base: &Path, root: &Path) -> PathBuf {
    let key = hash(root.to_string_lossy().as_bytes());
    base.join(&key[..24])
}

fn mtime(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

/// Files under `root`, as `/`-separated relative paths, honouring ignore
/// files. `None` when there are more than `max_files`.
fn walk(root: &Path, max_files: usize) -> Option<Vec<(String, PathBuf)>> {
    let walker = ignore::WalkBuilder::new(root)
        // Dotfiles are exactly what an agent edits (.env, .eslintrc).
        .hidden(false)
        // Honour .gitignore even though this is, by definition, not a repo.
        .require_git(false)
        .git_global(false)
        .parents(false)
        .follow_links(false)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .map_or(true, |name| !SKIPPED_DIRS.contains(&name))
        })
        .build();
    let mut files = Vec::new();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        if files.len() >= max_files {
            return None;
        }
        let relative = relative.to_string_lossy().replace('\\', "/");
        files.push((relative, entry.into_path()));
    }
    files.sort();
    Some(files)
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn load_manifest(store: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(store.join(MANIFEST)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Record `root` as it is now. `Err` carries a reason fit to show the user.
fn take_baseline(root: &Path, store: &Path, limits: Limits) -> Result<Manifest, String> {
    let files = walk(root, limits.snapshot_max_files).ok_or_else(|| {
        format!(
            "More than {} files here; open the panel in a project folder",
            limits.snapshot_max_files
        )
    })?;
    // Start clean: blobs from an older baseline would otherwise pile up.
    let _ = std::fs::remove_dir_all(store);
    let blobs = store.join(BLOBS);
    private_dir(store).map_err(|err| err.to_string())?;
    private_dir(&blobs).map_err(|err| err.to_string())?;

    let mut manifest = Manifest {
        root: root.to_path_buf(),
        files: BTreeMap::new(),
    };
    for (relative, path) in files {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let small = meta.len() as usize <= limits.max_file_bytes;
        // An oversized file is recorded by size and time alone: enough to
        // notice a change, without reading gigabytes.
        let (hash, stored) = if small {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let hash = hash(&bytes);
            let stored = !looks_binary(&bytes) && std::fs::write(blobs.join(&hash), &bytes).is_ok();
            (hash, stored)
        } else {
            (String::new(), false)
        };
        manifest.files.insert(
            relative,
            Entry {
                size: meta.len(),
                mtime: mtime(&meta),
                hash,
                stored,
            },
        );
    }
    let json = serde_json::to_string(&manifest).map_err(|err| err.to_string())?;
    std::fs::write(store.join(MANIFEST), json).map_err(|err| err.to_string())?;
    Ok(manifest)
}

/// Forget the baseline for `root`; the next scan records a fresh one.
pub fn reset(base: &Path, root: &Path) {
    let _ = std::fs::remove_dir_all(store_for(base, root));
}

/// Remove baselines nobody has scanned against for [`BASELINE_MAX_AGE`].
pub fn prune(base: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let manifest = entry.path().join(MANIFEST);
        let stale = std::fs::metadata(&manifest)
            .and_then(|meta| meta.accessed().or_else(|_| meta.modified()))
            .ok()
            .and_then(|time| now.duration_since(time).ok())
            .is_some_and(|age| age > BASELINE_MAX_AGE);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Line diff of two texts as hunks with [`CONTEXT_LINES`] of context.
pub fn diff_text(old: &str, new: &str) -> (Vec<Hunk>, usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let (mut added, mut removed) = (0, 0);
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(CONTEXT_LINES) {
        let Some(first) = group.first() else {
            continue;
        };
        let mut hunk = Hunk {
            old_start: first.old_range().start as u32 + 1,
            new_start: first.new_range().start as u32 + 1,
            lines: Vec::new(),
        };
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Insert => {
                        added += 1;
                        LineKind::Added
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        LineKind::Removed
                    }
                };
                hunk.lines.push(DiffLine {
                    kind,
                    old_no: change.old_index().map(|idx| idx as u32 + 1),
                    new_no: change.new_index().map(|idx| idx as u32 + 1),
                    text: change.value().trim_end_matches(['\n', '\r']).to_string(),
                });
            }
        }
        hunks.push(hunk);
    }
    (hunks, added, removed)
}

fn stored_text(store: &Path, entry: &Entry) -> Option<String> {
    if !entry.stored {
        return None;
    }
    let bytes = std::fs::read(store.join(BLOBS).join(&entry.hash)).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// One file present both then and now. `None` when it has not changed.
fn compare(
    path: &str,
    full: &Path,
    entry: &Entry,
    store: &Path,
    limits: Limits,
) -> Option<FileChange> {
    let meta = std::fs::metadata(full).ok()?;
    if meta.len() == entry.size && mtime(&meta) == entry.mtime {
        return None;
    }
    let mut file = FileChange::new(path, FileStatus::Modified);
    if meta.len() as usize > limits.max_file_bytes || entry.hash.is_empty() {
        file.note = Some("too large to show".to_string());
        return Some(file);
    }
    let bytes = std::fs::read(full).ok()?;
    if hash(&bytes) == entry.hash {
        // Touched, or rewritten with the same content.
        return None;
    }
    let old = stored_text(store, entry);
    match old {
        Some(old) if !looks_binary(&bytes) => {
            let new = String::from_utf8_lossy(&bytes);
            let (hunks, added, removed) = diff_text(&old, &new);
            file.hunks = hunks;
            file.added = added;
            file.removed = removed;
            file.cap_lines();
        }
        _ => file.note = Some("binary".to_string()),
    }
    Some(file)
}

pub fn scan(root: &Path, base: &Path, limits: Limits) -> Scan {
    let store = store_for(base, root);
    let source = ChangeSource::Snapshot;
    let Some(manifest) = load_manifest(&store).filter(|manifest| manifest.root == root) else {
        return match take_baseline(root, &store, limits) {
            Ok(_) => Scan::Changes(ChangeSet {
                source,
                root: root.to_path_buf(),
                files: Vec::new(),
                note: Some("No Git or SVN here: tracking changes from now on".to_string()),
            }),
            Err(reason) => Scan::Unavailable(reason),
        };
    };

    // Twice the baseline's ceiling: a build that floods the directory should
    // not be read file by file.
    let Some(current) = walk(root, limits.snapshot_max_files.saturating_mul(2)) else {
        return Scan::Unavailable("Too many new files here to compare".to_string());
    };

    let mut files = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (relative, full) in &current {
        seen.insert(relative.as_str());
        match manifest.files.get(relative) {
            Some(entry) => files.extend(compare(relative, full, entry, &store, limits)),
            None => files.push(match super::read_text_file(full, limits.max_file_bytes) {
                Ok(text) => FileChange::all_added(relative.clone(), &text),
                Err(reason) => {
                    let mut file = FileChange::new(relative.clone(), FileStatus::Added);
                    file.note = Some(reason);
                    file
                }
            }),
        }
    }
    for (relative, entry) in &manifest.files {
        if seen.contains(relative.as_str()) {
            continue;
        }
        let mut file = FileChange::new(relative.clone(), FileStatus::Deleted);
        match stored_text(&store, entry) {
            Some(old) => {
                let (hunks, added, removed) = diff_text(&old, "");
                file.hunks = hunks;
                file.added = added;
                file.removed = removed;
                file.cap_lines();
            }
            None => file.note = Some("binary or too large".to_string()),
        }
        files.push(file);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Scan::Changes(ChangeSet {
        source,
        root: root.to_path_buf(),
        files,
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            max_file_bytes: 1024,
            snapshot_max_files: 100,
        }
    }

    fn changes(root: &Path, base: &Path) -> ChangeSet {
        match scan(root, base, limits()) {
            Scan::Changes(set) => set,
            Scan::Unavailable(reason) => panic!("unavailable: {}", reason),
        }
    }

    /// Rewrite a file so that its size or mtime is certain to differ: some
    /// filesystems only keep whole seconds.
    fn rewrite(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        let later = SystemTime::now() + Duration::from_secs(5);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(later).unwrap();
    }

    #[test]
    fn the_first_scan_records_a_baseline_and_reports_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();

        let set = changes(&root, &base);
        assert_eq!(set.source, ChangeSource::Snapshot);
        assert!(set.files.is_empty());
        assert!(set.note.unwrap().contains("from now on"));
        assert!(changes(&root, &base).files.is_empty());
    }

    #[test]
    fn later_scans_show_what_changed_since() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/kept.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(root.join("same.txt"), "still\n").unwrap();
        changes(&root, &base);

        rewrite(&root.join("src/kept.txt"), "one\n2\nthree\n");
        rewrite(&root.join("same.txt"), "still\n");
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();

        let set = changes(&root, &base);
        let summary: Vec<(&str, FileStatus, usize, usize)> = set
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("gone.txt", FileStatus::Deleted, 0, 1),
                ("new.txt", FileStatus::Added, 1, 0),
                ("src/kept.txt", FileStatus::Modified, 1, 1),
            ]
        );
        let kept = &set.files[2].hunks[0];
        assert_eq!(kept.lines[1].kind, LineKind::Removed);
        assert_eq!(kept.lines[1].old_no, Some(2));
        assert_eq!(kept.lines[2].text, "2");
    }

    #[test]
    fn resetting_makes_the_present_the_new_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        changes(&root, &base);
        rewrite(&root.join("a.txt"), "two\n");
        assert_eq!(changes(&root, &base).files.len(), 1);

        reset(&base, &root);
        assert!(changes(&root, &base).files.is_empty());
        assert!(changes(&root, &base).files.is_empty());
    }

    #[test]
    fn ignored_and_heavy_directories_are_not_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
        changes(&root, &base);

        std::fs::write(root.join("node_modules/x/i.js"), "x").unwrap();
        std::fs::write(root.join("out/build.log"), "x").unwrap();
        std::fs::write(root.join(".env"), "KEY=1\n").unwrap();

        let set = changes(&root, &base);
        let paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, [".env"]);
    }

    #[test]
    fn a_directory_with_too_many_files_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(&root).unwrap();
        for idx in 0..5 {
            std::fs::write(root.join(format!("{idx}.txt")), "x").unwrap();
        }
        let tight = Limits {
            max_file_bytes: 1024,
            snapshot_max_files: 3,
        };
        match scan(&root, &base, tight) {
            Scan::Unavailable(reason) => assert!(reason.contains("More than 3 files")),
            Scan::Changes(_) => panic!("expected a refusal"),
        }
    }

    #[test]
    fn binary_and_oversized_files_change_without_a_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("pic.bin"), [0u8, 1, 2]).unwrap();
        std::fs::write(root.join("big.txt"), "x".repeat(4000)).unwrap();
        changes(&root, &base);

        rewrite(&root.join("pic.bin"), "\0changed");
        rewrite(&root.join("big.txt"), &"y".repeat(4001));

        let set = changes(&root, &base);
        let notes: Vec<(&str, Option<&str>)> = set
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.note.as_deref()))
            .collect();
        assert_eq!(
            notes,
            vec![
                ("big.txt", Some("too large to show")),
                ("pic.bin", Some("binary")),
            ]
        );
    }

    #[test]
    fn hunks_carry_context_and_both_line_numbers() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
        let (hunks, added, removed) = diff_text(old, new);
        assert_eq!((added, removed), (1, 1));
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (2, 2));
        // Three lines of context either side of the changed pair.
        assert_eq!(hunks[0].lines.len(), 8);
        assert_eq!(hunks[0].lines[3].kind, LineKind::Removed);
        assert_eq!(hunks[0].lines[4].kind, LineKind::Added);
        assert_eq!(hunks[0].lines[4].new_no, Some(5));
    }

    #[test]
    fn stale_baselines_are_pruned_and_fresh_ones_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, base) = (tmp.path().join("proj"), tmp.path().join("store"));
        std::fs::create_dir_all(&root).unwrap();
        changes(&root, &base);
        let store = store_for(&base, &root);

        prune(&base, SystemTime::now());
        assert!(store.exists());
        prune(
            &base,
            SystemTime::now() + BASELINE_MAX_AGE + Duration::from_secs(60),
        );
        assert!(!store.exists());
    }
}
