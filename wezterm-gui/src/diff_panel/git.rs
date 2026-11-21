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

/// Keys of a repository's own configuration that name a command Git runs
/// while the panel reads the working copy.
const COMMAND_KEYS: &str =
    r"^(core\.fsmonitor|diff\..+\.textconv|filter\..+\.(clean|smudge|process))$";

/// What a scan switches off because the repository's own configuration set
/// it. The panel runs Git by itself in any directory a pane visits, a
/// downloaded archive or a nested checkout the user never opened included;
/// its `.git/config` must not get to run a command. The user's global and
/// system configuration is not touched, and `diff_panel.trusted_repos` lifts
/// all of this for a repository.
#[derive(Debug, Default, PartialEq, Eq)]
struct Guard {
    /// `-c` overrides, which outrank every configuration file.
    overrides: Vec<String>,
    no_textconv: bool,
    /// Not trusted: `git diff` would run `git status` in each submodule,
    /// under that submodule's own configuration, to tell whether it is dirty.
    ignore_dirty_submodules: bool,
}

impl Guard {
    /// The guard for `entries`, the repository's own `(key, value)` pairs
    /// matching [`COMMAND_KEYS`].
    fn for_entries<'a>(entries: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> Self {
        let mut guard = Guard {
            ignore_dirty_submodules: true,
            ..Guard::default()
        };
        for (key, value) in entries {
            let key = key.to_ascii_lowercase();
            if key == "core.fsmonitor" {
                // `true` is Git's own daemon; anything else names a hook.
                let builtin = value.is_none_or(|value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "" | "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
                    )
                });
                let off = "core.fsmonitor=false".to_string();
                if !builtin && !guard.overrides.contains(&off) {
                    guard.overrides.push(off);
                }
            } else if key.starts_with("diff.") {
                guard.no_textconv = true;
            } else if let Some(name) = key
                .strip_prefix("filter.")
                .and_then(|rest| rest.rsplit_once('.'))
                .map(|(name, _)| name)
            {
                // An empty command is no filter; `required` would turn the
                // missing filter into an error.
                for (setting, value) in [
                    ("clean", ""),
                    ("smudge", ""),
                    ("process", ""),
                    ("required", "false"),
                ] {
                    let entry = format!("filter.{name}.{setting}={value}");
                    if !guard.overrides.contains(&entry) {
                        guard.overrides.push(entry);
                    }
                }
            }
        }
        guard
    }
}

/// `git config -z` output: `key\nvalue` records, each after a `scope` record
/// when `scoped`. Only the repository's own (`local`, `worktree`) are kept.
fn own_entries(output: &str, scoped: bool) -> Vec<(&str, Option<&str>)> {
    let mut records = output.split('\0').filter(|record| !record.is_empty());
    let mut entries = Vec::new();
    loop {
        let scope = if scoped {
            match records.next() {
                Some(scope) => Some(scope),
                None => break,
            }
        } else {
            None
        };
        let Some(record) = records.next() else {
            break;
        };
        if scope.is_some_and(|scope| scope != "local" && scope != "worktree") {
            continue;
        }
        entries.push(match record.split_once('\n') {
            Some((key, value)) => (key, Some(value)),
            None => (record, None),
        });
    }
    entries
}

/// The [`Guard`] for the repository at `root`.
fn guard_for(runner: &Runner, root: &Path) -> Guard {
    if runner.trusts(root) {
        return Guard::default();
    }
    let probe = |args: &[&str]| {
        let mut full = vec!["config"];
        full.extend_from_slice(args);
        full.extend(["-z", "--get-regexp", COMMAND_KEYS]);
        runner.run("git", &full, root)
    };
    // `--show-scope` (Git 2.26) tells the repository's settings from the
    // user's, includes too. An older Git reads the repository's file alone.
    let output = match probe(&["--show-scope"]) {
        Ok(out) if out.success => return Guard::for_entries(own_entries(&out.stdout, true)),
        // No match: exit 1 with nothing said.
        Ok(out) if out.stderr.trim().is_empty() => return Guard::for_entries([]),
        _ => probe(&["--local"]),
    };
    match output {
        Ok(out) if out.success => Guard::for_entries(own_entries(&out.stdout, false)),
        _ => Guard::for_entries([]),
    }
}

fn git(
    runner: &Runner,
    root: &Path,
    guard: &Guard,
    args: &[&str],
) -> std::io::Result<CommandOutput> {
    let mut full = vec![
        "--no-optional-locks",
        // Paths as they are, not octal-escaped.
        "-c",
        "core.quotepath=off",
    ];
    for entry in &guard.overrides {
        full.extend(["-c", entry.as_str()]);
    }
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
    guard: &Guard,
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
    ];
    if guard.no_textconv {
        args.push("--no-textconv");
    }
    if guard.ignore_dirty_submodules {
        args.push("--ignore-submodules=dirty");
    }
    args.extend([base, "--"]);
    args.extend(pathspecs.iter().map(String::as_str));
    git(runner, root, guard, &args)
}

/// NUL-separated paths, as `ls-files -z` prints them.
fn parse_path_list(output: &str) -> Vec<&str> {
    output.split('\0').filter(|path| !path.is_empty()).collect()
}

fn untracked_file(root: &Path, path: &str, limits: Limits, read_content: bool) -> FileChange {
    if !read_content {
        return FileChange::new(path, FileStatus::Added);
    }
    match read_text_file(&root.join(path), root, limits.max_file_bytes) {
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
    let guard = guard_for(runner, root);
    let branch = match git(
        runner,
        root,
        &guard,
        &["symbolic-ref", "--short", "-q", "HEAD"],
    ) {
        Ok(out) if out.success => Some(out.stdout.trim().to_string()).filter(|b| !b.is_empty()),
        // Detached HEAD: a repository, just not on a branch.
        Ok(_) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Scan::Unavailable("This is a Git repository, but git is not installed".into());
        }
        Err(err) => return Scan::Unavailable(format!("Could not run git: {err}")),
    };

    let mut diff = match run_chunked(&literal_pathspecs(paths), |specs| {
        diff_against(runner, root, &guard, "HEAD", specs)
    }) {
        Ok(out) => out,
        Err(err) => return Scan::Unavailable(format!("Could not run git: {err}")),
    };
    if !diff.success && !diff.truncated {
        // No commit yet, so no HEAD to compare with.
        match run_chunked(&literal_pathspecs(paths), |specs| {
            diff_against(runner, root, &guard, EMPTY_TREE, specs)
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
        git(runner, root, &guard, &args)
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
    fn only_the_repositorys_own_settings_are_switched_off() {
        let scoped = "global\0filter.lfs.clean\ngit-lfs clean -- %f\0\
                      local\0core.fsmonitor\n.git/hook\0\
                      local\0filter.evil.process\nrun\0\
                      worktree\0diff.x.textconv\ncat\0";
        let entries = own_entries(scoped, true);
        assert_eq!(
            entries,
            [
                ("core.fsmonitor", Some(".git/hook")),
                ("filter.evil.process", Some("run")),
                ("diff.x.textconv", Some("cat")),
            ]
        );
        let guard = Guard::for_entries(entries);
        assert_eq!(
            guard.overrides,
            [
                "core.fsmonitor=false",
                "filter.evil.clean=",
                "filter.evil.smudge=",
                "filter.evil.process=",
                "filter.evil.required=false",
            ]
        );
        assert!(guard.no_textconv && guard.ignore_dirty_submodules);
        // Git's own fsmonitor daemon runs nothing from the repository.
        let builtin = Guard::for_entries(own_entries("core.fsmonitor\ntrue\0", false));
        assert!(builtin.overrides.is_empty() && !builtin.no_textconv);
    }

    /// A repository whose own configuration runs a command at every turn of
    /// a scan, each leaving a marker file when it does.
    #[cfg(unix)]
    fn hostile_repo(root: &Path) -> Option<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let markers = root
            .parent()?
            .join(format!("{}-markers", root.file_name()?.to_string_lossy()));
        std::fs::create_dir_all(&markers).unwrap();
        sh(root, &["init", "-q"])?;
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join("b.dat"), "data\n").unwrap();
        std::fs::write(
            root.join(".gitattributes"),
            "*.txt diff=evil\n*.dat filter=evil\n",
        )
        .unwrap();
        sh(root, &["add", "-A"])?;
        sh(root, &["commit", "-qm", "init"])?;
        let hook = |name: &str, body: &str| {
            let path = markers.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\ntouch '{}/ran-{name}'\n{body}\n",
                    markers.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        let fsmonitor = hook("fsmonitor", "exit 1");
        let textconv = hook("textconv", "echo CONVERTED; cat \"$1\"");
        let clean = hook("clean", "cat");
        sh(root, &["config", "core.fsmonitor", &fsmonitor])?;
        sh(root, &["config", "diff.evil.textconv", &textconv])?;
        sh(root, &["config", "filter.evil.clean", &clean])?;
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("b.dat"), "data\nmore\n").unwrap();
        Some(markers)
    }

    #[cfg(unix)]
    #[test]
    fn a_repositorys_own_config_runs_nothing_unless_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let Some(markers) = hostile_repo(&root) else {
            return;
        };
        let ran = || -> Vec<String> {
            let mut ran: Vec<String> = std::fs::read_dir(&markers)
                .unwrap()
                .filter_map(|entry| {
                    let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                    name.strip_prefix("ran-").map(str::to_string)
                })
                .collect();
            ran.sort();
            ran
        };

        let set = changes(&root);
        assert_eq!(ran(), Vec::<String>::new());
        let a = set.files.iter().find(|file| file.path == "a.txt").unwrap();
        assert!(!format!("{:?}", a.hunks).contains("CONVERTED"));
        assert!(set.files.iter().any(|file| file.path == "b.dat"));

        // Trusted, the repository's textconv shapes the diff again.
        let runner = Runner::host().trusting(vec![root.clone()]);
        let Scan::Changes(set) = scan(&root, &runner, limits()) else {
            panic!("scan unavailable");
        };
        let a = set.files.iter().find(|file| file.path == "a.txt").unwrap();
        assert!(format!("{:?}", a.hunks).contains("CONVERTED"));
        assert!(ran().contains(&"textconv".to_string()));
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
