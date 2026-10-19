//! What the tree does to files: moving them (dragged onto a folder),
//! creating and renaming them, and spelling their paths and folders for a
//! terminal (dragged onto a pane, "cd here").
//!
//! Planning is pure; [`apply_moves`] touches the disk and runs on a worker
//! thread.

use super::{join_rel, parent_rel, EntryKind};
use std::path::Path;

/// Of `picked`, the ones not inside another picked folder: moving a folder
/// already moves what is in it. Order is kept.
pub fn outermost(picked: &[(String, EntryKind)]) -> Vec<(String, EntryKind)> {
    picked
        .iter()
        .filter(|(rel, _)| {
            !picked.iter().any(|(other, kind)| {
                *kind == EntryKind::Dir
                    && other != rel
                    && rel
                        .strip_prefix(other.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .cloned()
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub from: String,
    pub to: String,
}

fn name_of(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Whether `rel` is `folder` or inside it.
fn within(rel: &str, folder: &str) -> bool {
    folder.is_empty()
        || rel == folder
        || rel
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The renames that move `sources` into the folder `dest` (`""` is the
/// root). Items already in `dest` are left out, so the plan may be empty.
pub fn plan_move(sources: &[(String, EntryKind)], dest: &str) -> Result<Vec<Move>, String> {
    let mut moves: Vec<Move> = Vec::new();
    for (from, kind) in outermost(sources) {
        if from.is_empty() {
            continue;
        }
        if kind == EntryKind::Dir && within(dest, &from) {
            return Err(format!("Cannot move {} into itself", name_of(&from)));
        }
        if parent_rel(&from) == Some(dest) {
            continue;
        }
        let to = join_rel(dest, name_of(&from));
        if moves.iter().any(|planned| planned.to == to) {
            return Err(format!("Two items are named {}", name_of(&from)));
        }
        moves.push(Move { from, to });
    }
    Ok(moves)
}

/// Whether a drop of `sources` on `dest` would do anything at all, for the
/// drop highlight.
pub fn is_drop_target(sources: &[(String, EntryKind)], dest: &str) -> bool {
    plan_move(sources, dest).is_ok_and(|moves| !moves.is_empty())
}

/// Carry out `moves` under `root`. Nothing is overwritten: when any target
/// exists, nothing moves. Returns how many moved; an error says which move
/// failed after the earlier ones were done.
pub fn apply_moves(root: &Path, moves: &[Move]) -> Result<usize, String> {
    let abs = |rel: &str| {
        let mut path = root.to_path_buf();
        for part in rel.split('/').filter(|part| !part.is_empty()) {
            path.push(part);
        }
        path
    };
    for planned in moves {
        if std::fs::symlink_metadata(abs(&planned.to)).is_ok() {
            return Err(format!("{} already exists", planned.to));
        }
    }
    for (done, planned) in moves.iter().enumerate() {
        if let Err(err) = std::fs::rename(abs(&planned.from), abs(&planned.to)) {
            let moved = if done == 0 {
                String::new()
            } else {
                format!(" ({done} moved before it)")
            };
            return Err(format!("Could not move {}: {err}{moved}", planned.from));
        }
    }
    Ok(moves.len())
}

/// How the program in a pane reads a pasted path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteSyntax {
    /// sh, bash, zsh, fish, and agents running under them.
    Posix,
    /// PowerShell and cmd.
    Windows,
}

/// `paths` as text to paste into a pane: quoted where needed, separated by
/// spaces, with a trailing space so typing can go straight on.
pub fn paste_text(paths: &[String], syntax: PasteSyntax) -> String {
    let quoted: Vec<String> = paths
        .iter()
        .map(|path| match syntax {
            PasteSyntax::Posix => shlex::try_quote(path)
                .map(|quoted| quoted.into_owned())
                .unwrap_or_else(|_| path.clone()),
            PasteSyntax::Windows => {
                let plain = path
                    .chars()
                    .all(|c| c.is_alphanumeric() || "\\/:._-".contains(c));
                if plain {
                    path.clone()
                } else {
                    format!("\"{path}\"")
                }
            }
        })
        .collect();
    format!("{} ", quoted.join(" "))
}

/// Why `name` cannot be a file or folder name, if it cannot.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("A name is needed".to_string());
    }
    if trimmed != name {
        return Err("A name cannot start or end with a space".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("{name} is not a name"));
    }
    if let Some(bad) = name.chars().find(|c| matches!(c, '/' | '\\' | '\0')) {
        let shown = if bad == '\0' {
            "NUL".to_string()
        } else {
            bad.to_string()
        };
        return Err(format!("A name cannot contain {shown}"));
    }
    Ok(())
}

/// The relative path `rel` gets when renamed to `name`, in the same folder.
pub fn renamed(rel: &str, name: &str) -> Result<String, String> {
    validate_name(name)?;
    Ok(join_rel(parent_rel(rel).unwrap_or(""), name))
}

/// Rename `from` to `to` under `root`, never onto another file. A change of
/// case only is allowed where the filesystem ignores case.
pub fn apply_rename(root: &Path, from: &str, to: &str) -> Result<(), String> {
    let abs = |rel: &str| {
        let mut path = root.to_path_buf();
        for part in rel.split('/').filter(|part| !part.is_empty()) {
            path.push(part);
        }
        path
    };
    let (from_abs, to_abs) = (abs(from), abs(to));
    let case_only = from.to_lowercase() == to.to_lowercase() && from != to;
    if !case_only && std::fs::symlink_metadata(&to_abs).is_ok() {
        return Err(format!("{to} already exists"));
    }
    std::fs::rename(&from_abs, &to_abs).map_err(|err| format!("Could not rename {from}: {err}"))
}

/// Create an empty file (or a folder, with `folder`) at `rel` under `root`.
pub fn create(root: &Path, rel: &str, folder: bool) -> Result<(), String> {
    let mut path = root.to_path_buf();
    for part in rel.split('/').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    let result = if folder {
        std::fs::create_dir(&path)
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|_| ())
    };
    result.map_err(|err| match err.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{rel} already exists"),
        _ => format!("Could not create {rel}: {err}"),
    })
}

/// The shell a pane runs, as far as typing a `cd` into it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    PowerShell,
    Cmd,
}

impl ShellKind {
    /// From the basename of the pane's foreground program.
    pub fn from_program(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "pwsh" | "powershell" => ShellKind::PowerShell,
            "cmd" => ShellKind::Cmd,
            _ => ShellKind::Posix,
        }
    }
}

/// What to type into a shell to move it to `dir`: the line cleared first,
/// then the `cd`, then Enter.
pub fn cd_command(dir: &str, shell: ShellKind) -> String {
    match shell {
        ShellKind::Posix => {
            let quoted = shlex::try_quote(dir)
                .map(|quoted| quoted.into_owned())
                .unwrap_or_else(|_| dir.to_string());
            format!("\x15cd -- {quoted}\r")
        }
        // PSReadLine and cmd both clear the line on Escape.
        ShellKind::PowerShell => format!(
            "\x1bSet-Location -LiteralPath '{}'\r",
            dir.replace('\'', "''")
        ),
        ShellKind::Cmd => format!("\x1bcd /d \"{dir}\"\r"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn items(list: &[(&str, EntryKind)]) -> Vec<(String, EntryKind)> {
        list.iter()
            .map(|(rel, kind)| (rel.to_string(), *kind))
            .collect()
    }

    const D: EntryKind = EntryKind::Dir;
    const F: EntryKind = EntryKind::File;

    #[test]
    fn a_folder_carries_its_contents() {
        let picked = items(&[("src", D), ("src/main.rs", F), ("srcx/a", F)]);
        assert_eq!(outermost(&picked), items(&[("src", D), ("srcx/a", F)]));
    }

    #[test]
    fn moves_into_a_folder() {
        let plan = plan_move(&items(&[("a.txt", F), ("docs/b.md", F)]), "src").unwrap();
        assert_eq!(
            plan,
            [
                Move {
                    from: "a.txt".into(),
                    to: "src/a.txt".into()
                },
                Move {
                    from: "docs/b.md".into(),
                    to: "src/b.md".into()
                },
            ]
        );
        let plan = plan_move(&items(&[("src/x", F)]), "").unwrap();
        assert_eq!(plan[0].to, "x");
    }

    #[test]
    fn a_drop_on_the_own_folder_does_nothing() {
        assert_eq!(plan_move(&items(&[("src/x", F)]), "src"), Ok(vec![]));
        assert!(!is_drop_target(&items(&[("src/x", F)]), "src"));
        assert!(is_drop_target(&items(&[("src/x", F)]), "docs"));
    }

    #[test]
    fn a_folder_cannot_go_into_itself() {
        assert!(plan_move(&items(&[("src", D)]), "src").is_err());
        assert!(plan_move(&items(&[("src", D)]), "src/ui").is_err());
        // A sibling with a longer name is not inside it.
        assert!(plan_move(&items(&[("src", D)]), "srcx").is_ok());
    }

    #[test]
    fn two_items_cannot_land_on_one_name() {
        assert!(plan_move(&items(&[("a/x", F), ("b/x", F)]), "c").is_err());
    }

    #[test]
    fn applying_moves_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        fs::write(root.join("src/b.txt"), "old").unwrap();

        let clash = plan_move(&items(&[("a.txt", F), ("b.txt", F)]), "src").unwrap();
        assert!(apply_moves(root, &clash).is_err());
        // Nothing moved: the check runs before the first rename.
        assert!(root.join("a.txt").exists());
        assert_eq!(fs::read_to_string(root.join("src/b.txt")).unwrap(), "old");

        let plan = plan_move(&items(&[("a.txt", F)]), "src").unwrap();
        assert_eq!(apply_moves(root, &plan), Ok(1));
        assert!(root.join("src/a.txt").exists());
        assert!(!root.join("a.txt").exists());
    }

    #[test]
    fn names_must_be_plain() {
        assert!(validate_name("main.rs").is_ok());
        assert!(validate_name(".env").is_ok());
        for bad in [
            "", "  ", " lead", "trail ", ".", "..", "a/b", "a\\b", "a\0b",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(renamed("src/a.rs", "b.rs"), Ok("src/b.rs".into()));
        assert_eq!(renamed("a.rs", "b.rs"), Ok("b.rs".into()));
        assert!(renamed("a.rs", "x/y").is_err());
    }

    #[test]
    fn create_and_rename_never_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(create(root, "a.txt", false), Ok(()));
        assert!(create(root, "a.txt", false).is_err());
        assert_eq!(create(root, "src", true), Ok(()));
        assert!(root.join("src").is_dir());
        fs::write(root.join("b.txt"), "b").unwrap();
        assert!(apply_rename(root, "a.txt", "b.txt").is_err());
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "b");
        assert_eq!(apply_rename(root, "a.txt", "c.txt"), Ok(()));
        assert!(root.join("c.txt").exists());
        // A change of case only goes through even where names ignore case.
        assert_eq!(apply_rename(root, "c.txt", "C.txt"), Ok(()));
        assert!(fs::read_dir(root)
            .unwrap()
            .any(|entry| entry.unwrap().file_name() == "C.txt"));
    }

    #[test]
    fn cd_is_typed_for_the_shell_in_the_pane() {
        assert_eq!(ShellKind::from_program("zsh"), ShellKind::Posix);
        assert_eq!(ShellKind::from_program("pwsh.exe"), ShellKind::PowerShell);
        assert_eq!(ShellKind::from_program("CMD.EXE"), ShellKind::Cmd);
        assert_eq!(
            cd_command("/r/a b", ShellKind::Posix),
            "\x15cd -- '/r/a b'\r"
        );
        assert_eq!(
            cd_command(r"C:\it's", ShellKind::PowerShell),
            "\x1bSet-Location -LiteralPath 'C:\\it''s'\r"
        );
        assert_eq!(
            cd_command(r"C:\a b", ShellKind::Cmd),
            "\x1bcd /d \"C:\\a b\"\r"
        );
    }

    #[test]
    fn pasted_paths_are_quoted_for_the_shell() {
        let paths = vec!["/r/plain.rs".to_string(), "/r/with space's.md".to_string()];
        assert_eq!(
            paste_text(&["/r/a b".to_string()], PasteSyntax::Posix),
            "'/r/a b' "
        );
        assert_eq!(
            paste_text(&paths, PasteSyntax::Posix),
            r#"/r/plain.rs "/r/with space's.md" "#
        );
        let paths = vec![r"C:\r\plain.rs".to_string(), r"C:\r\a b.md".to_string()];
        assert_eq!(
            paste_text(&paths, PasteSyntax::Windows),
            r#"C:\r\plain.rs "C:\r\a b.md" "#
        );
    }
}
