//! The daemon's `PATH`.
//!
//! A GUI app started from the Finder or the Dock inherits launchd's minimal
//! `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), and so does the towerd it
//! spawns. The agent CLIs live elsewhere: Claude Code in `~/.local/bin`,
//! Homebrew's `opencode` and `npx` in `/opt/homebrew/bin`, npm globals under
//! the user's home. With the bare `PATH` every role shows "not installed"
//! and every pane closes the second it opens.
//!
//! `apply` runs once at daemon start, before any thread exists: it asks the
//! login shell for its `PATH`, adds the folders agent CLIs are known to
//! install into, and sets the process `PATH`. Everything that starts a
//! program by name (`on_path`, ACP agents, gate commands, `gh`) then finds
//! it, and pane launches pass the same `PATH` into the pane.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// What `apply` did, for Setup.
#[derive(Clone, Debug, PartialEq)]
pub struct PathReport {
    /// Folders that were not on `PATH` before and exist (for display; the
    /// folders that do not exist yet are on `PATH` too, so a CLI installed
    /// later is found without a restart).
    pub added: Vec<PathBuf>,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// The login shell answered; its `PATH` comes first.
    LoginShell,
    /// The login shell could not be asked (why); only the fallback folders
    /// were added.
    FallbackOnly(String),
    /// `TOWER_NO_LOGIN_PATH` is set: nothing was changed.
    Disabled,
    /// Windows: there is no login shell to ask; only the fallback folders.
    NotApplicable,
}

static REPORT: OnceLock<PathReport> = OnceLock::new();

/// How long the login shell may take. The GUI waits a few seconds for the
/// daemon's socket, so a slow rc file must not eat that.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_millis(1500);

/// Markers around the shell's answer, so rc-file noise is ignored.
const START: &str = "__TOWER_PATH__";
const END: &str = "__TOWER_END__";

/// Fix the process `PATH` once. Call before starting any thread.
pub fn apply() -> &'static PathReport {
    REPORT.get_or_init(|| {
        if std::env::var_os("TOWER_NO_LOGIN_PATH").is_some() {
            return PathReport {
                added: vec![],
                source: Source::Disabled,
            };
        }
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let login = login_shell_path(LOGIN_SHELL_TIMEOUT);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let appdata = std::env::var_os("APPDATA");
        let fallback = fallback_dirs(home.as_deref(), appdata.as_deref(), cfg!(windows));
        let login_os = login.as_ref().ok().map(|s| OsString::from(s.as_str()));
        let (path, added) = enrich(&existing, login_os.as_deref(), &fallback);
        std::env::set_var("PATH", &path);
        PathReport {
            added,
            source: match login {
                Ok(_) => Source::LoginShell,
                Err(_) if cfg!(windows) => Source::NotApplicable,
                Err(why) => Source::FallbackOnly(why),
            },
        }
    })
}

/// What `apply` did, if it ran.
pub fn report() -> Option<&'static PathReport> {
    REPORT.get()
}

/// Folders probed after `PATH` when a program is looked up. Keep in sync
/// with `fallback_command_dirs` in wezterm-gui's `render/sidebar.rs`, which
/// solves the same problem for the sidebar's agent launcher.
pub fn fallback_dirs(home: Option<&Path>, appdata: Option<&OsStr>, windows: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if windows {
        // npm's global bin on Windows, where `npm i -g` puts `claude.cmd`.
        if let Some(appdata) = appdata {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    }
    if let Some(home) = home {
        for rel in [
            ".local/bin",
            "bin",
            ".claude/local",
            ".bun/bin",
            ".cargo/bin",
            ".volta/bin",
            ".npm-global/bin",
            ".yarn/bin",
            ".local/share/mise/shims",
            ".asdf/shims",
        ] {
            dirs.push(home.join(rel));
        }
    }
    if !windows {
        // On Windows these are relative to the current drive's root, which
        // for a process started from a network share is a network lookup.
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        // libc's default when PATH is unset; after `set_var` it no longer
        // applies, so `git`, `sh` and `plutil` must stay reachable.
        for sys in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            dirs.push(PathBuf::from(sys));
        }
    }
    dirs
}

/// `login` first (what the user's terminal sees), then `existing`, then the
/// fallback folders; each folder once. Empty and relative entries are
/// dropped: a `.` on `PATH` would resolve `npx` inside an agent's working
/// folder, which is a repo an agent can write to.
pub fn enrich(
    existing: &OsStr,
    login: Option<&OsStr>,
    fallback: &[PathBuf],
) -> (OsString, Vec<PathBuf>) {
    let before: Vec<PathBuf> = std::env::split_paths(existing).collect();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut push = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if p.as_os_str().is_empty() || !p.is_absolute() {
            return;
        }
        let key = path_key(&p);
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        out.push(p);
    };
    if let Some(login) = login {
        for p in std::env::split_paths(login) {
            push(p, &mut out);
        }
    }
    for p in &before {
        push(p.clone(), &mut out);
    }
    for p in fallback {
        push(p.clone(), &mut out);
    }
    let had: Vec<String> = before.iter().map(|p| path_key(p)).collect();
    let added = out
        .iter()
        .filter(|p| !had.contains(&path_key(p)) && p.is_dir())
        .cloned()
        .collect();
    let joined = std::env::join_paths(&out).unwrap_or_else(|_| existing.to_os_string());
    (joined, added)
}

/// One folder, spelled one way: no trailing separator, case-folded on
/// Windows.
fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        s.to_ascii_lowercase()
    } else {
        s.to_string()
    }
}

/// The text between the markers in the shell's output, if both are there.
pub fn parse_marked(stdout: &str) -> Option<String> {
    let start = stdout.rfind(START)? + START.len();
    let rest = &stdout[start..];
    let end = rest.find(END)?;
    Some(rest[..end].to_string())
}

/// Ask the user's login shell for its `PATH`, giving it `timeout`.
#[cfg(not(windows))]
pub fn login_shell_path(timeout: Duration) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/sh"
            })
        });
    let name = shell
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let script = match name.as_str() {
        "fish" => format!("printf '\\n{START}%s{END}\\n' (string join : $PATH)"),
        "sh" | "bash" | "zsh" | "ksh" | "dash" | "ash" => {
            format!("printf '\\n{START}%s{END}\\n' \"$PATH\"")
        }
        other => return Err(format!("unknown shell {other:?}")),
    };
    let mut cmd = Command::new(&shell);
    cmd.args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so a timeout takes the rc file's helpers
        // (a hung `nvm use`, a `brew shellenv` subshell) down with it.
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", shell.display()))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Read in pieces and stop at the end marker: an rc file may leave a
    // background helper (p10k's gitstatusd, an async worker) holding the
    // pipe open, so EOF can come long after the answer, or never.
    std::thread::spawn(move || {
        let mut acc = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    acc.extend_from_slice(&chunk[..n]);
                    if String::from_utf8_lossy(&acc).contains(END) {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&acc).into_owned());
    });
    let kill_group = |child: &mut std::process::Child| {
        let _ = Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
    };
    let started = Instant::now();
    let mut out: Option<String> = None;
    let exited = loop {
        if out.is_none() {
            if let Ok(s) = rx.try_recv() {
                out = Some(s);
            }
        }
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if out.is_some() => {
                // Answered; whatever the rc file left running is not needed.
                kill_group(&mut child);
                break true;
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                kill_group(&mut child);
                break false;
            }
            Err(e) => {
                kill_group(&mut child);
                return Err(format!("{}: {e}", shell.display()));
            }
        }
    };
    // After exit the reader finishes at once unless a descendant still
    // holds the pipe; do not wait on that for long.
    let out = match out {
        Some(s) => s,
        None => rx
            .recv_timeout(Duration::from_millis(300))
            .unwrap_or_default(),
    };
    match parse_marked(&out) {
        Some(p) if !p.trim().is_empty() => Ok(p.trim().to_string()),
        _ if !exited => Err(format!(
            "{} did not answer within {:.1}s",
            shell.display(),
            timeout.as_secs_f32()
        )),
        _ => Err(format!("{} printed no PATH", shell.display())),
    }
}

#[cfg(windows)]
pub fn login_shell_path(_timeout: Duration) -> Result<String, String> {
    Err("no login shell on Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> OsString {
        OsString::from(x)
    }

    #[test]
    fn login_first_then_existing_then_fallback_each_once() {
        let fallback = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        let (path, _) = enrich(
            &s("/usr/bin:/bin"),
            Some(&s("/Users/x/.local/bin:/opt/homebrew/bin:/usr/bin")),
            &fallback,
        );
        assert_eq!(
            path.to_string_lossy(),
            "/Users/x/.local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/local/bin"
        );
    }

    #[test]
    fn empty_and_relative_entries_are_dropped() {
        let (path, _) = enrich(&s("/usr/bin::.:bin:/bin/"), Some(&s("./x:/bin")), &[]);
        assert_eq!(path.to_string_lossy(), "/bin:/usr/bin");
    }

    #[test]
    fn added_lists_only_new_folders_that_exist() {
        let d = std::env::temp_dir().join(format!("tower-envpath-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        let missing = d.join("not-there");
        let (path, added) = enrich(&s("/usr/bin"), None, &[d.clone(), missing.clone()]);
        assert_eq!(added, vec![d.clone()]);
        // Not yet existing folders are still on PATH, for a later install.
        assert!(path
            .to_string_lossy()
            .contains(missing.to_string_lossy().as_ref()));
        let (_, added) = enrich(&d.as_os_str().to_os_string(), None, &[d.clone()]);
        assert!(added.is_empty(), "already there: not added");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_login_shell_means_fallback_only() {
        let (path, _) = enrich(&s("/usr/bin"), None, &[PathBuf::from("/opt/homebrew/bin")]);
        assert_eq!(path.to_string_lossy(), "/usr/bin:/opt/homebrew/bin");
    }

    #[test]
    fn parse_marked_ignores_banner_noise() {
        let out = format!("Welcome!\nsome rc output\n{START}/a:/b{END}\ntrailing\n");
        assert_eq!(parse_marked(&out).as_deref(), Some("/a:/b"));
        assert_eq!(
            parse_marked(&format!("{START}/a:/b")),
            None,
            "no end marker"
        );
        assert_eq!(parse_marked("nothing"), None);
        // The last pair wins when an rc file echoes the script.
        let out = format!("{START}x{END}\n{START}/real{END}\n");
        assert_eq!(parse_marked(&out).as_deref(), Some("/real"));
    }

    #[test]
    fn fallback_dirs_follow_home_and_platform() {
        let home = Path::new("/Users/x");
        let unix = fallback_dirs(Some(home), None, false);
        assert!(unix.contains(&home.join(".local/bin")));
        assert!(unix.contains(&PathBuf::from("/opt/homebrew/bin")));
        let win = fallback_dirs(
            Some(home),
            Some(OsStr::new("C:\\Users\\x\\AppData\\Roaming")),
            true,
        );
        assert_eq!(
            win[0],
            PathBuf::from("C:\\Users\\x\\AppData\\Roaming").join("npm")
        );
        assert!(!win.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(!win.contains(&PathBuf::from("/usr/bin")));
        let bare = fallback_dirs(None, None, false);
        assert!(bare.contains(&PathBuf::from("/usr/local/bin")));
        // With PATH unset and no login shell, the system folders still come.
        assert!(bare.ends_with(&[
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
            PathBuf::from("/usr/sbin"),
            PathBuf::from("/sbin")
        ]));
    }

    #[cfg(not(windows))]
    #[test]
    fn login_shell_answers_or_fails_within_the_timeout() {
        let started = std::time::Instant::now();
        let r = login_shell_path(Duration::from_millis(1500));
        assert!(started.elapsed() < Duration::from_secs(3), "{r:?}");
        if let Ok(p) = r {
            assert!(p.contains('/'), "{p}");
        }
    }
}
