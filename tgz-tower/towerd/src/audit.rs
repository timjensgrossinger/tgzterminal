//! `audit.jsonl`: one line per thing Tower did or was asked to do outside
//! itself (an issue comment, a transition, a PR, an agent start), whether it
//! ran on its own or after a click. Append-only, 0600, rotated at a size
//! cap so it never grows without bound.

use crate::policy::{Action, Decision, Level};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Rotate to `audit.jsonl.1` past this size; one old file is kept.
const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Serialize, Debug, PartialEq)]
pub struct Entry<'a> {
    pub ts: u64,
    pub action: &'static str,
    pub level: Level,
    pub decision: Decision,
    /// Whether this happened while armed.
    pub armed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flight: Option<&'a str>,
    /// What it touched: `ACME-12`, `owner/repo#4`, an agent id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'a str>,
    /// `asked`, `drafted`, `done`, `failed: …`, `skipped`.
    pub outcome: &'a str,
    /// Why the level was what it was, when not the plain policy:
    /// `hands_free` for a hands-free flight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'a str>,
}

impl<'a> Entry<'a> {
    pub fn new(action: Action, level: Level, decision: Decision, outcome: &'a str) -> Self {
        Self {
            ts: crate::paths::now_unix(),
            action: action.key(),
            level,
            decision,
            armed: false,
            flight: None,
            target: None,
            outcome,
            via: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("audit.jsonl"),
        }
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry. Errors are returned for logging; callers must not
    /// let a full disk stop the action itself.
    pub fn append(&self, entry: &Entry) -> std::io::Result<()> {
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > MAX_BYTES) {
            let mut old = self.path.as_os_str().to_owned();
            old.push(".1");
            std::fs::rename(&self.path, PathBuf::from(old))?;
        }
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        // One write call per line, so concurrent appends never interleave.
        opts.open(&self.path)?.write_all(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tower-audit-{}", crate::paths::random_u64()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn appends_one_json_line_per_entry() {
        let d = tmp();
        let audit = Audit::new(&d);
        let mut e = Entry::new(Action::AckPost, Level::Auto, Decision::Run, "done");
        e.armed = true;
        e.target = Some("ACME-12");
        audit.append(&e).unwrap();
        audit
            .append(&Entry::new(
                Action::Land,
                Level::Confirm,
                Decision::Ask,
                "asked",
            ))
            .unwrap();
        let text = std::fs::read_to_string(audit.path()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["action"], "ack.post");
        assert_eq!(lines[0]["level"], "auto");
        assert_eq!(lines[0]["decision"], "run");
        assert_eq!(lines[0]["armed"], true);
        assert_eq!(lines[0]["target"], "ACME-12");
        assert!(lines[1].get("target").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(audit.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn rotates_past_the_size_cap() {
        let d = tmp();
        let audit = Audit::new(&d);
        std::fs::write(audit.path(), vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        audit
            .append(&Entry::new(
                Action::Land,
                Level::Auto,
                Decision::Run,
                "done",
            ))
            .unwrap();
        assert!(d.join("audit.jsonl.1").exists());
        let fresh = std::fs::read_to_string(audit.path()).unwrap();
        assert_eq!(fresh.lines().count(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
