//! Watching a working copy, so the panel re-reads it when a file is written
//! rather than at its next poll.
//!
//! The watch only says "something changed"; what changed is still the
//! provider's question to answer. Where watching is not possible (a network
//! share, a WSL distro's filesystem seen from Windows, a tree too large for
//! the platform's limits) the poll simply remains the only trigger.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct Watch {
    root: PathBuf,
    dirty: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
}

impl Watch {
    /// Watch everything under `root`. `on_change` runs on the watcher's
    /// thread, once per run of changes: again only after [`Self::clear`].
    ///
    /// Blocking on platforms that register every directory; never call it on
    /// the GUI thread.
    pub fn start(root: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let dirty = Arc::new(AtomicBool::new(false));
        let mut watcher = notify::recommended_watcher({
            let dirty = Arc::clone(&dirty);
            let root = root.to_path_buf();
            move |event: notify::Result<notify::Event>| {
                let relevant = match &event {
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
                    Ok(event) => {
                        event.paths.is_empty()
                            || event.paths.iter().any(|path| is_relevant(&root, path))
                    }
                    // Typically "events were dropped": something changed.
                    Err(_) => true,
                };
                if relevant && !dirty.swap(true, Ordering::Relaxed) {
                    on_change();
                }
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            root: root.to_path_buf(),
            dirty,
            _watcher: watcher,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Something under the root changed since the last [`Self::clear`].
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Call when a scan starts: a change during the scan counts for the next.
    pub fn clear(&self) {
        self.dirty.store(false, Ordering::Relaxed);
    }
}

/// Whether a write to `path` can change what the panel shows.
///
/// A version-control client rewrites its own bookkeeping constantly, and
/// reading the working copy must not look like a change to it. Inside `.git`
/// only what moves `HEAD` or the index counts; inside `.svn` only the
/// working-copy database.
pub fn is_relevant(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let names: Vec<&str> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        return true;
    }
    let leaf = relative.file_name().and_then(|name| name.to_str());
    // At any depth: a panel over a folder of clones watches every module's
    // `.git`, and each `git status` it runs rewrites that module's objects.
    if let Some(at) = names.iter().position(|name| *name == ".git") {
        return !leaf.is_some_and(|leaf| leaf.ends_with(".lock"))
            && matches!(
                names.get(at + 1).copied(),
                Some("index" | "HEAD" | "refs" | "packed-refs")
            );
    }
    if names.contains(&".svn") {
        return leaf == Some("wc.db");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clients_own_bookkeeping_is_not_a_change() {
        let root = Path::new("/work/proj");
        let relevant = |path: &str| is_relevant(root, &root.join(path));
        assert!(relevant("src/main.rs"));
        assert!(relevant("new-dir"));
        // A commit, a checkout, a stage.
        assert!(relevant(".git/HEAD"));
        assert!(relevant(".git/index"));
        assert!(relevant(".git/refs/heads/main"));
        // Churn that leaves the diff as it was.
        assert!(!relevant(".git/index.lock"));
        assert!(!relevant(".git/objects/ab/cdef"));
        assert!(!relevant(".git/logs/HEAD"));
        assert!(!relevant(".git/FETCH_HEAD"));
        assert!(relevant(".svn/wc.db"));
        assert!(!relevant(".svn/wc.db-journal"));
        assert!(!relevant("vendor/lib/.svn/tmp/x"));
        // A file merely named like the directory is an ordinary file.
        assert!(relevant("docs/.gitignore"));
        // A module's own repository, below the watched folder.
        assert!(relevant("tool/.git/index"));
        assert!(!relevant("tool/.git/objects/ab/cdef"));
        assert!(!relevant("tool/.git/index.lock"));
        assert!(relevant("mod/.svn/wc.db"));
    }

    #[test]
    fn a_write_marks_the_watch_dirty_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(watch) = Watch::start(tmp.path(), move || {
            let _ = tx.send(());
        }) else {
            eprintln!("cannot watch here; skipping");
            return;
        };
        assert!(!watch.is_dirty());
        std::fs::write(tmp.path().join("a.txt"), "one").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the write was noticed");
        assert!(watch.is_dirty());
        watch.clear();
        assert!(!watch.is_dirty());
    }
}
