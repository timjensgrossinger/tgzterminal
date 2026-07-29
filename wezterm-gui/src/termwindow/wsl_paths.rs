//! Windows <-> WSL path translation for the sidebar launchers.
//!
//! Only one direction actually needs code. `LocalDomain::fixup_command`
//! (`mux/src/domain.rs`) spawns WSL panes as
//! `wsl.exe --distribution <d> --cd <cwd> --exec <argv>`, and `wsl.exe --cd`
//! accepts a Windows path and translates it itself — so going
//! Windows -> WSL we simply pass the Windows path through untouched.
//!
//! The reverse (a WSL pane's Linux cwd handed to a Windows domain) has no
//! such helper, and neither does the project-root marker walk, which has to
//! stat a distro's filesystem from the Windows side. Both are covered here.
//!
//! These are pure string functions with no `cfg` gating so they can be tested
//! on any host, including the macOS dev box where WSL does not exist.

use std::path::PathBuf;

/// UNC prefixes Windows exposes a distro's filesystem under. `wsl.localhost`
/// is the modern form; `wsl$` still works and is what older docs show.
const UNC_PREFIXES: [&str; 2] = [r"\\wsl.localhost\", r"\\wsl$\"];

/// Distro name for a domain, or `None` when the domain is not a WSL domain.
///
/// Prefers a configured `wsl_domains` entry, whose `distribution` may differ
/// from its `name`; falls back to stripping the `WSL:` prefix that
/// `WslDomain::default_domains()` generates.
pub fn distro_for_domain(domain_name: &str, config: &config::ConfigHandle) -> Option<String> {
    for domain in config.wsl_domains() {
        if domain.name == domain_name {
            return Some(
                domain
                    .distribution
                    .clone()
                    .unwrap_or_else(|| domain.name.clone()),
            );
        }
    }
    domain_name
        .strip_prefix("WSL:")
        .map(str::trim)
        .filter(|distro| !distro.is_empty())
        .map(str::to_string)
}

/// True when `domain_name` looks like a WSL domain.
pub fn is_wsl_domain(domain_name: &str, config: &config::ConfigHandle) -> bool {
    distro_for_domain(domain_name, config).is_some()
}

/// A Linux path as seen inside `distro`, rewritten so Windows can open it.
///
/// - `/mnt/c/foo` -> `C:\foo` (a drive mount is a real Windows path)
/// - `/home/tim`  -> `\\wsl.localhost\Ubuntu\home\tim`
///
/// Returns `None` for relative paths, which callers treat as "no usable cwd"
/// and fall back to the target domain's default.
pub fn wsl_to_windows(linux_path: &str, distro: &str) -> Option<PathBuf> {
    let path = linux_path.trim();
    if !path.starts_with('/') || distro.trim().is_empty() {
        return None;
    }

    if let Some(rest) = strip_mnt_drive(path) {
        let (drive, tail) = rest;
        let tail = tail.replace('/', "\\");
        return Some(PathBuf::from(if tail.is_empty() {
            // `/mnt/c` is the drive root, which needs the trailing separator:
            // `C:` alone means "current directory on C:" to Windows.
            format!("{}:\\", drive.to_ascii_uppercase())
        } else {
            format!("{}:\\{}", drive.to_ascii_uppercase(), tail)
        }));
    }

    let tail = path.trim_start_matches('/').replace('/', "\\");
    Some(PathBuf::from(format!(
        r"\\wsl.localhost\{}\{}",
        distro.trim(),
        tail
    )))
}

/// The inverse of [`wsl_to_windows`]: a Windows path rewritten as the distro
/// sees it. Used to map a UNC marker-walk result back into the distro's view.
///
/// - `\\wsl.localhost\Ubuntu\home\tim` -> `/home/tim` (also the `\\wsl$\` form)
/// - `C:\foo` -> `/mnt/c/foo`
///
/// A UNC path naming a *different* distro yields `None`: that directory is not
/// reachable under the target distro's own root.
pub fn windows_to_wsl(win_path: &str, distro: &str) -> Option<String> {
    let path = win_path.trim();
    if path.is_empty() {
        return None;
    }

    for prefix in UNC_PREFIXES {
        // Windows path comparison is case-insensitive, and so are distro names
        // as far as the UNC share is concerned.
        if path.len() >= prefix.len() && path[..prefix.len()].eq_ignore_ascii_case(prefix) {
            let rest = &path[prefix.len()..];
            let (unc_distro, tail) = match rest.find(['\\', '/']) {
                Some(idx) => (&rest[..idx], &rest[idx + 1..]),
                None => (rest, ""),
            };
            if !unc_distro.eq_ignore_ascii_case(distro.trim()) {
                return None;
            }
            return Some(format!("/{}", tail.replace('\\', "/")));
        }
    }

    // Drive-letter path: C:\foo or C:/foo, and bare `C:` meaning the root.
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let tail = path[2..].trim_start_matches(['\\', '/']).replace('\\', "/");
        return Some(if tail.is_empty() {
            format!("/mnt/{}", drive)
        } else {
            format!("/mnt/{}/{}", drive, tail)
        });
    }

    None
}

/// Split `/mnt/<drive>[/tail]` into the drive letter and the remaining path.
/// Only single-letter mounts count: `/mnt/data` is an ordinary directory.
fn strip_mnt_drive(path: &str) -> Option<(char, &str)> {
    let rest = path.strip_prefix("/mnt/")?;
    let (drive, tail) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };
    let mut chars = drive.chars();
    let letter = chars.next()?;
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some((letter, tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnt_drive_becomes_a_windows_drive_path() {
        assert_eq!(
            wsl_to_windows("/mnt/c/Users/tim/proj", "Ubuntu"),
            Some(PathBuf::from(r"C:\Users\tim\proj"))
        );
        // Lowercase drive letters are uppercased, matching Windows convention.
        assert_eq!(
            wsl_to_windows("/mnt/d/data", "Ubuntu"),
            Some(PathBuf::from(r"D:\data"))
        );
    }

    #[test]
    fn mnt_drive_root_keeps_its_trailing_separator() {
        // `C:` without a separator means "current dir on C:", not the root.
        assert_eq!(
            wsl_to_windows("/mnt/c", "Ubuntu"),
            Some(PathBuf::from(r"C:\"))
        );
        assert_eq!(
            wsl_to_windows("/mnt/c/", "Ubuntu"),
            Some(PathBuf::from(r"C:\"))
        );
    }

    #[test]
    fn multi_letter_mnt_entry_is_not_a_drive() {
        // /mnt/data is a normal directory inside the distro, not a mount.
        assert_eq!(
            wsl_to_windows("/mnt/data/x", "Ubuntu"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu\mnt\data\x"))
        );
    }

    #[test]
    fn distro_internal_path_becomes_a_unc_path() {
        assert_eq!(
            wsl_to_windows("/home/tim/proj", "Ubuntu-22.04"),
            Some(PathBuf::from(r"\\wsl.localhost\Ubuntu-22.04\home\tim\proj"))
        );
    }

    #[test]
    fn relative_paths_and_blank_distros_do_not_translate() {
        assert_eq!(wsl_to_windows("relative/path", "Ubuntu"), None);
        assert_eq!(wsl_to_windows("", "Ubuntu"), None);
        assert_eq!(wsl_to_windows("/home/tim", "  "), None);
    }

    #[test]
    fn unc_path_maps_back_to_a_linux_path() {
        assert_eq!(
            windows_to_wsl(r"\\wsl.localhost\Ubuntu\home\tim", "Ubuntu"),
            Some("/home/tim".to_string())
        );
        // The older \\wsl$\ share form is still in wide use.
        assert_eq!(
            windows_to_wsl(r"\\wsl$\Ubuntu\home\tim", "Ubuntu"),
            Some("/home/tim".to_string())
        );
        // Share and distro names compare case-insensitively on Windows.
        assert_eq!(
            windows_to_wsl(r"\\WSL.LOCALHOST\ubuntu\home", "Ubuntu"),
            Some("/home".to_string())
        );
    }

    #[test]
    fn unc_path_for_another_distro_is_rejected() {
        // Debian's /home is not reachable from inside Ubuntu.
        assert_eq!(
            windows_to_wsl(r"\\wsl.localhost\Debian\home\tim", "Ubuntu"),
            None
        );
    }

    #[test]
    fn drive_path_maps_to_mnt() {
        assert_eq!(
            windows_to_wsl(r"C:\Users\tim", "Ubuntu"),
            Some("/mnt/c/Users/tim".to_string())
        );
        // Forward slashes appear when the path came from a file:// URL.
        assert_eq!(
            windows_to_wsl("C:/Users/tim", "Ubuntu"),
            Some("/mnt/c/Users/tim".to_string())
        );
        assert_eq!(windows_to_wsl(r"C:\", "Ubuntu"), Some("/mnt/c".to_string()));
        assert_eq!(windows_to_wsl("C:", "Ubuntu"), Some("/mnt/c".to_string()));
    }

    #[test]
    fn unmappable_windows_paths_return_none() {
        assert_eq!(windows_to_wsl(r"\\server\share\x", "Ubuntu"), None);
        assert_eq!(windows_to_wsl("relative\\path", "Ubuntu"), None);
        assert_eq!(windows_to_wsl("", "Ubuntu"), None);
    }

    #[test]
    fn drive_paths_round_trip_both_ways() {
        let linux = "/mnt/c/Users/tim/proj";
        let win = wsl_to_windows(linux, "Ubuntu").unwrap();
        assert_eq!(
            windows_to_wsl(&win.to_string_lossy(), "Ubuntu"),
            Some(linux.to_string())
        );
    }

    #[test]
    fn distro_paths_round_trip_both_ways() {
        let linux = "/home/tim/proj";
        let win = wsl_to_windows(linux, "Ubuntu").unwrap();
        assert_eq!(
            windows_to_wsl(&win.to_string_lossy(), "Ubuntu"),
            Some(linux.to_string())
        );
    }
}
