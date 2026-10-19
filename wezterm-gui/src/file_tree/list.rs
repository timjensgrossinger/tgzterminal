//! Reading one folder of the tree. Runs on a worker thread: a folder can sit
//! on a network share or behind the WSL filesystem bridge.

use super::{parent_rel, sort_entries, Entry, EntryKind, Listing, Tree};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Version-control metadata: never something to browse.
const VCS_DIRS: [&str; 3] = [".git", ".svn", ".hg"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOptions {
    /// Show names starting with a dot.
    pub show_hidden: bool,
    /// Leave out what `.gitignore`, `.ignore` and git's exclude files leave
    /// out.
    pub respect_gitignore: bool,
}

/// The entries directly inside `dir`, sorted for display.
pub fn list_dir(dir: &Path, options: ListOptions) -> Result<Vec<Entry>, String> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err("Not a folder".to_string()),
        Err(err) => return Err(err.to_string()),
    }
    let ignore = options.respect_gitignore;
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(!options.show_hidden)
        .ignore(ignore)
        .git_ignore(ignore)
        .git_exclude(ignore)
        .git_global(ignore)
        // Ignore files above `dir` still apply: a folder opened deep in a
        // repository is filtered as git would filter it.
        .parents(ignore)
        .follow_links(false)
        .build();
    let mut entries = Vec::new();
    for item in walker {
        let Ok(item) = item else {
            continue;
        };
        if item.depth() == 0 {
            continue;
        }
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            // A name that is not UTF-8 cannot be shown or passed on faithfully.
            continue;
        };
        if VCS_DIRS.contains(&name.as_str()) {
            continue;
        }
        let symlink = item.path_is_symlink();
        let is_dir = if symlink {
            // What the link points at; a dangling link is shown as a file.
            std::fs::metadata(item.path()).is_ok_and(|meta| meta.is_dir())
        } else {
            item.file_type().is_some_and(|kind| kind.is_dir())
        };
        entries.push(Entry {
            name,
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            symlink,
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// Most files the filter searches; a tree larger than this is cut off and
/// says so.
pub const MAX_INDEX: usize = 200_000;

/// Every file under `root` as a relative path, for the filter: from
/// `git ls-files` when `git` can answer for the root, else from a walk.
/// The flag says the list was cut off at [`MAX_INDEX`].
pub fn path_index(
    root: &Path,
    options: ListOptions,
    git: Option<&crate::diff_panel::exec::Runner>,
) -> (Vec<String>, bool) {
    let from_git = git
        .filter(|_| options.respect_gitignore && root.join(".git").exists())
        .and_then(|runner| {
            runner
                .run(
                    "git",
                    &[
                        "ls-files",
                        "-z",
                        "--cached",
                        "--others",
                        "--exclude-standard",
                    ],
                    root,
                )
                .ok()
        })
        .filter(|out| out.success && !out.truncated);
    if let Some(out) = from_git {
        let mut files: Vec<String> = out
            .stdout
            .split('\0')
            .filter(|rel| !rel.is_empty())
            .filter(|rel| options.show_hidden || !rel.split('/').any(|part| part.starts_with('.')))
            .map(str::to_string)
            .collect();
        files.sort();
        files.dedup();
        let cut = files.len() > MAX_INDEX;
        files.truncate(MAX_INDEX);
        return (files, cut);
    }
    let ignore = options.respect_gitignore;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(!options.show_hidden)
        .ignore(ignore)
        .git_ignore(ignore)
        .git_exclude(ignore)
        .git_global(ignore)
        .parents(ignore)
        .follow_links(false)
        .filter_entry(|entry| {
            entry
                .file_name()
                .to_str()
                .map_or(true, |name| !VCS_DIRS.contains(&name))
        })
        .build();
    let mut files = Vec::new();
    for item in walker.flatten() {
        if !item.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(rel) = item
            .path()
            .strip_prefix(root)
            .ok()
            .and_then(|rel| rel.to_str())
        else {
            continue;
        };
        if files.len() == MAX_INDEX {
            return (files, true);
        }
        files.push(rel.replace('\\', "/"));
    }
    files.sort();
    (files, false)
}

/// Read the root and every folder in `expanded` that is on show: one whose
/// parent was read and holds it. Parents are read before their children.
pub fn list_open(
    root: PathBuf,
    expanded: &HashSet<String>,
    options: ListOptions,
    mut list: impl FnMut(&Path, ListOptions) -> Result<Vec<Entry>, String>,
) -> Tree {
    let mut tree = Tree::new(root);
    let mut open: Vec<&String> = expanded.iter().filter(|rel| !rel.is_empty()).collect();
    open.sort_by_key(|rel| (rel.matches('/').count(), rel.as_str()));
    for rel in std::iter::once("").chain(open.into_iter().map(String::as_str)) {
        if let Some(parent) = parent_rel(rel) {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            let shown = matches!(
                tree.listing(parent),
                Some(Listing::Loaded(entries))
                    if entries.iter().any(|e| e.name == name && e.kind == EntryKind::Dir)
            );
            if !shown {
                continue;
            }
        }
        let listing = match list(&tree.abs_path(rel), options) {
            Ok(entries) => Listing::Loaded(entries),
            Err(reason) => Listing::Failed(reason),
        };
        tree.set_listing(rel.to_string(), listing);
    }
    tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join(".editorconfig"), "").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();
        fs::write(root.join("debug.log"), "").unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("src/trace.log"), "").unwrap();
        dir
    }

    const ALL: ListOptions = ListOptions {
        show_hidden: true,
        respect_gitignore: true,
    };

    #[test]
    fn lists_one_level_folders_first_without_vcs_or_ignored() {
        let dir = fixture();
        let entries = list_dir(dir.path(), ALL).unwrap();
        assert_eq!(
            names(&entries),
            ["src", ".editorconfig", ".gitignore", "Cargo.toml"]
        );
        assert_eq!(entries[0].kind, EntryKind::Dir);
    }

    #[test]
    fn ignore_files_of_parent_folders_apply() {
        let dir = fixture();
        let entries = list_dir(&dir.path().join("src"), ALL).unwrap();
        assert_eq!(names(&entries), ["nested", "main.rs"]);
    }

    #[test]
    fn hidden_and_ignored_can_be_shown_or_hidden() {
        let dir = fixture();
        let entries = list_dir(
            dir.path(),
            ListOptions {
                show_hidden: false,
                respect_gitignore: false,
            },
        )
        .unwrap();
        assert_eq!(
            names(&entries),
            ["src", "target", "Cargo.toml", "debug.log"]
        );
    }

    #[test]
    fn a_missing_folder_is_an_error() {
        let dir = fixture();
        assert!(list_dir(&dir.path().join("gone"), ALL).is_err());
        assert!(list_dir(&dir.path().join("Cargo.toml"), ALL).is_err());
    }

    #[test]
    fn list_open_reads_only_folders_on_show() {
        let dir = fixture();
        let expanded: HashSet<String> = ["src", "src/nested", "target", "gone/deeper"]
            .iter()
            .map(|rel| rel.to_string())
            .collect();
        let mut asked = Vec::new();
        let tree = list_open(dir.path().to_path_buf(), &expanded, ALL, |path, options| {
            asked.push(path.strip_prefix(dir.path()).unwrap().to_path_buf());
            list_dir(path, options)
        });
        // `target` is ignored, so not on show; `gone` does not exist.
        assert_eq!(
            asked,
            [
                PathBuf::new(),
                PathBuf::from("src"),
                PathBuf::from("src").join("nested")
            ]
        );
        assert!(tree.is_listed("src/nested"));
        assert!(!tree.is_listed("target"));
    }

    #[test]
    fn the_index_walks_without_git() {
        let dir = fixture();
        let (files, cut) = path_index(dir.path(), ALL, None);
        assert!(!cut);
        assert_eq!(
            files,
            [".editorconfig", ".gitignore", "Cargo.toml", "src/main.rs"]
        );
        let (files, _) = path_index(
            dir.path(),
            ListOptions {
                show_hidden: false,
                respect_gitignore: true,
            },
            None,
        );
        assert_eq!(files, ["Cargo.toml", "src/main.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_folder_is_a_folder() {
        let dir = fixture();
        std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("link")).unwrap();
        let entries = list_dir(dir.path(), ALL).unwrap();
        let link = entries.iter().find(|entry| entry.name == "link").unwrap();
        assert_eq!(link.kind, EntryKind::Dir);
        assert!(link.symlink);
    }
}
