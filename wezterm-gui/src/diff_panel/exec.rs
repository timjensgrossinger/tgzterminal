//! Running a version-control client and collecting what it prints, with a
//! ceiling on both how much and how long.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Most a client may print before it is cut off. A diff larger than this is
/// not something a side panel can usefully show.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Longest a client may run. Generous: a cold `git status` on a large tree
/// over a slow disk takes seconds.
pub const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// The output hit [`MAX_OUTPUT_BYTES`] or the run hit [`TIMEOUT`], so
    /// `stdout` is only its beginning.
    pub truncated: bool,
}

fn read_capped(mut source: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match source.read(&mut buf) {
            Ok(0) | Err(_) => return (out, false),
            Ok(n) => {
                let room = cap - out.len();
                if n >= room {
                    out.extend_from_slice(&buf[..room]);
                    return (out, true);
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
    }
}

/// Run `program` in `cwd`. `Err` only when it could not be started at all
/// (typically: not installed).
pub fn run(program: &str, args: &[&str], cwd: &Path) -> std::io::Result<CommandOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // An agent is very likely running the same client in the same tree
        // right now; never contend with it for the index lock.
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Parsed output must not depend on the user's locale.
        .env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashing up on every refresh.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let out_reader = std::thread::spawn(move || read_capped(stdout, MAX_OUTPUT_BYTES));
    let err_reader = std::thread::spawn(move || read_capped(stderr, 64 * 1024));

    let deadline = Instant::now() + TIMEOUT;
    let mut killed = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                // A reader that returned early stopped at its cap; the child
                // would block forever writing into the full pipe.
                if Instant::now() >= deadline || out_reader.is_finished() {
                    killed = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    let (stdout, capped) = out_reader.join().unwrap_or_default();
    let (stderr, _) = err_reader.join().unwrap_or_default();
    Ok(CommandOutput {
        success: !killed && status.is_some_and(|status| status.success()),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated: capped || killed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_cut_at_the_cap() {
        let data = vec![b'x'; 1000];
        let (out, capped) = read_capped(&data[..], 100);
        assert_eq!(out.len(), 100);
        assert!(capped);

        let (out, capped) = read_capped(&data[..], 5000);
        assert_eq!(out.len(), 1000);
        assert!(!capped);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(run("tgz-no-such-program-for-tests", &[], tmp.path()).is_err());
    }
}
