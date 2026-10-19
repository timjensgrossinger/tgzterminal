//! Gate commands, endpoint probes and git worktrees: the daemon's outside-world IO.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tower_proto::{Endpoint, GateResult, GateState, Health};

pub const OUTPUT_TAIL: usize = 4096;

/// Keep the last `max` bytes of `s`, on a char boundary.
pub fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &s[start..])
}

pub struct GateRun {
    pub results: Vec<GateResult>,
    /// Output tail of the first failing command.
    pub failure: Option<String>,
}

/// Run each command with `sh -c` in `dir`, stopping at the first failure.
/// Commands come from tower.toml / .tower.toml only, never from agents.
pub async fn run_gates(dir: &Path, commands: &[String], timeout: Duration) -> GateRun {
    let mut results: Vec<GateResult> = commands
        .iter()
        .map(|c| GateResult {
            name: c.clone(),
            state: GateState::Pending,
            detail: None,
        })
        .collect();
    let mut failure = None;
    for i in 0..commands.len() {
        let started = Instant::now();
        let outcome = run_one(dir, &commands[i], timeout).await;
        let secs = started.elapsed().as_secs_f32();
        match outcome {
            Ok((true, _)) => {
                results[i].state = GateState::Passed;
                results[i].detail = Some(format!("{secs:.1}s"));
            }
            Ok((false, out)) => {
                results[i].state = GateState::Failed;
                results[i].detail = Some(format!("failed after {secs:.1}s"));
                failure = Some(out);
                break;
            }
            Err(err) => {
                results[i].state = GateState::Failed;
                results[i].detail = Some(err.clone());
                failure = Some(err);
                break;
            }
        }
    }
    GateRun { results, failure }
}

async fn run_one(dir: &Path, cmd: &str, timeout: Duration) -> Result<(bool, String), String> {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("/bin/sh", "-c")
    };
    let mut child = Command::new(shell)
        .arg(flag)
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let collect = async {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let _ = tokio::join!(out.read_to_end(&mut a), err.read_to_end(&mut b));
        let status = child.wait().await;
        a.extend_from_slice(&b);
        (status, a)
    };
    match tokio::time::timeout(timeout, collect).await {
        Ok((Ok(status), bytes)) => Ok((
            status.success(),
            tail(&String::from_utf8_lossy(&bytes), OUTPUT_TAIL),
        )),
        Ok((Err(e), _)) => Err(format!("wait failed: {e}")),
        Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

/// Minimal HTTP/1.1 GET for `http://host:port/path` (model servers on
/// localhost or the LAN; no TLS). Returns (status, body, latency).
pub async fn http_get(url: &str, timeout: Duration) -> Result<(u16, String, Duration), String> {
    use tokio::io::AsyncWriteExt;
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// endpoints are supported".to_string())?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };
    let started = Instant::now();
    let work = async {
        let mut s = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| e.to_string())?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        s.take(4 << 20)
            .read_to_end(&mut buf)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(buf)
    };
    let buf = tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| "timed out".to_string())??;
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    let body = if chunked {
        dechunk(body)
    } else {
        body.to_string()
    };
    Ok((status, body, started.elapsed()))
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 || after.len() < size {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    out
}

/// Model names from an Ollama `/api/tags` or OpenAI `/v1/models` body.
pub fn parse_models(kind: &str, body: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let (list, key) = if kind == "ollama" {
        (v.get("models"), "name")
    } else {
        (v.get("data"), "id")
    };
    list.and_then(|l| l.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get(key).and_then(|x| x.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn probe(ep: Endpoint) -> Endpoint {
    let base = ep
        .base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string();
    let url = if ep.kind == "ollama" {
        format!("{base}/api/tags")
    } else {
        format!("{base}/v1/models")
    };
    match http_get(&url, Duration::from_secs(3)).await {
        Ok((200, body, latency)) => Endpoint {
            health: Health::Up,
            latency_ms: Some(latency.as_millis() as u32),
            models: parse_models(&ep.kind, &body),
            ..ep
        },
        _ => Endpoint {
            health: Health::Down,
            latency_ms: None,
            models: vec![],
            ..ep
        },
    }
}

/// Create a worktree for `key` next to the repo containing `dir`:
/// `<repo>/../<repo-name>-tower/<KEY>` on branch `tower/<key>`.
pub async fn create_worktree(dir: &Path, key: &str) -> Result<(PathBuf, String), String> {
    let top = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| format!("git: {e}"))?;
    if !top.status.success() {
        return Err(format!("{} is not inside a git repository", dir.display()));
    }
    let root = PathBuf::from(String::from_utf8_lossy(&top.stdout).trim());
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".into());
    let parent = root.parent().unwrap_or(&root);
    let path = parent.join(format!("{name}-tower")).join(key);
    let branch = format!("tower/{}", key.to_lowercase());
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["worktree", "add", "-b", &branch])
        .arg(&path)
        .output()
        .await
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(tail(String::from_utf8_lossy(&out.stderr).trim(), 400));
    }
    Ok((path, branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end_on_a_char_boundary() {
        assert_eq!(tail("short", 10), "short");
        let t = tail("ääääää", 5);
        assert!(t.starts_with('…'));
        assert!(t.len() <= 5 + '…'.len_utf8() + 1);
    }

    #[test]
    fn models_parse_for_both_apis() {
        let ollama = r#"{"models":[{"name":"qwen3.6:27b"},{"name":"llama3"}]}"#;
        assert_eq!(
            parse_models("ollama", ollama),
            vec!["qwen3.6:27b", "llama3"]
        );
        let openai = r#"{"data":[{"id":"qwen3.8-27b"}]}"#;
        assert_eq!(parse_models("lmstudio", openai), vec!["qwen3.8-27b"]);
        assert!(parse_models("ollama", "nope").is_empty());
    }

    #[test]
    fn chunked_bodies_are_decoded() {
        assert_eq!(dechunk("4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), "Wikipedia");
    }

    #[tokio::test]
    async fn gates_stop_at_the_first_failure() {
        let dir = std::env::temp_dir();
        let run = run_gates(
            &dir,
            &["true".into(), "echo boom >&2; false".into(), "true".into()],
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(run.results[0].state, GateState::Passed);
        assert_eq!(run.results[1].state, GateState::Failed);
        assert_eq!(run.results[2].state, GateState::Pending);
        assert!(run.failure.unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn gates_time_out() {
        let run = run_gates(
            &std::env::temp_dir(),
            &["sleep 5".into()],
            Duration::from_millis(200),
        )
        .await;
        assert_eq!(run.results[0].state, GateState::Failed);
        assert!(run.failure.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn worktrees_land_next_to_the_repo_on_their_own_branch() {
        let base = std::env::temp_dir().join(format!("tower-wt-{}", crate::paths::random_u64()));
        let repo = base.join("proj");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        let (path, branch) = create_worktree(&repo.join("sub"), "TWR-7").await.unwrap();
        let want = base
            .canonicalize()
            .unwrap()
            .join("proj-tower")
            .join("TWR-7");
        assert_eq!(path.canonicalize().unwrap(), want);
        assert_eq!(branch, "tower/twr-7");
        assert!(path.join(".git").exists());
        // Same key again: the branch exists, git refuses, and we say why.
        let err = create_worktree(&repo, "TWR-7").await.unwrap_err();
        assert!(!err.is_empty());
        let outside = create_worktree(&base, "TWR-8").await.unwrap_err();
        assert!(outside.contains("not inside a git repository"));
    }

    #[tokio::test]
    async fn unreachable_endpoints_are_down() {
        let ep = Endpoint {
            id: "x".into(),
            name: "x".into(),
            base_url: "http://127.0.0.1:9".into(),
            kind: "ollama".into(),
            health: Health::Unknown,
            latency_ms: None,
            models: vec![],
        };
        assert_eq!(probe(ep).await.health, Health::Down);
    }
}
