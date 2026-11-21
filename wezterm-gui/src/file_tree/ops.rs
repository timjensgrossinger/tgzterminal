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
        if let Err(err) = rename_new(&abs(&planned.from), &abs(&planned.to)) {
            let moved = if done == 0 {
                String::new()
            } else {
                format!(" ({done} moved before it)")
            };
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                return Err(format!("{} already exists{moved}", planned.to));
            }
            return Err(format!("Could not move {}: {err}{moved}", planned.from));
        }
    }
    Ok(moves.len())
}

/// `std::fs::rename` that fails rather than replace a file that appeared at
/// `to` after it was checked. Where the system has no such rename, it is the
/// check and a plain rename.
fn rename_new(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(any(
        target_os = "macos",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
    ))]
    {
        use std::os::unix::ffi::OsStrExt;
        let from_c = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let to_c = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both are NUL-terminated paths that outlive the call.
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
        // SAFETY: as above; AT_FDCWD resolves relative paths as rename does.
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from_c.as_ptr(),
                libc::AT_FDCWD,
                to_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // A filesystem (or kernel) without the flag: fall through.
        if !matches!(
            err.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP)
        ) {
            return Err(err);
        }
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

/// Whether `a` and `b` are one file: on a filesystem that ignores case, two
/// spellings of a name.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        // The final path carries the name's case as stored.
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

/// Whether typing `text` would press a key: a control character in a path
/// (a file name may hold any byte but `/` and NUL) is Enter, Ctrl-U or
/// Escape to the line editor reading it.
pub fn has_control(text: &str) -> bool {
    text.chars().any(is_control)
}

fn is_control(c: char) -> bool {
    c < ' ' || c == '\x7f'
}

/// `path` quoted for bash, zsh or ksh. A path with control characters is
/// spelled in `$'…'` with them as `\xHH`, so only printable text is typed.
fn quote_posix(path: &str) -> String {
    if !has_control(path) {
        return shlex::try_quote(path)
            .map(|quoted| quoted.into_owned())
            .unwrap_or_else(|_| path.to_string());
    }
    let mut quoted = String::from("$'");
    for c in path.chars() {
        match c {
            '\\' | '\'' => {
                quoted.push('\\');
                quoted.push(c);
            }
            c if is_control(c) => quoted.push_str(&format!("\\x{:02x}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

/// `path` quoted for fish, which has no `$'…'`: single-quoted runs, with
/// control characters between them as unquoted `\xHH`.
fn quote_fish(path: &str) -> String {
    fn flush(run: &mut String, quoted: &mut String) {
        if !run.is_empty() {
            quoted.push('\'');
            quoted.push_str(run);
            quoted.push('\'');
            run.clear();
        }
    }
    let mut quoted = String::new();
    let mut run = String::new();
    for c in path.chars() {
        if is_control(c) {
            flush(&mut run, &mut quoted);
            quoted.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            if matches!(c, '\\' | '\'') {
                run.push('\\');
            }
            run.push(c);
        }
    }
    flush(&mut run, &mut quoted);
    if quoted.is_empty() {
        quoted.push_str("''");
    }
    quoted
}

/// `path` in PowerShell's single quotes. PowerShell ends such a string on
/// the typographic single quotes too, not only `'`; each is doubled to stand
/// for itself.
fn quote_powershell(path: &str) -> String {
    let mut quoted = String::from("'");
    for c in path.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

/// `paths` as text to paste into a pane running `shell`: quoted where
/// needed, separated by spaces, with a trailing space so typing can go
/// straight on. Never contains a control character.
pub fn paste_text(paths: &[String], shell: ShellKind) -> String {
    let quoted: Vec<String> = paths
        .iter()
        .map(|path| {
            let plain = path
                .chars()
                .all(|c| c.is_alphanumeric() || "\\/:._-".contains(c));
            match shell {
                ShellKind::Posix | ShellKind::Other => quote_posix(path),
                ShellKind::Fish => quote_fish(path),
                ShellKind::PowerShell if plain => path.clone(),
                ShellKind::PowerShell => quote_powershell(path),
                // A Windows name holds neither `"` nor control characters.
                ShellKind::Cmd if plain => path.clone(),
                ShellKind::Cmd => format!("\"{path}\""),
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
    // Windows reserves more: `C:x` would land on another drive and `a:b` in
    // an alternate stream. Its filesystems cannot store any of these anyway.
    let forbidden = |c: char| {
        matches!(c, '/' | '\\' | '\0')
            || (cfg!(windows)
                && (matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*') || is_control(c)))
    };
    if let Some(bad) = name.chars().find(|c| forbidden(*c)) {
        let shown = match bad {
            '\0' => "NUL".to_string(),
            c if is_control(c) => format!("U+{:04X}", c as u32),
            c => c.to_string(),
        };
        return Err(format!("A name cannot contain {shown}"));
    }
    let mut parts = Path::new(name).components();
    if !matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err(format!("{name} is not a name"));
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
    let failed = |err: std::io::Error| match err.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{to} already exists"),
        _ => format!("Could not rename {from}: {err}"),
    };
    if std::fs::symlink_metadata(&to_abs).is_ok() {
        // Where names ignore case, the new spelling finds the file itself;
        // where they do not, it is another file and stays.
        if case_only && same_file(&from_abs, &to_abs) {
            return std::fs::rename(&from_abs, &to_abs).map_err(failed);
        }
        return Err(format!("{to} already exists"));
    }
    rename_new(&from_abs, &to_abs).map_err(failed)
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

/// The shell a pane runs, as far as typing a path into it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    /// bash, zsh and ksh, which read `$'…'`.
    Posix,
    Fish,
    PowerShell,
    Cmd,
    /// Anything else (`sh`, an agent, …): POSIX quoting, but no `$'…'`.
    Other,
}

impl ShellKind {
    /// From the basename of the pane's foreground program.
    pub fn from_program(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        // A login shell's name starts with `-`.
        let name = name.trim_start_matches('-');
        let name = name.strip_suffix(".exe").unwrap_or(name);
        match name {
            "bash" | "zsh" | "ksh" | "mksh" => ShellKind::Posix,
            "fish" => ShellKind::Fish,
            "pwsh" | "powershell" => ShellKind::PowerShell,
            "cmd" => ShellKind::Cmd,
            _ => ShellKind::Other,
        }
    }
}

/// What to type into a shell to move it to `dir`: the line cleared first,
/// then the `cd`, then Enter. `None` when `dir` cannot be spelled for that
/// shell in printable text; the caller opens a shell there instead.
pub fn cd_command(dir: &str, shell: ShellKind) -> Option<String> {
    match shell {
        ShellKind::Posix => Some(format!("\x15cd -- {}\r", quote_posix(dir))),
        ShellKind::Fish => Some(format!("\x15cd -- {}\r", quote_fish(dir))),
        _ if has_control(dir) => None,
        ShellKind::Other => Some(format!("\x15cd -- {}\r", quote_posix(dir))),
        // PSReadLine and cmd both clear the line on Escape.
        ShellKind::PowerShell => Some(format!(
            "\x1bSet-Location -LiteralPath {}\r",
            quote_powershell(dir)
        )),
        ShellKind::Cmd => Some(format!("\x1bcd /d \"{dir}\"\r")),
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
    fn names_with_a_drive_or_stream_are_refused_on_windows() {
        for name in ["C:x", "a:b", "a|b", "a\x01b"] {
            assert_eq!(validate_name(name).is_err(), cfg!(windows), "{name:?}");
        }
    }

    #[test]
    fn a_case_rename_never_replaces_another_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("A.txt"), "upper").unwrap();
        let separate = fs::read_dir(root).unwrap().count() == 2;
        if separate {
            // A filesystem that keeps case: these are two files.
            assert!(apply_rename(root, "a.txt", "A.txt").is_err());
            assert_eq!(fs::read_to_string(root.join("A.txt")).unwrap(), "upper");
        }
    }

    #[test]
    fn cd_is_typed_for_the_shell_in_the_pane() {
        assert_eq!(ShellKind::from_program("zsh"), ShellKind::Posix);
        assert_eq!(ShellKind::from_program("-bash"), ShellKind::Posix);
        assert_eq!(ShellKind::from_program("fish"), ShellKind::Fish);
        assert_eq!(ShellKind::from_program("sh"), ShellKind::Other);
        assert_eq!(ShellKind::from_program("pwsh.exe"), ShellKind::PowerShell);
        assert_eq!(ShellKind::from_program("CMD.EXE"), ShellKind::Cmd);
        assert_eq!(
            cd_command("/r/a b", ShellKind::Posix).as_deref(),
            Some("\x15cd -- '/r/a b'\r")
        );
        assert_eq!(
            cd_command("/r/a b", ShellKind::Other).as_deref(),
            Some("\x15cd -- '/r/a b'\r")
        );
        assert_eq!(
            cd_command("/r/a b", ShellKind::Fish).as_deref(),
            Some("\x15cd -- '/r/a b'\r")
        );
        assert_eq!(
            cd_command(r"C:\it's", ShellKind::PowerShell).as_deref(),
            Some("\x1bSet-Location -LiteralPath 'C:\\it''s'\r")
        );
        assert_eq!(
            cd_command(r"C:\a b", ShellKind::Cmd).as_deref(),
            Some("\x1bcd /d \"C:\\a b\"\r")
        );
    }

    /// Only the final Enter may be a control character: anything else in
    /// the typed text is a key press inside the line being typed.
    fn printable_but_the_frame(typed: &str, frame: &[char]) -> bool {
        let body = typed.trim_start_matches(frame).trim_end_matches('\r');
        !has_control(body)
    }

    #[test]
    fn control_characters_in_a_folder_are_never_typed() {
        let dir = "/r/x\x15touch pwned #\r\x1b";
        let posix = cd_command(dir, ShellKind::Posix).unwrap();
        assert_eq!(posix, "\x15cd -- $'/r/x\\x15touch pwned #\\x0d\\x1b'\r");
        assert!(printable_but_the_frame(&posix, &['\x15']));
        let fish = cd_command(dir, ShellKind::Fish).unwrap();
        assert_eq!(fish, "\x15cd -- '/r/x'\\x15'touch pwned #'\\x0d\\x1b\r");
        assert!(printable_but_the_frame(&fish, &['\x15']));
        assert_eq!(
            cd_command("/r/it's\x07", ShellKind::Posix).as_deref(),
            Some("\x15cd -- $'/r/it\\'s\\x07'\r")
        );
        // No printable spelling: the caller opens a shell there instead.
        for shell in [ShellKind::Other, ShellKind::PowerShell, ShellKind::Cmd] {
            assert_eq!(cd_command(dir, shell), None, "{shell:?}");
        }
    }

    #[test]
    fn typographic_quotes_cannot_end_a_powershell_string() {
        let typed = cd_command("C:\\a\u{2019};calc;\u{2018}", ShellKind::PowerShell).unwrap();
        assert_eq!(
            typed,
            "\x1bSet-Location -LiteralPath 'C:\\a\u{2019}\u{2019};calc;\u{2018}\u{2018}'\r"
        );
        assert_eq!(
            paste_text(
                &["C:\\$(calc) \u{201b}.txt".to_string()],
                ShellKind::PowerShell
            ),
            "'C:\\$(calc) \u{201b}\u{201b}.txt' "
        );
    }

    #[test]
    fn pasted_paths_are_quoted_for_the_shell() {
        let paths = vec!["/r/plain.rs".to_string(), "/r/with space's.md".to_string()];
        assert_eq!(
            paste_text(&["/r/a b".to_string()], ShellKind::Posix),
            "'/r/a b' "
        );
        assert_eq!(
            paste_text(&paths, ShellKind::Posix),
            r#"/r/plain.rs "/r/with space's.md" "#
        );
        assert_eq!(
            paste_text(&["/r/a\rb".to_string()], ShellKind::Other),
            "$'/r/a\\x0db' "
        );
        assert_eq!(
            paste_text(&["/r/a\rb".to_string()], ShellKind::Fish),
            "'/r/a'\\x0d'b' "
        );
        let paths = vec![r"C:\r\plain.rs".to_string(), r"C:\r\a b.md".to_string()];
        assert_eq!(
            paste_text(&paths, ShellKind::Cmd),
            r#"C:\r\plain.rs "C:\r\a b.md" "#
        );
        assert_eq!(
            paste_text(&paths, ShellKind::PowerShell),
            r"C:\r\plain.rs 'C:\r\a b.md' "
        );
    }
}
