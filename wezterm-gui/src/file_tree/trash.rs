//! Moving files to the platform's trash, so a delete from the tree can be
//! taken back from Finder, Explorer or the file manager. Blocking: run it on
//! a worker thread.
//!
//! Nothing here deletes outright. Where no trash can be reached, the answer
//! is an error and the file stays.

use std::path::Path;

/// Move `path` (a file or a whole folder) to the trash.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_err() {
        return Err(format!("{} is gone", path.display()));
    }
    platform(path)
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // <https://github.com/SSheldon/rust-objc/issues/125>
fn platform(path: &Path) -> Result<(), String> {
    use cocoa::base::{id, nil, NO};
    use cocoa::foundation::{NSAutoreleasePool, NSString};
    use objc::{class, msg_send, sel, sel_impl};

    let text = path
        .to_str()
        .ok_or_else(|| "The path is not valid UTF-8".to_string())?;
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let ns_path = NSString::alloc(nil).init_str(text).autorelease();
        let url: id = msg_send![class!(NSURL), fileURLWithPath: ns_path];
        let manager: id = msg_send![class!(NSFileManager), defaultManager];
        let mut error: id = nil;
        let ok: cocoa::base::BOOL =
            msg_send![manager, trashItemAtURL: url resultingItemURL: nil error: &mut error];
        let result = if ok == NO {
            let reason = if error == nil {
                "the Trash refused it".to_string()
            } else {
                let description: id = msg_send![error, localizedDescription];
                let utf8: *const std::os::raw::c_char = msg_send![description, UTF8String];
                if utf8.is_null() {
                    "the Trash refused it".to_string()
                } else {
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned()
                }
            };
            Err(reason)
        } else {
            Ok(())
        };
        pool.drain();
        result
    }
}

#[cfg(windows)]
fn platform(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // The Recycle Bin through .NET's own call, which is what Explorer's
    // Delete does. The path travels in the environment, never through the
    // command line's quoting.
    const SCRIPT: &str = "Add-Type -AssemblyName Microsoft.VisualBasic; \
        $p = $env:TGZ_TRASH_PATH; \
        if (Test-Path -LiteralPath $p -PathType Container) { \
          [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($p, 'OnlyErrorDialogs', 'SendToRecycleBin') \
        } else { \
          [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($p, 'OnlyErrorDialogs', 'SendToRecycleBin') \
        }";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("TGZ_TRASH_PATH", path)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("Could not start PowerShell: {err}"))?;
    if output.status.success() && std::fs::symlink_metadata(path).is_err() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().find(|line| !line.trim().is_empty());
        Err(reason
            .map(str::trim)
            .unwrap_or("the Recycle Bin refused it (network and WSL paths have none)")
            .to_string())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform(path: &Path) -> Result<(), String> {
    // The desktop's own trash: GLib's `gio`, else trash-cli, which also
    // reach the trash of other mounted disks. Without either, the home trash
    // directly, by the same freedesktop.org spec, so nothing needs
    // installing for the common case.
    let mut missing = true;
    for argv in [&["gio", "trash", "--"][..], &["trash-put", "--"][..]] {
        match std::process::Command::new(argv[0])
            .args(&argv[1..])
            .arg(path)
            .output()
        {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                missing = false;
                let stderr = String::from_utf8_lossy(&output.stderr);
                if let Some(line) = stderr.lines().find(|line| !line.trim().is_empty()) {
                    return Err(line.trim().to_string());
                }
            }
            Err(_) => {}
        }
    }
    if !missing {
        return Err("The trash refused it".to_string());
    }
    let trash = home_trash_dir().ok_or_else(|| "No home folder to keep a trash in".to_string())?;
    freedesktop_trash(path, &trash, &chrono::Local::now())
}

/// `$XDG_DATA_HOME/Trash`, by default `~/.local/share/Trash`.
// Used on Linux and the BSDs; built on macOS only to be tested.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn home_trash_dir() -> Option<std::path::PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/share"))
        })?;
    Some(data.join("Trash"))
}

/// Move `path` into the trash at `trash` as the freedesktop.org trash spec
/// lays it out: the item under `files/`, and a `.trashinfo` under `info/`
/// saying where it came from and when, which is what a file manager's
/// Restore reads. The info file is created first and exclusively, which
/// claims the name; a name taken already gets a number. Only for items on
/// the same disk as the trash: anything else is refused, not copied.
// Used on Linux and the BSDs; built on macOS only to be tested.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn freedesktop_trash(
    path: &Path,
    trash: &Path,
    now: &chrono::DateTime<chrono::Local>,
) -> Result<(), String> {
    use std::io::Write;
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [&files, &info] {
        std::fs::create_dir_all(dir).map_err(|err| format!("Could not make the trash: {err}"))?;
    }
    let absolute = std::path::absolute(path).map_err(|err| err.to_string())?;
    let name = absolute
        .file_name()
        .ok_or_else(|| "Nothing to trash".to_string())?
        .to_string_lossy()
        .into_owned();
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode_path(&absolute),
        now.format("%Y-%m-%dT%H:%M:%S")
    );
    for attempt in 1..1000 {
        let stored = if attempt == 1 {
            name.clone()
        } else {
            format!("{name}.{attempt}")
        };
        let info_path = info.join(format!("{stored}.trashinfo"));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("Could not write to the trash: {err}")),
        };
        let written = file.write_all(record.as_bytes());
        let moved = written.map_err(|err| err.to_string()).and_then(|()| {
            std::fs::rename(&absolute, files.join(&stored)).map_err(|err| {
                if err.raw_os_error() == Some(18) {
                    // EXDEV: another disk, whose trash only gio knows.
                    "It is on another disk than the trash".to_string()
                } else {
                    err.to_string()
                }
            })
        });
        if let Err(reason) = moved {
            let _ = std::fs::remove_file(&info_path);
            return Err(reason);
        }
        return Ok(());
    }
    Err("The trash is full of items with this name".to_string())
}

/// A path as the trash spec's `Path=` key wants it: bytes outside the
/// unreserved set percent-encoded, separators kept.
// Used on Linux and the BSDs; built on macOS only to be tested.
#[cfg(all(unix, any(test, not(target_os = "macos"))))]
fn percent_encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts a real (empty) file in the user's trash, so it only runs when
    /// asked for: `cargo test -p wezterm-gui trash -- --ignored`.
    #[test]
    #[ignore]
    fn a_file_goes_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tgzterminal-trash-check.txt");
        std::fs::write(&path, "").unwrap();
        assert_eq!(move_to_trash(&path), Ok(()));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_home_trash_keeps_where_an_item_came_from() {
        use chrono::TimeZone;
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 19, 14, 3, 7)
            .unwrap();
        let first = dir.path().join("a b.txt");
        std::fs::write(&first, "1").unwrap();
        assert_eq!(freedesktop_trash(&first, &trash, &now), Ok(()));
        assert!(!first.exists());
        assert_eq!(
            std::fs::read_to_string(trash.join("files/a b.txt")).unwrap(),
            "1"
        );
        let record = std::fs::read_to_string(trash.join("info/a b.txt.trashinfo")).unwrap();
        assert!(record.starts_with("[Trash Info]\nPath=/"));
        assert!(record.contains("/a%20b.txt\n"));
        assert!(record.ends_with("DeletionDate=2026-10-19T14:03:07\n"));

        // A second item of the same name gets a number, not the first's place.
        std::fs::write(&first, "2").unwrap();
        assert_eq!(freedesktop_trash(&first, &trash, &now), Ok(()));
        assert_eq!(
            std::fs::read_to_string(trash.join("files/a b.txt.2")).unwrap(),
            "2"
        );
        assert!(trash.join("info/a b.txt.2.trashinfo").exists());

        // A folder goes whole.
        let folder = dir.path().join("src");
        std::fs::create_dir_all(folder.join("deep")).unwrap();
        assert_eq!(freedesktop_trash(&folder, &trash, &now), Ok(()));
        assert!(trash.join("files/src/deep").is_dir());
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_trash_call() {
        let dir = tempfile::tempdir().unwrap();
        assert!(move_to_trash(&dir.path().join("gone")).is_err());
    }
}
