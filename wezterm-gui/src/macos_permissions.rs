//! First-run macOS privacy (TCC) priming.
//!
//! macOS gates access to `~/Documents`, `~/Desktop` and `~/Downloads` behind
//! per-app consent prompts that appear the first time the app touches one of
//! those directories. Left to chance, TGZTerminal interrupts the user with a
//! permission dialog at some arbitrary later moment -- typically the first time
//! a pane's cwd lands in `~/Documents` and the sidebar probes it for git state
//! (see `termwindow::render::sidebar`). Touching each directory once on the
//! first launch moves those prompts into initial setup, where the user has the
//! context to answer them.
//!
//! Consent, once given, is recorded by macOS against the bundle's *signing
//! identity*, so it survives app updates as long as the bundle keeps being
//! signed by the same certificate. An ad-hoc signed bundle has no identity and
//! is pinned to the binary's code directory hash instead, which changes on
//! every rebuild and silently revokes every grant. See
//! `ci/macos-signing-cert.sh`.
//!
//! Everything here is best-effort: any failure is logged and swallowed, and a
//! denied prompt is not retried on the next launch.

use config::wezterm_version;
use std::path::{Path, PathBuf};

/// Bumping this re-primes once on the next launch, e.g. if a new directory is
/// added to `PRIMED_DIRS`.
const MARKER_VERSION: u32 = 2;

/// Directories, relative to `$HOME`, that the fork reads on behalf of the user.
const PRIMED_DIRS: &[&str] = &["Documents", "Desktop", "Downloads"];

fn marker_path() -> PathBuf {
    config::DATA_DIR.join("macos-permissions-primed")
}

/// The marker records `<MARKER_VERSION> <build version>`.
///
/// The build version matters because replacing the bundle can lose the grants
/// this marker claims to have collected: for an ad-hoc signed bundle TCC keys
/// consent to the binary's code directory hash, which changes with every
/// build, and switching signing identities has the same effect. Recording the
/// build that primed lets an updated install ask once more instead of
/// silently running without folder access.
///
/// A version-only marker written by an older build parses as "primed by an
/// unknown build", which is stale by definition. Any parse failure also means
/// not primed: re-priming is idempotent and costs one directory read.
fn already_primed(marker: &Path) -> bool {
    let contents = match std::fs::read_to_string(marker) {
        Ok(contents) => contents,
        Err(_) => return false,
    };

    // Split once only: the build version is the rest of the line, and is not
    // guaranteed to be a single whitespace-free token.
    let (version_field, build) = match contents.trim().split_once(char::is_whitespace) {
        Some((version, build)) => (version, build.trim()),
        None => (contents.trim(), ""),
    };

    let version = match version_field.parse::<u32>() {
        Ok(version) => version,
        Err(_) => return false,
    };
    if version < MARKER_VERSION {
        return false;
    }

    build == wezterm_version()
}

fn marker_contents() -> String {
    format!("{} {}\n", MARKER_VERSION, wezterm_version())
}

/// TCC attributes an access to the *responsible* application. When we run
/// straight out of `target/release` the responsible app is whatever terminal
/// launched us, so priming would pester the user with prompts on that app's
/// behalf and record them under its identity. Only prime from inside a bundle.
fn running_from_app_bundle() -> bool {
    std::env::current_exe().map_or(false, |exe| {
        exe.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .map_or(false, |name| name.ends_with(".app"))
        })
    })
}

/// Trigger the folder-access prompts once, in the background.
///
/// Reading a directory blocks until the user answers the prompt, so this must
/// not run on the thread driving the GUI event loop.
pub fn prime_first_run() {
    let marker = marker_path();
    if already_primed(&marker) {
        return;
    }
    if !running_from_app_bundle() {
        log::debug!("not running from an app bundle; skipping permission priming");
        return;
    }

    let spawned = std::thread::Builder::new()
        .name("macos-permission-prime".into())
        .spawn(move || {
            for name in PRIMED_DIRS {
                let dir = config::HOME_DIR.join(name);
                match std::fs::read_dir(&dir) {
                    Ok(mut entries) => {
                        // The prompt fires on the first entry, not on open().
                        let _ = entries.next();
                        log::debug!("permission priming: {} accessible", dir.display());
                    }
                    Err(err) => {
                        // Denied, or the directory simply does not exist.
                        log::debug!("permission priming: {}: {:#}", dir.display(), err);
                    }
                }
            }

            // Written even when a prompt was denied: macOS will not ask again
            // either way, and re-running this on every launch would be pure
            // startup cost.
            if let Err(err) = std::fs::write(&marker, marker_contents()) {
                log::warn!(
                    "unable to record permission priming in {}: {:#}",
                    marker.display(),
                    err
                );
            }
        });

    if let Err(err) = spawned {
        log::warn!("unable to spawn permission priming thread: {:#}", err);
    }
}

#[cfg(test)]
mod tests {
    use super::{already_primed, marker_contents, MARKER_VERSION};
    use std::io::Write;

    fn marker_with(contents: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(contents.as_bytes()).expect("write marker");
        file.flush().expect("flush marker");
        file
    }

    #[test]
    fn marker_written_by_this_build_counts_as_primed() {
        let file = marker_with(&marker_contents());
        assert!(already_primed(file.path()));
    }

    /// The bundle was replaced by an update; grants may not have survived.
    #[test]
    fn marker_from_another_build_is_stale() {
        let file = marker_with(&format!("{} tgz-v1970.01.1\n", MARKER_VERSION));
        assert!(!already_primed(file.path()));
    }

    /// Written by a build from before the marker recorded a version string.
    #[test]
    fn legacy_version_only_marker_is_stale() {
        let file = marker_with("1\n");
        assert!(!already_primed(file.path()));
    }

    #[test]
    fn unreadable_or_malformed_marker_is_not_primed() {
        let file = marker_with("not a version\n");
        assert!(!already_primed(file.path()));

        let missing = std::path::Path::new("/nonexistent/tgzterminal/marker");
        assert!(!already_primed(missing));
    }
}
