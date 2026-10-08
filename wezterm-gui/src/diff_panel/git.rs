//! Git provider: the working tree against `HEAD`, plus untracked files.
//!
//! Uses the command-line client rather than libgit2 so the user's own
//! configuration (ignore rules, attributes, filters) decides what a change is.

use super::exec::{run_chunked, CommandOutput, Runner};
use super::{
    read_text_file, unified, ChangeSet, ChangeSource, FileChange, FileStatus, Limits, Scan,
    MAX_UNTRACKED_FILES,
};
use std::path::Path;

/// The tree every repository has, with nothing in it: what the working tree
/// is compared against before the first commit exists.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn git(runner: &Runner, root: &Path, args: &[&str]) -> std::io::Result<CommandOutput> {
    let mut full = vec![
        "--no-optional-locks",
        // Paths as they are, not octal-escaped.
        "-c",
        "core.quotepath=off",
    ];
    full.extend_from_slice(args);
    runner.run("git", &full, root)
}

/// `paths` as literal pathspecs: `[`, `*` and `?` in a file name are not globs.
fn literal_pathspecs(paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .map(|path| format!(":(literal){path}"))
        .collect()
}

fn diff_against(
    runner: &Runner,
    root: &Path,
    base: &str,
    pathspecs: &[String],
) -> std::io::Result<CommandOutput> {
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        // The parser strips these; a user's `diff.noprefix` must not
        // take them away.
        "--src-prefix=a/",
        "--dst-prefix=b/",
        base,
        "--",
    ];
    args.extend(pathspecs.iter().map(String::as_str));
    git(runner, root, &args)
}

/// NUL-separated paths, as `ls-files -z` prints them.
fn parse_path_list(output: &str) -> Vec<&str> {
    output.split('\0').filter(|path| !path.is_empty()).collect()
}

fn untracked_file(root: &Path, path: &str, limits: Limits, read_content: bool) -> FileChange {
    if !read_content {
        return FileChange::new(path, FileStatus::Added);
    }
    match read_text_file(&root.join(path), limits.max_file_bytes) {
        Ok(text) => FileChange::all_added(path, &text),
        Err(reason) => {
            let mut file = FileChange::new(path, FileStatus::Added);
            file.note = Some(reason);
            file
        }
    }
}

pub fn scan(root: &Path, runner: &Runner, limits: Limits) -> Scan {
    scan_paths(root, runner, &[], limits)
}

/// [`scan`] narrowed to `paths` (relative to `root`, `/`-separated); every
/// change when `paths` is empty.
pub fn scan_paths(root: &Path, runner: &Runner, paths: &[&str], limits: Limits) -> Scan {
    let branch = match git(runner, root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        Ok(out) if out.success => Some(out.stdout.trim().to_string()).filter(|b| !b.is_empty()),
        // Detached HEAD: a repository, just not on a branch.
        Ok(_) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Scan::Unavailable("This is a Git repository, but git is not installed".into());
        }
        Err(err) => return Scan::Unavailable(format!("Could not run git: {err}")),
    };

    let mut diff = match run_chunked(&literal_pathspecs(paths), |specs| {
        diff_against(runner, root, "HEAD", specs)
    }) {
        Ok(out) => out,
        Err(err) => return Scan::Unavailable(format!("Could not run git: {err}")),
    };
    if !diff.success && !diff.truncated {
        // No commit yet, so no HEAD to compare with.
        match run_chunked(&literal_pathspecs(paths), |specs| {
            diff_against(runner, root, EMPTY_TREE, specs)
        }) {
            Ok(out) if out.success || out.truncated => diff = out,
            _ => {
                let reason = diff.stderr.lines().next().unwrap_or("git diff failed");
                return Scan::Unavailable(reason.trim().to_string());
            }
        }
    }

    let mut files = unified::parse(&diff.stdout);
    if files.is_empty() {
        if let Some(reason) = diff.timeout_reason("git") {
            return Scan::Unavailable(reason);
        }
    }
    let mut note = diff.timeout_reason("git").or_else(|| {
        diff.truncated
            .then(|| "The diff is too large to show in full".to_string())
    });

    let untracked = run_chunked(&literal_pathspecs(paths), |specs| {
        let mut args = vec!["ls-files", "--others", "--exclude-standard", "-z", "--"];
        args.extend(specs.iter().map(String::as_str));
        git(runner, root, &args)
    });
    if let Ok(out) = untracked {
        if out.success {
            let untracked = parse_path_list(&out.stdout);
            for (idx, path) in untracked.iter().enumerate() {
                files.push(untracked_file(
                    root,
                    path,
                    limits,
                    idx < MAX_UNTRACKED_FILES,
                ));
            }
            if untracked.len() > MAX_UNTRACKED_FILES && note.is_none() {
                note = Some(format!(
                    "{} untracked files; contents shown for the first {MAX_UNTRACKED_FILES}",
                    untracked.len()
                ));
            }
        }
    }

    Scan::Changes(ChangeSet {
        source: ChangeSource::Git { branch },
        root: root.to_path_buf(),
        files,
        note: note.or_else(|| runner.fallback_note("git")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_panel::LineKind;

    fn limits() -> Limits {
        Limits {
            max_file_bytes: 1024 * 1024,
            snapshot_max_files: 1000,
            nested_max_depth: 3,
            nested_max_roots: 64,
        }
    }

    /// Run git in a scratch repository, isolated from the developer's own
    /// configuration. `None` when git is not installed.
    fn sh(root: &Path, args: &[&str]) -> Option<()> {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        assert!(status.success(), "git {:?} failed", args);
        Some(())
    }

    fn changes(root: &Path) -> ChangeSet {
        match scan(root, &Runner::host(), limits()) {
            Scan::Changes(set) => set,
            Scan::Unavailable(reason) => panic!("scan unavailable: {}", reason),
        }
    }

    #[test]
    fn a_scan_narrowed_to_paths_reads_only_those() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        if sh(root, &["init", "-q", "-b", "main"]).is_none() {
            eprintln!("git not installed; skipping");
            return;
        }
        for name in ["a.txt", "b.txt", "[x].txt"] {
            std::fs::write(root.join(name), "one\n").unwrap();
        }
        sh(root, &["add", "."]).unwrap();
        sh(root, &["commit", "-q", "-m", "first"]).unwrap();
        for name in ["a.txt", "b.txt", "[x].txt", "x.txt"] {
            std::fs::write(root.join(name), "two\n").unwrap();
        }
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(root.join("other.txt"), "fresh\n").unwrap();

        // `[x].txt` is a name, not a glob that would match `x.txt`.
        let paths = ["a.txt", "[x].txt", "new.txt", "missing.txt"];
        let set = match scan_paths(root, &Runner::host(), &paths, limits()) {
            Scan::Changes(set) => set,
            Scan::Unavailable(reason) => panic!("scan unavailable: {}", reason),
        };
        let mut shown: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        shown.sort();
        assert_eq!(shown, vec!["[x].txt", "a.txt", "new.txt"]);
    }

    #[test]
    fn path_lists_split_on_nul() {
        assert_eq!(
            parse_path_list("a.txt\0dir/b c.txt\0"),
            ["a.txt", "dir/b c.txt"]
        );
        assert!(parse_path_list("").is_empty());
    }

    #[test]
    fn a_working_tree_is_compared_with_head() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        if sh(root, &["init", "-q", "-b", "main"]).is_none() {
            eprintln!("git not installed; skipping");
            return;
        }
        std::fs::write(root.join("kept.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("gone.txt"), "bye\n").unwrap();
        sh(root, &["add", "."]).unwrap();
        sh(root, &["commit", "-q", "-m", "first"]).unwrap();

        std::fs::write(root.join("kept.txt"), "one\n2\n").unwrap();
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new file.txt"), "fresh\n").unwrap();

        let set = changes(root);
        assert_eq!(
            set.source,
            ChangeSource::Git {
                branch: Some("main".into())
            }
        );
        let mut summary: Vec<(&str, FileStatus, usize, usize)> = set
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
            .collect();
        summary.sort_by_key(|entry| entry.0);
        assert_eq!(
            summary,
            vec![
                ("gone.txt", FileStatus::Deleted, 0, 1),
                ("kept.txt", FileStatus::Modified, 1, 1),
                ("new file.txt", FileStatus::Added, 1, 0),
            ]
        );
        assert_eq!(set.totals(), (2, 2));
    }

    #[test]
    fn a_repository_with_no_commits_shows_everything_as_added() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        if sh(root, &["init", "-q"]).is_none() {
            return;
        }
        std::fs::write(root.join("staged.txt"), "a\n").unwrap();
        std::fs::write(root.join("loose.txt"), "b\n").unwrap();
        sh(root, &["add", "staged.txt"]).unwrap();

        let set = changes(root);
        let mut paths: Vec<&str> = set.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["loose.txt", "staged.txt"]);
        assert!(set.files.iter().all(|f| f.status == FileStatus::Added));
        assert!(set
            .files
            .iter()
            .all(|f| f.hunks[0].lines[0].kind == LineKind::Added));
    }

    #[test]
    fn ignored_files_are_not_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        if sh(root, &["init", "-q"]).is_none() {
            return;
        }
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        sh(root, &["add", "."]).unwrap();
        sh(root, &["commit", "-q", "-m", "first"]).unwrap();
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out.o"), "x").unwrap();

        assert!(changes(root).files.is_empty());
    }
}
