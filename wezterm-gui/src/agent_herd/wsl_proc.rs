//! Reading a WSL distro's `/proc` from the Windows side.
//!
//! A session file found inside a distro carries a pid from the distro's pid
//! namespace, which means nothing to Windows process APIs. The distro's own
//! `/proc` is reachable through the same `\\wsl.localhost\<distro>\` share the
//! session files were read from, so the process can be asked directly instead
//! of guessed at: whether it still runs (`stat`), and which pane it was started
//! in (`environ`, where `WEZTERM_PANE` arrives through `WSLENV`).
//!
//! Every function here degrades to "no answer" on an unreadable share, so a
//! caller always keeps its older heuristic as the fallback.

use mux::pane::PaneId;
use std::path::{Component, Path, PathBuf};

/// The distro's root (`\\wsl.localhost\Ubuntu\`) for a home found inside it.
fn distro_root(home: &Path) -> Option<PathBuf> {
    let mut components = home.components();
    match components.next()? {
        Component::Prefix(prefix) => {
            let mut root = PathBuf::from(prefix.as_os_str());
            root.push(std::path::MAIN_SEPARATOR_STR);
            Some(root)
        }
        // Not a UNC home (a test fixture, or a non-Windows host): no `/proc`
        // of the distro to read.
        _ => None,
    }
}

fn proc_file(home: &Path, pid: u32, leaf: &str) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    Some(
        distro_root(home)?
            .join("proc")
            .join(pid.to_string())
            .join(leaf),
    )
}

/// Is `pid` running inside the distro?
///
/// `Some(false)` when the process is gone or is a zombie, or when its start
/// time differs from `proc_start` (the pid was reused). `None` when the share
/// could not be asked, so the caller falls back to its own heuristic.
pub fn process_state(home: &Path, pid: u32, proc_start: Option<u64>) -> Option<bool> {
    let path = proc_file(home, pid, "stat")?;
    match std::fs::read_to_string(&path) {
        Ok(stat) => {
            let (state, start) = parse_stat(&stat)?;
            if state == 'Z' || state == 'X' {
                return Some(false);
            }
            Some(proc_start.map_or(true, |expected| expected == start))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(err) => {
            log::debug!("cannot read {}: {err:#}", path.display());
            None
        }
    }
}

/// The pane of *this* GUI process that `pid` was started in, if any.
///
/// Pane ids restart at zero in every GUI process, so the pane id alone could
/// name a pane of another TGZTerminal instance; the socket path, unique per
/// process, is required to match as well.
pub fn pane_of_process(home: &Path, pid: u32) -> Option<PaneId> {
    let ours = std::env::var("WEZTERM_UNIX_SOCKET").ok()?;
    let environ = std::fs::read(proc_file(home, pid, "environ")?).ok()?;
    pane_from_environ(&environ, &ours)
}

/// `(state, starttime)` from a `/proc/<pid>/stat` line.
///
/// The command name is parenthesised and may itself contain spaces and
/// parentheses, so fields are counted from the *last* `)`. `starttime` is
/// field 22, the 20th after the name.
fn parse_stat(stat: &str) -> Option<(char, u64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let start = fields.nth(18)?.parse().ok()?;
    Some((state, start))
}

fn pane_from_environ(environ: &[u8], our_socket: &str) -> Option<PaneId> {
    let mut pane = None;
    let mut socket = None;
    for entry in environ.split(|&b| b == 0) {
        if let Some(value) = entry.strip_prefix(b"WEZTERM_PANE=") {
            pane = std::str::from_utf8(value).ok()?.parse().ok();
        } else if let Some(value) = entry.strip_prefix(b"WEZTERM_UNIX_SOCKET=") {
            socket = Some(value);
        }
    }
    if socket? != our_socket.as_bytes() {
        return None;
    }
    pane
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_fields_are_counted_from_the_last_paren() {
        let stat = "675 (claude) S 306 675 306 34816 675 4194304 292722 108398 2 580 8120 \
                    11465 395 6379 20 0 17 0 19502 5735522304 92299";
        assert_eq!(parse_stat(stat), Some(('S', 19502)));
        let odd = "7 (a) b) Z 1 7 1 0 7 0 0 0 0 0 0 0 0 0 20 0 1 0 42 0 0";
        assert_eq!(parse_stat(odd), Some(('Z', 42)));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn environ_binds_only_to_our_own_socket() {
        let env = b"PATH=/bin\0WEZTERM_PANE=7\0WEZTERM_UNIX_SOCKET=C:\\sock-1\0";
        assert_eq!(pane_from_environ(env, "C:\\sock-1"), Some(7));
        assert_eq!(pane_from_environ(env, "C:\\sock-2"), None);
        assert_eq!(pane_from_environ(b"WEZTERM_PANE=7\0", "C:\\sock-1"), None);
        assert_eq!(
            pane_from_environ(b"WEZTERM_UNIX_SOCKET=C:\\sock-1\0", "C:\\sock-1"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn distro_root_is_the_unc_share() {
        let home = Path::new(r"\\wsl.localhost\Ubuntu\home\hello");
        assert_eq!(
            distro_root(home),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\"))
        );
        assert_eq!(distro_root(Path::new("relative/home")), None);
    }
}
