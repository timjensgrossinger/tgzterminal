//! Which agent sessions were open, so they can be reopened.
//!
//! The launcher's "Resume session" list is built by scanning vendor transcript
//! stores: it knows every session that ever existed under `$HOME`, but nothing
//! about which ones you actually had open. This module records that, per window,
//! so a window's agents can be brought back after the app goes away — including
//! when it goes away unexpectedly, which is the case the feature exists for.
//!
//! Deliberately a separate file from `tgz_ui_state`: this one is written whenever
//! a window's agent set changes, from every window, while racing a crash, so it
//! carries a version, a pruning policy, an atomic write and a write lock. Those
//! are the wrong semantics to graft onto a file that stores UI toggles.
//!
//! Everything here is best-effort. A missing, stale, unreadable or corrupt file
//! means "nothing to offer" — never an error the user has to deal with.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Bumped when the meaning of an existing field changes. An unknown version is
/// treated as "no snapshot" rather than migrated: the payload is disposable, and
/// guessing at an older shape risks resuming the wrong thing.
const SNAPSHOT_VERSION: u32 = 1;

/// Windows retained in the file. Older entries beyond this are pruned on write.
const MAX_SNAPSHOT_WINDOWS: usize = 8;

/// Sessions recorded per window. This is a continuity aid, not a session
/// archive.
const MAX_SNAPSHOT_SESSIONS: usize = 25;

/// Snapshots older than this are not offered. Reopening the agents from a window
/// closed last month is noise, not continuity.
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One agent session that was open, in the form the resume path consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSession {
    /// Adapter id, e.g. `"claude"`. Matches the keys of `agent_ui.adapters`.
    pub adapter_id: String,
    /// The vendor's session id.
    ///
    /// Untrusted on read: this reaches argv, so the restore path re-checks it
    /// against the same charset gate the transcript scan applies.
    pub session_id: String,
    /// Where the agent was running; the resume command runs here.
    pub cwd: PathBuf,
    /// Display name at capture time. Never load-bearing — logs only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The agent sessions one window had open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    /// `<run_id>:<mux_window_id>` — unique per window per process run.
    pub key: String,
    /// The process run that wrote this. Entries from the current run are never
    /// offered back to it; that would restore a window into itself.
    pub run_id: String,
    /// Epoch millis of the last update, and the "which window was last" sort
    /// key. Millis rather than `SystemTime` so the file stays readable and
    /// independent of serde's `SystemTime` shape.
    pub updated_at_ms: u64,
    /// Set when the window went away through its close handler rather than with
    /// the process. Diagnostics only: a cleanly closed window's sessions are
    /// still offered, the way a browser keeps "recently closed".
    #[serde(default)]
    pub closed_cleanly: bool,
    /// In capture order, i.e. tab order, so restored tabs come back in place.
    pub sessions: Vec<SnapshotSession>,
}

/// On-disk shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LastSessionFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    windows: Vec<WindowSnapshot>,
}

fn state_path() -> PathBuf {
    config::DATA_DIR.join("tgz-last-session.json")
}

/// Serializes the read-modify-write cycle between windows of this process.
///
/// Cross-process races are not covered: two TGZTerminal processes can still drop
/// each other's entries. The atomic rename bounds that to losing whole entries
/// rather than corrupting the file.
fn write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn epoch_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Identifies this process run.
///
/// Pid alone is not enough: pids are recycled, and a recycled pid would make a
/// previous run's snapshot look like our own — so we would refuse to offer it.
fn run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID.get_or_init(|| format!("{}-{}", std::process::id(), epoch_millis_now()))
}

/// Snapshot key for a window of this run.
pub fn window_key(mux_window_id: usize) -> String {
    format!("{}:{}", run_id(), mux_window_id)
}

fn read_file_at(path: &Path) -> LastSessionFile {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|err| {
            log::warn!("failed to parse {}: {err:#}", path.display());
            LastSessionFile::default()
        }),
        // Missing file is the common first-run case; not worth logging.
        Err(_) => LastSessionFile::default(),
    }
}

/// Write via a temp file and rename, so a crash mid-write cannot leave a
/// half-written file where a readable one used to be.
fn write_file_at(path: &Path, file: &LastSessionFile) {
    let json = match serde_json::to_string_pretty(file) {
        Ok(json) => json,
        Err(err) => {
            log::warn!("failed to serialize last-session snapshot: {err:#}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::warn!("failed to create {}: {err:#}", parent.display());
            return;
        }
    }
    let temp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&temp, json) {
        log::warn!("failed to write {}: {err:#}", temp.display());
        return;
    }
    // The file lists project paths and session ids, so keep it to its owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)) {
            log::warn!("failed to chmod {}: {err:#}", temp.display());
        }
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        log::warn!("failed to rename {}: {err:#}", temp.display());
        let _ = std::fs::remove_file(&temp);
    }
}

/// Drop entries that cannot be resumed, collapse repeats, and cap the list.
///
/// Applied on both write and read: on write so the file stays bounded, on read
/// because the file is untrusted input like anything else under `$HOME`.
fn sanitize_sessions(sessions: Vec<SnapshotSession>) -> Vec<SnapshotSession> {
    let mut seen = std::collections::HashSet::new();
    sessions
        .into_iter()
        .filter(|session| {
            !session.adapter_id.is_empty()
                && crate::agent_herd::sessions::session_id_is_sane(&session.session_id)
        })
        .filter(|session| seen.insert((session.adapter_id.clone(), session.session_id.clone())))
        .take(MAX_SNAPSHOT_SESSIONS)
        .collect()
}

/// Insert or replace one window's entry, leaving other windows alone, and prune
/// the oldest entries past the window cap.
fn upsert_window(file: &mut LastSessionFile, snapshot: WindowSnapshot) {
    file.version = SNAPSHOT_VERSION;
    match file.windows.iter_mut().find(|w| w.key == snapshot.key) {
        Some(existing) => *existing = snapshot,
        None => file.windows.push(snapshot),
    }
    if file.windows.len() > MAX_SNAPSHOT_WINDOWS {
        // Newest first, then keep the head: the oldest lose.
        file.windows.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then(a.key.cmp(&b.key))
        });
        file.windows.truncate(MAX_SNAPSHOT_WINDOWS);
    }
}

/// The window a restore should reopen.
///
/// "Last" is last-*updated*, not last-*closed*: an unexpected quit never records
/// a close, so close order is unavailable in exactly the case that matters.
/// Entries from the current run are skipped, as are empty ones (a window with no
/// agents is not a candidate) and anything past [`SNAPSHOT_MAX_AGE`].
fn pick_last_window<'a>(
    file: &'a LastSessionFile,
    current_run_id: &str,
    now_ms: u64,
) -> Option<&'a WindowSnapshot> {
    if file.version != SNAPSHOT_VERSION {
        return None;
    }
    let max_age_ms = SNAPSHOT_MAX_AGE.as_millis() as u64;
    file.windows
        .iter()
        .filter(|w| w.run_id != current_run_id)
        .filter(|w| !w.sessions.is_empty())
        .filter(|w| now_ms.saturating_sub(w.updated_at_ms) <= max_age_ms)
        // Newest wins; the key breaks ties so the answer is deterministic when
        // two windows were touched in the same millisecond.
        .max_by(|a, b| {
            a.updated_at_ms
                .cmp(&b.updated_at_ms)
                .then(b.key.cmp(&a.key))
        })
}

/// Record one window's agent sessions. Best-effort; safe to call from a worker
/// thread.
pub fn record_window_sessions(key: String, sessions: Vec<SnapshotSession>, closed_cleanly: bool) {
    let sessions = sanitize_sessions(sessions);
    let path = state_path();
    let _guard = write_lock().lock();
    let mut file = read_file_at(&path);
    upsert_window(
        &mut file,
        WindowSnapshot {
            key: key.clone(),
            run_id: run_id().to_string(),
            updated_at_ms: epoch_millis_now(),
            closed_cleanly,
            sessions,
        },
    );
    write_file_at(&path, &file);
}

/// Sessions from the last window of a previous run, or `None` when there is
/// nothing to offer.
///
/// Reads the filesystem, so this is called once at window creation and never
/// from paint.
pub fn load_last_window() -> Option<Vec<SnapshotSession>> {
    let path = state_path();
    let file = read_file_at(&path);
    let snapshot = pick_last_window(&file, run_id(), epoch_millis_now())?;
    let sessions = sanitize_sessions(snapshot.sessions.clone());
    (!sessions.is_empty()).then_some(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(adapter: &str, id: &str, cwd: &str) -> SnapshotSession {
        SnapshotSession {
            adapter_id: adapter.to_string(),
            session_id: id.to_string(),
            cwd: PathBuf::from(cwd),
            label: Some(format!("{adapter} · {id}")),
        }
    }

    fn window(key: &str, run: &str, updated_at_ms: u64, count: usize) -> WindowSnapshot {
        WindowSnapshot {
            key: key.to_string(),
            run_id: run.to_string(),
            updated_at_ms,
            closed_cleanly: false,
            sessions: (0..count)
                .map(|i| session("claude", &format!("session-{i}"), "/repo"))
                .collect(),
        }
    }

    fn file_with(windows: Vec<WindowSnapshot>) -> LastSessionFile {
        LastSessionFile {
            version: SNAPSHOT_VERSION,
            windows,
        }
    }

    #[test]
    fn snapshot_lists_every_field() {
        // Every field named explicitly, so adding one forces this test to be
        // updated rather than silently going unpersisted.
        let snapshot = WindowSnapshot {
            key: "run-1:7".to_string(),
            run_id: "run-1".to_string(),
            updated_at_ms: 1_700_000_000_000,
            closed_cleanly: true,
            sessions: vec![SnapshotSession {
                adapter_id: "claude".to_string(),
                session_id: "abc-123".to_string(),
                cwd: PathBuf::from("/repo/here"),
                label: Some("claude · abc".to_string()),
            }],
        };
        let json = serde_json::to_string(&file_with(vec![snapshot.clone()])).unwrap();
        let parsed: LastSessionFile = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.version, SNAPSHOT_VERSION);
        assert_eq!(parsed.windows, vec![snapshot]);
    }

    #[test]
    fn unknown_version_is_treated_as_no_snapshot() {
        let file = LastSessionFile {
            version: SNAPSHOT_VERSION + 98,
            windows: vec![window("old:1", "old-run", 1_000, 2)],
        };
        assert!(pick_last_window(&file, "this-run", 2_000).is_none());
    }

    #[test]
    fn missing_version_field_is_treated_as_no_snapshot() {
        let parsed: LastSessionFile =
            serde_json::from_str(r#"{"windows":[]}"#).expect("windows-only file parses");
        assert_eq!(parsed.version, 0);
        assert!(pick_last_window(&parsed, "this-run", 2_000).is_none());
    }

    #[test]
    fn corrupt_json_falls_back_to_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tgz-last-session.json");
        std::fs::write(&path, "not json at all").unwrap();
        let file = read_file_at(&path);
        assert_eq!(file.version, 0);
        assert!(file.windows.is_empty());
    }

    #[test]
    fn absent_closed_cleanly_defaults_to_false() {
        let parsed: WindowSnapshot =
            serde_json::from_str(r#"{"key":"k","run_id":"r","updated_at_ms":1,"sessions":[]}"#)
                .unwrap();
        assert!(!parsed.closed_cleanly);
    }

    #[test]
    fn a_written_snapshot_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tgz-last-session.json");
        let mut file = LastSessionFile::default();
        upsert_window(&mut file, window("run-1:3", "run-1", 500, 2));
        write_file_at(&path, &file);

        let read_back = read_file_at(&path);
        assert_eq!(read_back.version, SNAPSHOT_VERSION);
        assert_eq!(read_back.windows.len(), 1);
        assert_eq!(read_back.windows[0].sessions.len(), 2);
        // The temp file must not be left behind.
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn pick_last_window_ignores_the_current_run() {
        // Otherwise a window would offer to restore itself.
        let file = file_with(vec![window("this-run:1", "this-run", 9_000, 3)]);
        assert!(pick_last_window(&file, "this-run", 9_500).is_none());
    }

    #[test]
    fn pick_last_window_takes_the_newest_previous_run_entry() {
        let file = file_with(vec![
            window("old:1", "run-a", 1_000, 1),
            window("newer:1", "run-b", 5_000, 2),
            window("mine:1", "this-run", 9_000, 4),
        ]);
        let picked = pick_last_window(&file, "this-run", 9_500).expect("a previous window");
        assert_eq!(picked.key, "newer:1");
    }

    #[test]
    fn pick_last_window_ignores_entries_older_than_the_max_age() {
        let now = SNAPSHOT_MAX_AGE.as_millis() as u64 * 2;
        let file = file_with(vec![window("stale:1", "run-a", 1, 2)]);
        assert!(pick_last_window(&file, "this-run", now).is_none());
    }

    #[test]
    fn pick_last_window_skips_windows_with_no_sessions() {
        let file = file_with(vec![
            window("empty:1", "run-a", 9_000, 0),
            window("has-one:1", "run-a", 1_000, 1),
        ]);
        let picked = pick_last_window(&file, "this-run", 9_500).expect("a window with sessions");
        assert_eq!(picked.key, "has-one:1");
    }

    #[test]
    fn pick_last_window_breaks_timestamp_ties_deterministically() {
        let file = file_with(vec![
            window("bbb:1", "run-a", 5_000, 1),
            window("aaa:1", "run-a", 5_000, 1),
        ]);
        for _ in 0..5 {
            let picked = pick_last_window(&file, "this-run", 5_500).unwrap();
            assert_eq!(picked.key, "aaa:1");
        }
    }

    #[test]
    fn upsert_window_replaces_the_same_key_and_leaves_other_windows_alone() {
        let mut file = LastSessionFile::default();
        upsert_window(&mut file, window("run-1:1", "run-1", 100, 1));
        upsert_window(&mut file, window("run-1:2", "run-1", 200, 1));
        upsert_window(&mut file, window("run-1:1", "run-1", 300, 3));

        assert_eq!(file.windows.len(), 2);
        let first = file.windows.iter().find(|w| w.key == "run-1:1").unwrap();
        assert_eq!(first.sessions.len(), 3);
        assert_eq!(first.updated_at_ms, 300);
        let second = file.windows.iter().find(|w| w.key == "run-1:2").unwrap();
        assert_eq!(second.sessions.len(), 1);
    }

    #[test]
    fn upsert_window_prunes_the_oldest_beyond_the_window_cap() {
        let mut file = LastSessionFile::default();
        for i in 0..(MAX_SNAPSHOT_WINDOWS + 4) {
            upsert_window(
                &mut file,
                window(&format!("run-1:{i}"), "run-1", 1_000 + i as u64, 1),
            );
        }
        assert_eq!(file.windows.len(), MAX_SNAPSHOT_WINDOWS);
        // The newest survive.
        assert!(file
            .windows
            .iter()
            .any(|w| w.key == format!("run-1:{}", MAX_SNAPSHOT_WINDOWS + 3)));
        assert!(!file.windows.iter().any(|w| w.key == "run-1:0"));
    }

    #[test]
    fn sanitize_sessions_rejects_hostile_session_ids() {
        // Ids reach argv. The snapshot file is as untrusted as any other file
        // under $HOME, so it goes through the same gate as the transcript scan.
        let hostile = vec![
            session("claude", "", "/repo"),
            session("claude", "--dangerously-skip-permissions", "/repo"),
            session("claude", "../../etc/passwd", "/repo"),
            session("claude", "has space", "/repo"),
            session("claude", &"x".repeat(129), "/repo"),
        ];
        assert!(sanitize_sessions(hostile).is_empty());
    }

    #[test]
    fn sanitize_sessions_rejects_empty_adapter_ids() {
        assert!(sanitize_sessions(vec![session("", "fine-id", "/repo")]).is_empty());
    }

    #[test]
    fn sanitize_sessions_keeps_good_entries_and_dedupes() {
        let kept = sanitize_sessions(vec![
            session("claude", "one", "/repo"),
            session("claude", "one", "/elsewhere"),
            session("codex", "one", "/repo"),
        ]);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].cwd, PathBuf::from("/repo"));
        assert_eq!(kept[1].adapter_id, "codex");
    }

    #[test]
    fn sanitize_sessions_truncates_to_the_session_cap() {
        let many: Vec<_> = (0..MAX_SNAPSHOT_SESSIONS + 10)
            .map(|i| session("claude", &format!("id-{i}"), "/repo"))
            .collect();
        assert_eq!(sanitize_sessions(many).len(), MAX_SNAPSHOT_SESSIONS);
    }
}
