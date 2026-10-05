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

/// Snapshots older than this are not offered.
///
/// This is a *staleness* bound and nothing else: it decides whether a restore
/// point is offered at all, never how many windows one restore reopens. That
/// width is set by [`RESTORE_SIBLING_WINDOW`] in [`pick_last_window_set`], which
/// keys off the run that wrote the snapshot. Raising this value must not turn
/// "the last window or windows" into "every window of the last month" — keep the
/// two rules separate.
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How far apart two windows of the same run may have last been written and
/// still count as "open together" when the run ended.
///
/// A run that stays up for weeks writes an entry per window as its agents
/// change, so same-run entries can be days apart. Without this band, restoring
/// "the last window/s" would drag in windows the user closed a fortnight ago.
/// The paint path rewrites a live window at most every couple of seconds, so
/// windows that were genuinely open together land well inside five minutes.
const RESTORE_SIBLING_WINDOW: Duration = Duration::from_secs(5 * 60);

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
    /// The WSL distro the session's files live in; `None` for this machine.
    /// A session from a distro resumes in that distro even when the CLI is
    /// also installed here. Absent in files written before it existed, which
    /// restore as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
}

impl SnapshotSession {
    /// Where the session was recorded, as the resume path takes it.
    pub fn origin(&self) -> crate::agent_herd::vendor::SessionOrigin {
        match &self.distro {
            Some(distro) => crate::agent_herd::vendor::SessionOrigin::Wsl(distro.clone()),
            None => crate::agent_herd::vendor::SessionOrigin::Host,
        }
    }
}

/// Which way a split divides its pane. Mirrors `mux::tab::SplitDirection`:
/// `Horizontal` puts the two halves side by side, `Vertical` stacks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotSplit {
    Horizontal,
    Vertical,
}

/// One tab's pane tree, reduced to the panes that held a restorable agent.
///
/// Generic over the leaf so the same shape serves every stage: pane ids while
/// capturing, indices into [`WindowSnapshot::sessions`] on disk, sessions once
/// loaded, spawn commands while restoring. Leaves are always visited first
/// subtree before second, which is the order the restore spawns them in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutTree<L> {
    Leaf(L),
    Split {
        direction: SnapshotSplit,
        /// Share of the split taken by `first`, in percent (5..=95).
        first_percent: u8,
        first: Box<LayoutTree<L>>,
        second: Box<LayoutTree<L>>,
    },
}

/// A tab layout as stored: leaves index into the window's `sessions`.
pub type SnapshotLayout = LayoutTree<usize>;

/// A tab layout as offered for restore: leaves are the sessions themselves.
pub type RestoreTab = LayoutTree<SnapshotSession>;

/// Clamp a split share so neither side of a restored split is a sliver.
pub fn clamp_split_percent(percent: u8) -> u8 {
    percent.clamp(5, 95)
}

impl<L> LayoutTree<L> {
    /// Leaves in spawn order.
    pub fn leaves(&self) -> Vec<&L> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a L>) {
        match self {
            LayoutTree::Leaf(leaf) => out.push(leaf),
            LayoutTree::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    pub fn leaf_count(&self) -> usize {
        match self {
            LayoutTree::Leaf(_) => 1,
            LayoutTree::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// The leaf that occupies a split's pane before it is split: the first
    /// leaf of the first subtree, all the way down.
    pub fn first_leaf(&self) -> &L {
        match self {
            LayoutTree::Leaf(leaf) => leaf,
            LayoutTree::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Map every leaf, dropping those `f` rejects. A split that loses one side
    /// collapses into the other; one that loses both disappears. Leaves are
    /// visited in spawn order, so `f` may carry order-dependent state.
    pub fn filter_map<M>(self, f: &mut impl FnMut(L) -> Option<M>) -> Option<LayoutTree<M>> {
        match self {
            LayoutTree::Leaf(leaf) => f(leaf).map(LayoutTree::Leaf),
            LayoutTree::Split {
                direction,
                first_percent,
                first,
                second,
            } => {
                let first = first.filter_map(f);
                let second = second.filter_map(f);
                match (first, second) {
                    (Some(first), Some(second)) => Some(LayoutTree::Split {
                        direction,
                        first_percent: clamp_split_percent(first_percent),
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }
}

/// What a window persists: its sessions, and how they sat in its tabs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowAgents {
    pub sessions: Vec<SnapshotSession>,
    /// One entry per tab that held an agent, in tab order. Every index into
    /// `sessions` appears in exactly one tab.
    pub tabs: Vec<SnapshotLayout>,
}

impl WindowAgents {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
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
    /// How `sessions` were laid out in tabs and splits. Additive, so it does not
    /// bump [`SNAPSHOT_VERSION`]: a file written before it has none, and then
    /// every session is restored into a tab of its own, as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<SnapshotLayout>,
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
    config::DATA_DIR.join(config::brand::state_file_name("tgz-last-session", "json"))
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
    sanitize_window_agents(WindowAgents {
        sessions,
        tabs: Vec::new(),
    })
    .sessions
}

/// [`sanitize_sessions`], plus keeping the tab layouts consistent with what
/// survived: leaves of dropped sessions are pruned (their splits collapse),
/// out-of-range or repeated indices are dropped, and any session no layout
/// places gets a tab of its own. Afterwards every session sits in exactly one
/// tab, which is what the restore relies on.
fn sanitize_window_agents(agents: WindowAgents) -> WindowAgents {
    let mut seen = std::collections::HashSet::new();
    let mut remap: Vec<Option<usize>> = Vec::with_capacity(agents.sessions.len());
    let mut sessions = Vec::new();
    for session in agents.sessions {
        let keep = sessions.len() < MAX_SNAPSHOT_SESSIONS
            && !session.adapter_id.is_empty()
            && crate::agent_herd::sessions::session_id_is_sane(&session.session_id)
            && seen.insert((session.adapter_id.clone(), session.session_id.clone()));
        if keep {
            remap.push(Some(sessions.len()));
            sessions.push(session);
        } else {
            remap.push(None);
        }
    }

    let mut placed = vec![false; sessions.len()];
    let mut tabs = Vec::new();
    for layout in agents.tabs {
        let pruned = layout.filter_map(&mut |old: usize| {
            let new = remap.get(old).copied().flatten()?;
            let already = std::mem::replace(&mut placed[new], true);
            (!already).then_some(new)
        });
        tabs.extend(pruned);
    }
    for (index, was_placed) in placed.iter().enumerate() {
        if !was_placed {
            tabs.push(LayoutTree::Leaf(index));
        }
    }
    WindowAgents { sessions, tabs }
}

/// The tabs one restore reopens, across every window of the set, in window
/// then tab order.
///
/// A window written before layouts were recorded has no `tabs`; sanitizing
/// gives each of its sessions a tab of its own. A session already placed by an
/// earlier window is pruned from later ones, and the whole set is capped at
/// [`MAX_SNAPSHOT_SESSIONS`] in spawn order.
fn restore_tabs(windows: &[&WindowSnapshot]) -> Vec<RestoreTab> {
    let mut seen = std::collections::HashSet::new();
    let mut tabs = Vec::new();
    for window in windows {
        let WindowAgents {
            sessions,
            tabs: layouts,
        } = sanitize_window_agents(WindowAgents {
            sessions: window.sessions.clone(),
            tabs: window.tabs.clone(),
        });
        for layout in layouts {
            let tab = layout.filter_map(&mut |index: usize| {
                let session = sessions.get(index)?;
                let fresh = seen.len() < MAX_SNAPSHOT_SESSIONS
                    && seen.insert((session.adapter_id.clone(), session.session_id.clone()));
                fresh.then(|| session.clone())
            });
            tabs.extend(tab);
        }
    }
    tabs
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

/// Every window of the last run that was open when that run ended.
///
/// This is what "reopen the last window/s" means. The user's windows at quit
/// time are a *set*, not a single window, and restoring only the newest one
/// silently drops the rest — but restoring every window in the file is worse,
/// because the file spans up to [`SNAPSHOT_MAX_AGE`].
///
/// The set is pinned to one shutdown by two rules that [`SNAPSHOT_MAX_AGE`]
/// deliberately does not participate in:
///
/// 1. same `run_id` as the newest candidate — never a union across runs, so two
///    separate launches minutes apart stay separate;
/// 2. last written within [`RESTORE_SIBLING_WINDOW`] of it — so a long-lived run
///    that wrote eight window entries over three weeks yields the last set, not
///    all eight.
///
/// Returned oldest-first by key so the reopened tabs land in a stable order.
fn pick_last_window_set<'a>(
    file: &'a LastSessionFile,
    current_run_id: &str,
    now_ms: u64,
) -> Vec<&'a WindowSnapshot> {
    let Some(newest) = pick_last_window(file, current_run_id, now_ms) else {
        return Vec::new();
    };
    let band_ms = RESTORE_SIBLING_WINDOW.as_millis() as u64;
    let max_age_ms = SNAPSHOT_MAX_AGE.as_millis() as u64;
    let mut set: Vec<&WindowSnapshot> = file
        .windows
        .iter()
        // Same filters as `pick_last_window`; a sibling is not exempt from them.
        .filter(|w| w.run_id != current_run_id)
        .filter(|w| !w.sessions.is_empty())
        .filter(|w| now_ms.saturating_sub(w.updated_at_ms) <= max_age_ms)
        // Rule 1: one run only.
        .filter(|w| w.run_id == newest.run_id)
        // Rule 2: open at the same time as the newest. Absolute difference, so a
        // window written a moment *after* the seed still counts.
        .filter(|w| w.updated_at_ms.abs_diff(newest.updated_at_ms) <= band_ms)
        .collect();
    set.sort_by(|a, b| a.key.cmp(&b.key));
    set
}

/// Record one window's agent sessions. Best-effort; safe to call from a worker
/// thread.
pub fn record_window_sessions(key: String, agents: WindowAgents, closed_cleanly: bool) {
    let WindowAgents { sessions, tabs } = sanitize_window_agents(agents);
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
            tabs,
        },
    );
    write_file_at(&path, &file);
}

/// What one restore click should reopen: the agent sessions of every window that
/// was open when the last previous run ended.
///
/// Reads the filesystem, so this is called once at window creation and never
/// from paint. `None` when there is nothing to offer.
pub fn load_last_window() -> Option<LastWindowSet> {
    let path = state_path();
    let file = read_file_at(&path);
    let windows = pick_last_window_set(&file, run_id(), epoch_millis_now());
    if windows.is_empty() {
        return None;
    }
    let window_count = windows.len();
    // Dedupe across windows too: the same session can only be resumed once, and
    // two windows may each have had a row for it.
    let tabs = restore_tabs(&windows);
    let sessions: Vec<SnapshotSession> = tabs
        .iter()
        .flat_map(|tab| tab.leaves().into_iter().cloned())
        .collect();
    (!sessions.is_empty()).then(|| LastWindowSet {
        sessions,
        tabs,
        window_count,
    })
}

/// The restore offer: what to reopen, and how many windows it came from.
///
/// The count is for the label only ("7 agents · 2 windows"); the restore itself
/// reopens every window's tabs into the current window, splits included, but
/// not window geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastWindowSet {
    /// Sessions to reopen, deduped, in spawn order: the leaves of `tabs`.
    pub sessions: Vec<SnapshotSession>,
    /// The tabs to rebuild, each session in exactly one of them.
    pub tabs: Vec<RestoreTab>,
    /// How many windows contributed. Always at least 1.
    pub window_count: usize,
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
            distro: None,
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
            tabs: Vec::new(),
        }
    }

    /// Like `window`, but with session ids unique to this window, so a test can
    /// tell which windows a set actually pulled from.
    fn window_ids(key: &str, run: &str, updated_at_ms: u64, ids: &[&str]) -> WindowSnapshot {
        WindowSnapshot {
            key: key.to_string(),
            run_id: run.to_string(),
            updated_at_ms,
            closed_cleanly: false,
            sessions: ids
                .iter()
                .map(|id| session("claude", id, "/repo"))
                .collect(),
            tabs: Vec::new(),
        }
    }

    fn ids_of(windows: &[&WindowSnapshot]) -> Vec<String> {
        windows
            .iter()
            .flat_map(|w| w.sessions.iter().map(|s| s.session_id.clone()))
            .collect()
    }

    const MINUTE_MS: u64 = 60 * 1000;
    const DAY_MS: u64 = 24 * 60 * MINUTE_MS;
    const NOW_MS: u64 = 1_800_000_000_000;

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
                distro: None,
            }],
            tabs: vec![LayoutTree::Split {
                direction: SnapshotSplit::Horizontal,
                first_percent: 40,
                first: Box::new(LayoutTree::Leaf(0)),
                second: Box::new(LayoutTree::Leaf(0)),
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

    #[test]
    fn window_set_takes_every_window_open_at_the_same_shutdown() {
        // Two windows of one run, written a minute apart: both were up when the
        // run ended, so a restore must bring back both.
        let file = file_with(vec![
            window_ids("run-a:0", "run-a", NOW_MS - DAY_MS, &["a0", "a1"]),
            window_ids("run-a:1", "run-a", NOW_MS - DAY_MS - MINUTE_MS, &["a2"]),
        ]);
        let set = pick_last_window_set(&file, "current", NOW_MS);
        assert_eq!(set.len(), 2);
        assert_eq!(ids_of(&set), vec!["a0", "a1", "a2"]);
    }

    #[test]
    fn window_set_drops_same_run_windows_from_an_older_sitting() {
        // One long-lived run that wrote an entry three weeks ago and another one
        // today. Both are inside the 30-day age cap and share a run_id, so only
        // the sibling band can separate them -- and it must.
        let file = file_with(vec![
            window_ids("run-a:0", "run-a", NOW_MS - DAY_MS, &["recent"]),
            window_ids("run-a:1", "run-a", NOW_MS - 21 * DAY_MS, &["ancient"]),
        ]);
        let set = pick_last_window_set(&file, "current", NOW_MS);
        assert_eq!(ids_of(&set), vec!["recent"]);
    }

    #[test]
    fn window_set_never_unions_two_runs() {
        // Two launches two minutes apart: inside the sibling band, but different
        // runs. Restoring both would reopen agents the user already closed once.
        let file = file_with(vec![
            window_ids("run-b:0", "run-b", NOW_MS - DAY_MS, &["newer"]),
            window_ids(
                "run-a:0",
                "run-a",
                NOW_MS - DAY_MS - 2 * MINUTE_MS,
                &["older"],
            ),
        ]);
        let set = pick_last_window_set(&file, "current", NOW_MS);
        assert_eq!(ids_of(&set), vec!["newer"]);
    }

    #[test]
    fn window_set_offers_a_25_day_old_run_whole() {
        // The age cap decides *whether* there is an offer, never how wide it is:
        // a run just inside 30 days still yields all of its windows.
        let age = NOW_MS - 25 * DAY_MS;
        let file = file_with(vec![
            window_ids("run-a:0", "run-a", age, &["a0"]),
            window_ids("run-a:1", "run-a", age - MINUTE_MS, &["a1"]),
        ]);
        let set = pick_last_window_set(&file, "current", NOW_MS);
        assert_eq!(ids_of(&set), vec!["a0", "a1"]);
    }

    #[test]
    fn window_set_still_honours_the_age_cap() {
        let file = file_with(vec![window_ids(
            "run-a:0",
            "run-a",
            NOW_MS - 31 * DAY_MS,
            &["too-old"],
        )]);
        assert!(pick_last_window_set(&file, "current", NOW_MS).is_empty());
    }

    #[test]
    fn window_set_skips_the_current_run_and_empty_windows() {
        let file = file_with(vec![
            window_ids("cur:0", "current", NOW_MS, &["mine"]),
            window_ids("run-a:0", "run-a", NOW_MS - MINUTE_MS, &[]),
            window_ids("run-b:0", "run-b", NOW_MS - 2 * MINUTE_MS, &["offered"]),
        ]);
        let set = pick_last_window_set(&file, "current", NOW_MS);
        assert_eq!(ids_of(&set), vec!["offered"]);
    }

    #[test]
    fn window_set_is_empty_on_an_unknown_version() {
        let mut file = file_with(vec![window_ids(
            "run-a:0",
            "run-a",
            NOW_MS - MINUTE_MS,
            &["a0"],
        )]);
        file.version = SNAPSHOT_VERSION + 1;
        assert!(pick_last_window_set(&file, "current", NOW_MS).is_empty());
    }

    fn split<L>(first_percent: u8, first: LayoutTree<L>, second: LayoutTree<L>) -> LayoutTree<L> {
        LayoutTree::Split {
            direction: SnapshotSplit::Horizontal,
            first_percent,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn leaf_ids(tab: &RestoreTab) -> Vec<&str> {
        tab.leaves()
            .into_iter()
            .map(|s| s.session_id.as_str())
            .collect()
    }

    #[test]
    fn filter_map_collapses_a_split_that_loses_a_side() {
        let tree = split(
            30,
            LayoutTree::Leaf(1),
            split(50, LayoutTree::Leaf(2), LayoutTree::Leaf(3)),
        );
        let pruned = tree
            .filter_map(&mut |n: usize| (n != 2).then_some(n))
            .unwrap();
        assert_eq!(pruned, split(30, LayoutTree::Leaf(1), LayoutTree::Leaf(3)));
    }

    #[test]
    fn filter_map_visits_leaves_in_spawn_order() {
        let tree = split(
            50,
            split(50, LayoutTree::Leaf("a"), LayoutTree::Leaf("b")),
            LayoutTree::Leaf("c"),
        );
        let mut order = Vec::new();
        tree.filter_map(&mut |l| {
            order.push(l);
            Some(l)
        });
        assert_eq!(order, vec!["a", "b", "c"]);
    }

    #[test]
    fn sanitize_remaps_layout_indices_past_a_dropped_session() {
        let agents = sanitize_window_agents(WindowAgents {
            sessions: vec![
                session("claude", "--evil", "/repo"),
                session("claude", "left", "/repo"),
                session("claude", "right", "/repo"),
            ],
            tabs: vec![
                split(60, LayoutTree::Leaf(1), LayoutTree::Leaf(2)),
                LayoutTree::Leaf(0),
            ],
        });
        assert_eq!(agents.sessions.len(), 2);
        assert_eq!(
            agents.tabs,
            vec![split(60, LayoutTree::Leaf(0), LayoutTree::Leaf(1))]
        );
    }

    #[test]
    fn sanitize_drops_bad_indices_and_gives_unplaced_sessions_a_tab() {
        // The file is untrusted: an index past the end, the same index twice,
        // and a session no layout mentions.
        let agents = sanitize_window_agents(WindowAgents {
            sessions: vec![
                session("claude", "a", "/repo"),
                session("claude", "b", "/repo"),
            ],
            tabs: vec![
                split(50, LayoutTree::Leaf(0), LayoutTree::Leaf(7)),
                LayoutTree::Leaf(0),
            ],
        });
        assert_eq!(agents.tabs, vec![LayoutTree::Leaf(0), LayoutTree::Leaf(1)]);
    }

    #[test]
    fn a_legacy_window_restores_one_tab_per_session() {
        let legacy = window_ids("run-a:0", "run-a", NOW_MS, &["a0", "a1"]);
        let tabs = restore_tabs(&[&legacy]);
        assert_eq!(tabs.len(), 2);
        assert_eq!(leaf_ids(&tabs[0]), vec!["a0"]);
        assert_eq!(leaf_ids(&tabs[1]), vec!["a1"]);
    }

    #[test]
    fn a_legacy_file_without_tabs_still_parses() {
        let parsed: WindowSnapshot =
            serde_json::from_str(r#"{"key":"k","run_id":"r","updated_at_ms":1,"sessions":[]}"#)
                .unwrap();
        assert!(parsed.tabs.is_empty());
    }

    #[test]
    fn a_split_tab_restores_as_one_tab() {
        let mut win = window_ids("run-a:0", "run-a", NOW_MS, &["left", "right", "solo"]);
        win.tabs = vec![
            split(55, LayoutTree::Leaf(0), LayoutTree::Leaf(1)),
            LayoutTree::Leaf(2),
        ];
        let tabs = restore_tabs(&[&win]);
        assert_eq!(tabs.len(), 2);
        assert_eq!(leaf_ids(&tabs[0]), vec!["left", "right"]);
        assert!(matches!(
            tabs[0],
            LayoutTree::Split {
                first_percent: 55,
                ..
            }
        ));
        assert_eq!(leaf_ids(&tabs[1]), vec!["solo"]);
    }

    #[test]
    fn a_session_in_two_windows_is_restored_once() {
        let first = window_ids("run-a:0", "run-a", NOW_MS, &["shared"]);
        let mut second = window_ids("run-a:1", "run-a", NOW_MS, &["shared", "other"]);
        second.tabs = vec![split(50, LayoutTree::Leaf(0), LayoutTree::Leaf(1))];
        let tabs = restore_tabs(&[&first, &second]);
        assert_eq!(tabs.len(), 2);
        assert_eq!(leaf_ids(&tabs[0]), vec!["shared"]);
        // The split lost its left side and collapsed to the survivor.
        assert_eq!(
            tabs[1],
            LayoutTree::Leaf(session("claude", "other", "/repo"))
        );
    }
}
