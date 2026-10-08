//! Running a version-control client and collecting what it prints, with a
//! ceiling on both how much and how long.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Most a client may print before it is cut off. A diff larger than this is
/// not something a side panel can usefully show.
/// Paths per client run: a long list is split so a command line stays well
/// inside Windows' 32K limit.
pub const PATHS_PER_RUN: usize = 200;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Longest a client may run. Generous: a cold `git status` on a large tree
/// over a slow disk takes seconds.
pub const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// The output hit [`MAX_OUTPUT_BYTES`] or the run hit [`TIMEOUT`], so
    /// `stdout` is only its beginning.
    pub truncated: bool,
    /// The run hit [`TIMEOUT`] and was stopped.
    pub timed_out: bool,
}

impl CommandOutput {
    /// What to tell the reader when the client was stopped for taking too
    /// long: an empty answer from it is not "nothing changed".
    pub fn timeout_reason(&self, program: &str) -> Option<String> {
        self.timed_out.then(|| {
            format!(
                "{program} did not finish within {} seconds",
                TIMEOUT.as_secs()
            )
        })
    }
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

/// Where a version-control client runs.
///
/// A working copy does not say which installation manages it, and on Windows
/// there can be two that disagree: the Windows client and the one inside a
/// WSL distro apply different line-ending and file-mode rules to the very
/// same files. Where the files live is the best clue. A client reaching
/// across the Windows/WSL boundary is also many times slower: every file it
/// has to read goes through the other side's filesystem bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Env {
    /// The machine this process runs on.
    Host,
    /// Inside a WSL distro, as `user` (the distro's default when `None`).
    Wsl {
        distro: String,
        user: Option<String>,
    },
}

impl Env {
    pub fn label(&self) -> &str {
        match self {
            Env::Host if cfg!(windows) => "Windows",
            Env::Host => "this machine",
            Env::Wsl { distro, .. } => distro,
        }
    }
}

/// The environments a client may run in, the working copy's own first.
///
/// The first environment that has a given client keeps it for the rest of
/// the scan, so one scan never mixes two installations' answers.
#[derive(Debug)]
pub struct Runner {
    envs: Vec<Env>,
    chosen: std::cell::Cell<Option<usize>>,
}

impl Runner {
    pub fn new(envs: Vec<Env>) -> Self {
        Self {
            envs,
            chosen: std::cell::Cell::new(None),
        }
    }

    /// A runner over the same environments that has not chosen one yet, for
    /// a scan of another working copy.
    pub fn fork(&self) -> Self {
        Self::new(self.envs.clone())
    }

    #[cfg(test)]
    pub fn host() -> Self {
        Self::new(vec![Env::Host])
    }

    /// Run `program` in `cwd`. `Err` only when no environment could start it
    /// (typically: not installed anywhere).
    pub fn run(&self, program: &str, args: &[&str], cwd: &Path) -> std::io::Result<CommandOutput> {
        if let Some(env) = self.chosen.get().and_then(|idx| self.envs.get(idx)) {
            return run_in(env, program, args, cwd);
        }
        let mut missing = None;
        for (idx, env) in self.envs.iter().enumerate() {
            match run_in(env, program, args, cwd) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => missing = Some(err),
                other => {
                    self.chosen.set(Some(idx));
                    return other;
                }
            }
        }
        Err(missing.unwrap_or_else(|| std::io::ErrorKind::NotFound.into()))
    }

    /// Worth telling the reader when the client that answered is not the
    /// working copy's own: its idea of what changed can differ.
    pub fn fallback_note(&self, program: &str) -> Option<String> {
        let idx = self.chosen.get().filter(|idx| *idx > 0)?;
        Some(format!(
            "{program} is not installed in {}; using the one in {}",
            self.envs.first()?.label(),
            self.envs.get(idx)?.label()
        ))
    }
}

/// `cd` into `$1`, then run the rest. 126 and 127 are the shell's own codes
/// for "cannot" and "not found"; neither Git nor Subversion exits with them.
const WSL_SCRIPT: &str = r#"cd -- "$1" 2>/dev/null || exit 126; shift; command -v "$1" >/dev/null 2>&1 || exit 127; GIT_OPTIONAL_LOCKS=0 LC_ALL=C exec "$@""#;

fn run_in(env: &Env, program: &str, args: &[&str], cwd: &Path) -> std::io::Result<CommandOutput> {
    let (distro, user) = match env {
        Env::Host => return run(program, args, cwd),
        Env::Wsl { distro, user } => (distro, user),
    };
    // The directory as the distro sees it. `wsl.exe --cd` is not used: the
    // script reports an unreachable directory instead of hanging on it.
    let linux_cwd = crate::termwindow::wsl_paths::windows_to_wsl(&cwd.to_string_lossy(), distro)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is not reachable from {distro}", cwd.display()),
            )
        })?;
    let mut command = config::hidden_wsl_command();
    command.args(["--distribution", distro.as_str()]);
    if let Some(user) = user {
        command.args(["--user", user.as_str()]);
    }
    command
        .args([
            "--exec",
            "sh",
            "-c",
            WSL_SCRIPT,
            "sh",
            linux_cwd.as_str(),
            program,
        ])
        .args(args);
    let (output, code) = collect(command)?;
    match code {
        Some(127) => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{program} is not installed in {distro}"),
        )),
        Some(126) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{linux_cwd} is not reachable in {distro}"),
        )),
        _ => Ok(output),
    }
}

/// Run `program` in `cwd` on this machine. `Err` only when it could not be
/// started at all (typically: not installed).
fn run(program: &str, args: &[&str], cwd: &Path) -> std::io::Result<CommandOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
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
    collect(command).map(|(output, _)| output)
}

/// Run `command` to its end, or to the output or time ceiling, and return
/// what it printed with its exit code.
fn collect(mut command: Command) -> std::io::Result<(CommandOutput, Option<i32>)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    // Set once the output has hit its cap. The reader finishing is not that
    // signal: it also finishes when the child closes its end on the way out,
    // a moment before its exit can be collected, and killing it then turned
    // a complete answer into a failed one.
    let over_cap = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out_reader = std::thread::spawn({
        let over_cap = std::sync::Arc::clone(&over_cap);
        move || {
            let read = read_capped(stdout, MAX_OUTPUT_BYTES);
            over_cap.store(read.1, std::sync::atomic::Ordering::Relaxed);
            read
        }
    });
    let err_reader = std::thread::spawn(move || read_capped(stderr, 64 * 1024));

    let deadline = Instant::now() + TIMEOUT;
    let mut killed = false;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                // A reader that stopped at its cap no longer drains the pipe;
                // the child would block forever writing into it.
                let late = Instant::now() >= deadline;
                if late || over_cap.load(std::sync::atomic::Ordering::Relaxed) {
                    timed_out = late;
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
    let code = if killed {
        None
    } else {
        status.and_then(|status| status.code())
    };
    Ok((
        CommandOutput {
            success: !killed && status.is_some_and(|status| status.success()),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            truncated: capped || killed,
            timed_out,
        },
        code,
    ))
}

/// Run `run` once with no paths (`paths` empty) or once per chunk of
/// `paths`, and join what comes back as if it were one run.
pub fn run_chunked(
    paths: &[String],
    mut run: impl FnMut(&[String]) -> std::io::Result<CommandOutput>,
) -> std::io::Result<CommandOutput> {
    if paths.is_empty() {
        return run(&[]);
    }
    let mut joined: Option<CommandOutput> = None;
    for chunk in paths.chunks(PATHS_PER_RUN) {
        let out = run(chunk)?;
        joined = Some(match joined {
            None => out,
            Some(mut acc) => {
                acc.stdout.push_str(&out.stdout);
                acc.stderr.push_str(&out.stderr);
                acc.success &= out.success;
                acc.truncated |= out.truncated;
                acc.timed_out |= out.timed_out;
                acc
            }
        });
    }
    Ok(joined.unwrap_or_default())
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
    fn a_runner_reports_a_client_no_environment_has() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = Runner::host();
        let err = runner
            .run("tgz-no-such-program-for-tests", &[], tmp.path())
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        // Nothing answered, so there is no fallback to mention.
        assert_eq!(runner.fallback_note("git"), None);
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(run("tgz-no-such-program-for-tests", &[], tmp.path()).is_err());
    }
}
