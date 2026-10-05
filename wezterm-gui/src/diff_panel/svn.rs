//! Subversion provider: the working copy against its pristine BASE.

use super::exec::Runner;
use super::{
    read_text_file, unified, ChangeSet, ChangeSource, FileChange, FileStatus, Limits, Scan,
    MAX_UNTRACKED_FILES,
};
use std::collections::HashMap;
use std::path::Path;

/// What `svn status` says about one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SvnState {
    Tracked(FileStatus),
    /// `?`: on disk but not under version control.
    Unversioned,
}

/// Parse plain `svn status` output: seven single-character columns, a space,
/// then the path. Only the first column (the item's own state) matters here.
fn parse_status(output: &str) -> Vec<(String, SvnState)> {
    output
        .lines()
        .filter_map(|line| {
            let state = match line.as_bytes().first()? {
                b'M' | b'R' => SvnState::Tracked(FileStatus::Modified),
                b'A' => SvnState::Tracked(FileStatus::Added),
                // `!` is missing: deleted on disk without `svn delete`.
                b'D' | b'!' => SvnState::Tracked(FileStatus::Deleted),
                b'C' => SvnState::Tracked(FileStatus::Conflicted),
                b'?' => SvnState::Unversioned,
                // Blank first column (property-only change), externals,
                // ignored items and the changelist headers.
                _ => return None,
            };
            let path = line.get(8..)?.trim();
            if path.is_empty() {
                return None;
            }
            Some((path.replace('\\', "/"), state))
        })
        .collect()
}

pub fn scan(root: &Path, runner: &Runner, limits: Limits) -> Scan {
    let status = match runner.run("svn", &["status"], root) {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Scan::Unavailable(
                "This is a Subversion working copy, but svn is not installed".into(),
            );
        }
        Err(err) => return Scan::Unavailable(format!("Could not run svn: {err}")),
    };
    if let Some(reason) = status.timeout_reason("svn") {
        return Scan::Unavailable(reason);
    }
    if !status.success && !status.truncated {
        let reason = status.stderr.lines().next().unwrap_or("svn status failed");
        return Scan::Unavailable(reason.trim().to_string());
    }
    let states = parse_status(&status.stdout);

    // `--internal-diff`: never hand off to a diff tool the user configured.
    let diff = match runner.run("svn", &["diff", "--internal-diff"], root) {
        Ok(out) => out,
        Err(err) => return Scan::Unavailable(format!("Could not run svn: {err}")),
    };
    let mut files = unified::parse(&diff.stdout);
    for file in &mut files {
        file.path = file.path.replace('\\', "/");
    }
    let mut note = diff.timeout_reason("svn").or_else(|| {
        (diff.truncated || status.truncated)
            .then(|| "The diff is too large to show in full".to_string())
    });

    // `svn status` knows things the diff does not say outright: conflicts,
    // and changed files the diff skipped.
    let mut index: HashMap<String, usize> = files
        .iter()
        .enumerate()
        .map(|(idx, file)| (file.path.clone(), idx))
        .collect();
    let mut unversioned = 0usize;
    for (path, state) in states {
        match state {
            SvnState::Tracked(status) => match index.get(&path) {
                Some(idx) => {
                    if status == FileStatus::Conflicted {
                        files[*idx].status = status;
                    }
                }
                None => {
                    // A directory shows up in status but has no diff of its
                    // own; its files are listed separately.
                    if root.join(&path).is_dir() {
                        continue;
                    }
                    index.insert(path.clone(), files.len());
                    files.push(FileChange::new(path, status));
                }
            },
            SvnState::Unversioned => {
                let full = root.join(&path);
                if full.is_dir() {
                    // Status lists an unversioned directory once, without
                    // descending. Say it is there; do not walk it.
                    let mut file = FileChange::new(format!("{path}/"), FileStatus::Added);
                    file.note = Some("unversioned directory".to_string());
                    files.push(file);
                    continue;
                }
                unversioned += 1;
                if unversioned > MAX_UNTRACKED_FILES {
                    files.push(FileChange::new(path, FileStatus::Added));
                    continue;
                }
                files.push(match read_text_file(&full, limits.max_file_bytes) {
                    Ok(text) => FileChange::all_added(path, &text),
                    Err(reason) => {
                        let mut file = FileChange::new(path, FileStatus::Added);
                        file.note = Some(reason);
                        file
                    }
                });
            }
        }
    }
    if unversioned > MAX_UNTRACKED_FILES && note.is_none() {
        note = Some(format!(
            "{unversioned} unversioned files; contents shown for the first {MAX_UNTRACKED_FILES}"
        ));
    }

    Scan::Changes(ChangeSet {
        source: ChangeSource::Svn,
        root: root.to_path_buf(),
        files,
        note: note.or_else(|| runner.fallback_note("svn")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_columns_are_read_by_position() {
        let output = "\
M       src/main.c
A  +    src/copied.c
D       old.c
!       missing.c
C       fight.c
?       notes with spaces.txt
 M      props-only
X       external
I       ignored.o
--- Changelist 'work':
";
        assert_eq!(
            parse_status(output),
            vec![
                ("src/main.c".into(), SvnState::Tracked(FileStatus::Modified)),
                ("src/copied.c".into(), SvnState::Tracked(FileStatus::Added)),
                ("old.c".into(), SvnState::Tracked(FileStatus::Deleted)),
                ("missing.c".into(), SvnState::Tracked(FileStatus::Deleted)),
                ("fight.c".into(), SvnState::Tracked(FileStatus::Conflicted)),
                ("notes with spaces.txt".into(), SvnState::Unversioned),
            ]
        );
    }

    #[test]
    fn windows_separators_are_normalised() {
        assert_eq!(
            parse_status("M       src\\win\\a.c\n"),
            vec![(
                "src/win/a.c".to_string(),
                SvnState::Tracked(FileStatus::Modified)
            )]
        );
    }

    #[test]
    fn a_real_working_copy_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let wc = tmp.path().join("wc");
        let run = |program: &str, args: &[&str], cwd: &Path| {
            std::process::Command::new(program)
                .args(args)
                .current_dir(cwd)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        if !run("svnadmin", &["create", repo.to_str().unwrap()], tmp.path()) {
            eprintln!("svn not installed; skipping");
            return;
        }
        let url = format!("file://{}", repo.display());
        assert!(run(
            "svn",
            &["checkout", "-q", &url, wc.to_str().unwrap()],
            tmp.path()
        ));
        std::fs::write(wc.join("kept.txt"), "one\ntwo\n").unwrap();
        assert!(run("svn", &["add", "-q", "kept.txt"], &wc));
        assert!(run("svn", &["commit", "-q", "-m", "first"], &wc));

        std::fs::write(wc.join("kept.txt"), "one\n2\n").unwrap();
        std::fs::write(wc.join("loose.txt"), "fresh\n").unwrap();

        let limits = Limits {
            max_file_bytes: 1024 * 1024,
            snapshot_max_files: 1000,
        };
        let Scan::Changes(set) = scan(&wc, &Runner::host(), limits) else {
            panic!("scan unavailable");
        };
        assert_eq!(set.source, ChangeSource::Svn);
        let mut summary: Vec<(&str, FileStatus, usize, usize)> = set
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed))
            .collect();
        summary.sort_by_key(|entry| entry.0);
        assert_eq!(
            summary,
            vec![
                ("kept.txt", FileStatus::Modified, 1, 1),
                ("loose.txt", FileStatus::Added, 1, 0),
            ]
        );
    }
}
