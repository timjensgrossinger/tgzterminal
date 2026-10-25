//! Before the sidebar starts an agent CLI: is its folder local-only?
//!
//! The answer comes from the `tower` binary (`tower guard dir`), so the GUI
//! and towerd read `[privacy]` and the repo's `.tower.toml` the same way.
//! It runs once per launch click, never while painting.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `tower guard dir`'s exit code for a local-only folder.
const LOCAL_ONLY_EXIT: i32 = 3;

/// How long a launch click may wait for the answer.
const DEADLINE: Duration = Duration::from_millis(1500);

/// Why the sidebar must not start `cli` (the launcher's adapter id, e.g.
/// `claude`) in `cwd`, or `None`. No Tower binary: no answer, no refusal.
/// No answer in time (a hung network folder): refused, since Tower could
/// not rule it out.
pub fn launch_refusal(cli: &str, cwd: Option<&Path>) -> Option<String> {
    let dir = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(Into::into))?;
    let bin = super::client::daemon_binary()?;
    let mut child = Command::new(bin)
        .arg("guard")
        .arg("dir")
        .arg(&dir)
        .arg(cli)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < DEADLINE => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(format!(
                    "{} was not started: Tower could not check in time whether this folder is local-only.",
                    cli
                ));
            }
        }
    };
    let mut why = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut why);
    }
    refusal_text(cli, status.code(), &why)
}

fn refusal_text(cli: &str, code: Option<i32>, why: &str) -> Option<String> {
    (code == Some(LOCAL_ONLY_EXIT)).then(|| {
        format!(
            "{} was not started: this folder is local-only ({}). Use Tower's Local role here, or allow it with [privacy] allow_clis.",
            cli,
            why.trim()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::refusal_text;

    #[test]
    fn only_the_local_only_exit_code_refuses() {
        let t = refusal_text("claude", Some(3), "~/src/payroll is a local-only folder\n").unwrap();
        assert!(t.starts_with("claude was not started"));
        assert!(t.contains("(~/src/payroll is a local-only folder)"));
        assert_eq!(refusal_text("claude", Some(0), ""), None);
        assert_eq!(
            refusal_text("claude", None, ""),
            None,
            "killed or missing: no refusal"
        );
    }
}
