//! Discovering an agent's **past** sessions on disk, so they can be resumed.
//!
//! This is the counterpart to [`super::claude`], which deliberately reports only
//! agents that are running right now: it filters every candidate through a
//! liveness check, so a session that has exited vanishes from the herd
//! immediately. Resuming needs the opposite view — the sessions that are *over*.
//!
//! Every adapter already carries a resume template
//! (`claude --resume {session_id}`, `codex resume {session_id}`, …), and the
//! sidebar already expands those templates. The missing half was always the
//! session id: it used to come only from OSC user vars, which Claude Code does
//! not emit. This module supplies it from the transcripts the agents leave
//! behind.
//!
//! Like the rest of [`crate::agent_herd`] the on-disk formats are treated as
//! **undocumented internals**: every field is optional, every parse failure
//! degrades one row rather than the scan, and an unreadable file is simply not
//! offered.
//!
//! ## Cost
//!
//! One project directory here holds 57 transcripts of 285 KB – 4 MB. Reading all
//! of them to build a menu of ten rows would be absurd, so the scan is two-phase:
//! a `stat`-only sweep collects candidates and sorts them by modification time,
//! and only the newest `limit` are opened to read a label. Measured on the
//! author's machine: 18 ms to stat 182 files, 15 ms to label the newest 12.
//!
//! Even so this touches the filesystem and must never run on the GUI thread —
//! callers scan on a worker thread and cache the result.

use super::{transcript, HerdActivity, HerdContent, HerdEvent, HerdEventKind, SubagentNode};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Longest plausible session id, and the alphabet it may use.
///
/// Ids reach argv, and they originate from a *filename* rather than from
/// anything this terminal wrote, so a file dropped into the projects directory
/// must not be able to smuggle in an argv element or a path traversal. Real ids
/// are UUIDs; this is deliberately a little wider without admitting separators.
const MAX_SESSION_ID_LEN: usize = 128;

/// Branch names that are not worth showing: on the default branch the prefix is
/// noise, and every row would carry it.
///
/// `HEAD` is what Claude records for a detached checkout: not a branch at all.
const UNINTERESTING_BRANCHES: &[&str] = &["main", "master", "trunk", "default", "HEAD"];

/// Shown when a session's own store records no usable description — a Claude
/// transcript whose only turn was `/clear`, or an OpenCode session never given a
/// title. Better than a blank row, which reads as a rendering bug.
const NO_DESCRIPTION: &str = "(no description)";

/// One resumable session found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSession {
    /// Adapter id this session belongs to, e.g. `"claude"`. Matches the keys of
    /// `agent_ui.adapters`, so the caller can look the resume template up.
    pub adapter_id: String,
    /// The vendor's own session identifier, substituted into `{session_id}`.
    pub session_id: String,
    /// Directory the session ran in. A resume belongs here, not wherever the
    /// active pane happens to be.
    pub cwd: PathBuf,
    /// Final component of `cwd`, shown as the row's project prefix.
    pub project: String,
    /// Branch the session was working on, when the transcript records one.
    pub git_branch: Option<String>,
    /// Short human description of what the session was about.
    pub label: String,
    /// Last write to the transcript; the sort key, newest first.
    pub modified: SystemTime,
}

impl AgentSession {
    /// The row text: `project · label`, with a branch prefix when the branch is
    /// worth mentioning.
    pub fn menu_label(&self) -> String {
        let branch = self
            .git_branch
            .as_deref()
            .filter(|branch| !branch.trim().is_empty())
            .filter(|branch| {
                !UNINTERESTING_BRANCHES
                    .iter()
                    .any(|dull| branch.eq_ignore_ascii_case(dull))
            });
        match branch {
            Some(branch) => format!("[{branch}] {} · {}", self.project, self.label),
            None => format!("{} · {}", self.project, self.label),
        }
    }
}

/// A candidate session, before its description is known.
///
/// Vendors differ in how much a cheap enumeration reveals. Claude, Codex and
/// Copilot keep one file per session, so a description costs a read and is
/// deferred; OpenCode answers with a single indexed query, so its rows arrive
/// complete. Both shapes carry the same sort key, so the `limit` is applied once
/// over the merged list and only the reads that survive that cut are paid for.
enum Candidate {
    /// Needs a file read before it can be offered.
    Deferred {
        adapter_id: &'static str,
        session_id: String,
        path: PathBuf,
        modified: SystemTime,
    },
    /// Already complete.
    Ready(AgentSession),
}

impl Candidate {
    fn modified(&self) -> SystemTime {
        match self {
            Candidate::Deferred { modified, .. } => *modified,
            Candidate::Ready(session) => session.modified,
        }
    }

    /// Stable tiebreak for equal timestamps, so ordering does not depend on
    /// directory iteration order.
    fn sort_key(&self) -> (&str, &str) {
        match self {
            Candidate::Deferred {
                adapter_id,
                session_id,
                ..
            } => (adapter_id, session_id),
            Candidate::Ready(session) => (&session.adapter_id, &session.session_id),
        }
    }
}

/// Collect the `limit` most recently touched resumable sessions.
///
/// Scans every project, newest first, across every vendor with a readable
/// session store. Returns fewer than `limit` — possibly none — when that is all
/// there is; never errors, since a missing or unreadable store just means that
/// agent contributes no history.
pub fn collect_recent_sessions(home: &Path, limit: usize) -> Vec<AgentSession> {
    collect_recent_sessions_across(std::slice::from_ref(&home.to_path_buf()), limit)
}

/// Like [`collect_recent_sessions`], but over several home directories.
///
/// On Windows the agent CLIs usually live inside a WSL distro and write their
/// transcripts to the distro's home, so the Windows home alone yields nothing.
/// Candidates from every home are pooled before the sort, so the result is one
/// newest-first list rather than a list per home.
pub fn collect_recent_sessions_across(homes: &[PathBuf], limit: usize) -> Vec<AgentSession> {
    collect_recent_sessions_since(homes, limit, None)
}

/// Why a candidate found on disk was not offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
    /// Nobody typed into it: a headless run, a scheduled task, a subagent.
    Headless,
    /// The store records no directory, and a resume must not guess one.
    NoCwd,
    /// Nothing in the head parsed, or it lacks the id a resume needs.
    Unreadable,
}

/// What one scan saw, so a log line can say why a session is not listed.
///
/// A session missing from the menu is otherwise indistinguishable from one
/// that was never found: these counts tell a filtered transcript from a home
/// that was not scanned at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionScanStats {
    /// Candidates inside the age window, before the `limit` cut.
    pub candidates: usize,
    pub listed: usize,
    pub headless: usize,
    pub no_cwd: usize,
    pub unreadable: usize,
    /// An id [`session_id_is_sane`] refused.
    pub bad_id: usize,
    /// Another transcript of a session already listed (Codex resumes).
    pub duplicates: usize,
    /// Head reads actually performed; the rest came from [`DETAILS_CACHE`].
    pub reads: usize,
}

impl SessionScanStats {
    fn count(&mut self, skip: Skip) {
        match skip {
            Skip::Headless => self.headless += 1,
            Skip::NoCwd => self.no_cwd += 1,
            Skip::Unreadable => self.unreadable += 1,
        }
    }
}

/// Head-read results, keyed by transcript path and valid for one mtime.
///
/// The session menus list everything from the last month, which can be
/// hundreds of transcripts, and over a WSL distro's `\\wsl.localhost\` share
/// every head read is a 9P round trip. A rescan (every menu open) then costs a
/// `stat` sweep plus reads of whatever changed. Skips are remembered too, so a
/// headless or unreadable file is not re-read until it is written again.
///
/// Pruned only once it outgrows [`DETAILS_CACHE_CAP`], and then to the
/// candidates of the scan that tipped it over. Pruning on every scan would let
/// two scans over different homes (two windows, one seeing a WSL distro the
/// other does not) evict each other's entries on every open.
static DETAILS_CACHE: LazyLock<Mutex<HashMap<PathBuf, (SystemTime, Result<AgentSession, Skip>)>>> =
    LazyLock::new(Default::default);

/// Entries [`DETAILS_CACHE`] may hold before it is pruned.
const DETAILS_CACHE_CAP: usize = 4096;

/// [`DETAILS_CACHE`], locked. A poisoned lock only means another scan panicked
/// mid-insert; the map itself is still a valid cache.
fn details_cache(
) -> std::sync::MutexGuard<'static, HashMap<PathBuf, (SystemTime, Result<AgentSession, Skip>)>> {
    DETAILS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Like [`collect_recent_sessions_across`], restricted to sessions last touched
/// at or after `newer_than`.
pub fn collect_recent_sessions_since(
    homes: &[PathBuf],
    limit: usize,
    newer_than: Option<SystemTime>,
) -> Vec<AgentSession> {
    scan_recent_sessions(homes, limit, newer_than).0
}

/// [`collect_recent_sessions_since`], plus what the scan saw on the way.
///
/// The cutoff is applied to the cheap `stat` sweep, before the sort and before
/// any transcript is opened, so an old session costs nothing but its `stat`.
/// `limit` stays a hard bound on top of it.
pub fn scan_recent_sessions(
    homes: &[PathBuf],
    limit: usize,
    newer_than: Option<SystemTime>,
) -> (Vec<AgentSession>, SessionScanStats) {
    let mut stats = SessionScanStats::default();
    if limit == 0 {
        return (Vec::new(), stats);
    }

    let mut candidates = Vec::new();
    for home in homes {
        let home = home.as_path();
        collect_claude_candidates(home, &mut candidates);
        collect_codex_candidates(home, &mut candidates);
        collect_opencode_candidates(home, limit, newer_than, &mut candidates);
        collect_copilot_candidates(home, &mut candidates);
    }
    if let Some(cutoff) = newer_than {
        candidates.retain(|candidate| candidate.modified() >= cutoff);
    }
    stats.candidates = candidates.len();

    candidates.sort_by(|a, b| {
        b.modified()
            .cmp(&a.modified())
            .then_with(|| a.sort_key().cmp(&b.sort_key()))
    });

    // One session can own several transcripts: Codex starts a fresh rollout
    // file every time a session is resumed, all carrying the same `session_id`.
    // Offering the same session several times would be noise, and since
    // candidates are already newest-first the first one seen is the one to keep.
    let mut seen = HashSet::new();
    let mut sessions = Vec::new();
    {
        let mut cache = details_cache();
        if cache.len() > DETAILS_CACHE_CAP {
            let live_paths: HashSet<&Path> = candidates
                .iter()
                .filter_map(|candidate| match candidate {
                    Candidate::Deferred { path, .. } => Some(path.as_path()),
                    Candidate::Ready(_) => None,
                })
                .collect();
            cache.retain(|path, _| live_paths.contains(path.as_path()));
        }
    }
    for candidate in candidates {
        if sessions.len() >= limit {
            break;
        }
        let session = match candidate {
            Candidate::Deferred {
                ref path, modified, ..
            } => {
                // Locked per lookup and per insert, never across the read: over
                // a WSL share one read can stall for seconds or not return at
                // all, and holding the lock through it stalled every other
                // window's scan behind it. Two windows scanning at once may now
                // read the same file twice, which is the cheaper failure.
                let cached = details_cache()
                    .get(path)
                    .filter(|(cached_at, _)| *cached_at == modified)
                    .map(|(_, details)| details.clone());
                let details = match cached {
                    Some(details) => details,
                    None => {
                        stats.reads += 1;
                        let details = read_candidate(&candidate);
                        details_cache().insert(path.clone(), (modified, details.clone()));
                        details
                    }
                };
                match details {
                    Ok(session) => session,
                    Err(skip) => {
                        stats.count(skip);
                        continue;
                    }
                }
            }
            Candidate::Ready(session) => session,
        };
        if !session_id_is_sane(&session.session_id) {
            stats.bad_id += 1;
            continue;
        }
        if seen.insert((session.adapter_id.clone(), session.session_id.clone())) {
            sessions.push(session);
        } else {
            stats.duplicates += 1;
        }
    }
    stats.listed = sessions.len();
    (sessions, stats)
}

/// Build a finished session from fields a vendor already knew.
fn ready_session(
    adapter_id: &str,
    session_id: String,
    cwd: PathBuf,
    git_branch: Option<String>,
    label: Option<String>,
    modified: SystemTime,
) -> AgentSession {
    let project = cwd
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    AgentSession {
        adapter_id: adapter_id.to_string(),
        session_id,
        cwd,
        project,
        git_branch,
        label: label.unwrap_or_else(|| NO_DESCRIPTION.to_string()),
        modified,
    }
}

/// Read the label and directory for one candidate.
///
/// A candidate whose directory cannot be determined is dropped rather than
/// guessed at: without a `cwd` the resume would start in the wrong place, which
/// is worse than not offering the row.
fn read_candidate(candidate: &Candidate) -> Result<AgentSession, Skip> {
    let Candidate::Deferred {
        adapter_id,
        session_id,
        path,
        modified,
    } = candidate
    else {
        return Err(Skip::Unreadable);
    };
    let details = match *adapter_id {
        "claude" => read_claude_details(path),
        "codex" => read_codex_details(path),
        "copilot" => read_copilot_workspace(path),
        _ => Err(Skip::Unreadable),
    }?;
    // Claude and Copilot name their file (or directory) after the session; Codex
    // records the id inside.
    let session_id = details
        .session_id
        .clone()
        .unwrap_or_else(|| session_id.clone());
    Ok(ready_session(
        adapter_id,
        session_id,
        details.cwd,
        details.git_branch,
        details.label,
        *modified,
    ))
}

/// What a head read recovers from a transcript.
struct SessionDetails {
    cwd: PathBuf,
    git_branch: Option<String>,
    /// `None` when the store records nothing usable; the caller substitutes
    /// [`NO_DESCRIPTION`].
    label: Option<String>,
    /// Set only by vendors that record the id inside the file rather than in
    /// its name; `None` keeps the id derived from the filename.
    session_id: Option<String>,
}

/// True when `id` is safe to substitute into a resume command.
///
/// The leading-dash rejection is load-bearing, not cosmetic. Ids are substituted
/// as their own argv element (`claude --resume <id>`), so a transcript named
/// `--dangerously-skip-permissions.jsonl` would otherwise hand the agent CLI a
/// flag where it expected a value. Anyone who can write into the projects
/// directory picks the filename, so the id must not be able to look like an
/// option.
///
/// Also called by the last-session restore path: a snapshot file is untrusted
/// input like any other file under `$HOME`, and this is the last place before
/// argv that can still say no.
pub(crate) fn session_id_is_sane(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SESSION_ID_LEN
        && !id.starts_with('-')
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

/// Enumerate `~/.claude/projects/*/<sessionId>.jsonl`.
///
/// Each project directory also contains one `<sessionId>/` subdirectory per
/// session holding subagent transcripts; only plain `.jsonl` *files* directly
/// under the project directory are sessions.
fn collect_claude_candidates(home: &Path, out: &mut Vec<Candidate>) {
    let projects_root = home.join(".claude").join("projects");
    // Resolve once so a symlinked project directory cannot walk us out of the
    // projects tree, mirroring `claude::resolve_project_dir`.
    let Ok(root) = projects_root.canonicalize() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let project_dir = entry.path();
        let Ok(project_dir) = project_dir.canonicalize() else {
            continue;
        };
        if !project_dir.starts_with(&root) || !project_dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&project_dir) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if !session_id_is_sane(session_id) {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            out.push(Candidate::Deferred {
                adapter_id: "claude",
                session_id: session_id.to_string(),
                path,
                modified,
            });
        }
    }
}

/// Read a Claude transcript's head for a label, directory and branch.
///
/// The label prefers Claude's own generated `ai-title`, which is exactly the
/// short description wanted here. It is written early but not at a fixed offset,
/// so the whole head budget is spent looking for it before falling back to the
/// first prompt the user actually typed. There is deliberately no search for a
/// `"type":"summary"` entry: current Claude Code does not write one.
///
/// A session nobody typed into is not offered: a headless run (see
/// [`claude_entrypoint_is_headless`]) or a scheduled task (see
/// [`is_claude_scheduled_task`]). Nobody resumes a `claude -p` run, a plugin's
/// background agent or last night's cron job, and on a machine with claude-mem
/// and a scheduled sweep installed they outnumber the real sessions four to
/// one.
fn read_claude_details(path: &Path) -> Result<SessionDetails, Skip> {
    let head = claude_head_scan(path, true);
    if head.headless {
        return Err(Skip::Headless);
    }
    if !head.parsed {
        return Err(Skip::Unreadable);
    }
    Ok(SessionDetails {
        cwd: head.cwd.ok_or(Skip::NoCwd)?,
        git_branch: head.git_branch,
        label: head.label,
        session_id: None,
    })
}

/// What one head scan of a Claude transcript yields.
struct ClaudeHead {
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
    label: Option<String>,
    /// The session was started without a human at the keyboard: a headless
    /// entrypoint, or a scheduled task's automated run.
    headless: bool,
    /// At least one head line was JSON. Without one the file is unreadable,
    /// not merely short of a directory.
    parsed: bool,
}

/// Opening tag the Claude desktop app's task scheduler wraps an automated
/// run's prompt in: `<scheduled-task name="team-jira-sweep" file="…/SKILL.md">
/// This is an automated run of a scheduled task. The user is not present…`.
const CLAUDE_SCHEDULED_TASK_TAG: &str = "<scheduled-task";

/// True when `prompt` is a scheduled task's automated run.
///
/// The entrypoint cannot tell these apart: the desktop app starts every
/// session, real or scheduled, as `claude-desktop` with `promptSource: "sdk"`.
/// The prompt is the only record of it.
fn is_claude_scheduled_task(prompt: &str) -> bool {
    prompt.trim_start().starts_with(CLAUDE_SCHEDULED_TASK_TAG)
}

/// True for a Claude Code `entrypoint` that means no interactive user.
///
/// Observed values: `cli` (the terminal UI) and `claude-desktop` are people;
/// `sdk-cli` is `claude -p` and anything built on it (claude-mem's observer, a
/// session-note hook), `sdk-ts` an Agent SDK program. The `sdk-` prefix covers
/// the Python SDK as well. Anything else, including a missing field (older
/// transcripts), counts as interactive: hiding a real session is the worse
/// mistake.
fn claude_entrypoint_is_headless(entrypoint: &str) -> bool {
    entrypoint.starts_with("sdk-")
}

/// Claude's own short description of a session, read from its transcript head.
///
/// The live `sessions/<pid>.json` also carries a `name`, but when its
/// `nameSource` is `derived` that name is only a slug of the working directory —
/// which is why the herd asks here instead. Bounded head read with an early
/// break, and it runs on the herd's scan thread, never on the paint path.
pub fn claude_transcript_label(path: &Path) -> Option<String> {
    // A live headless session still gets its label in the herd: it is running,
    // so it belongs there whether or not anyone would resume it.
    claude_head_scan(path, false).label
}

/// `stop_at_headless` ends the scan as soon as the transcript turns out to be
/// headless, for callers that drop such sessions anyway: without it, each of
/// them costs a full head read looking for a title that is never used.
fn claude_head_scan(path: &Path, stop_at_headless: bool) -> ClaudeHead {
    let mut cwd = None;
    let mut git_branch = None;
    let mut title = None;
    let mut prompt = None;
    let mut entrypoint_seen = false;
    let mut headless = false;
    let mut parsed = false;

    for line in transcript::head_lines(path, transcript::HEAD_LINES) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        parsed = true;
        if !entrypoint_seen {
            if let Some(entrypoint) = value.get("entrypoint").and_then(|value| value.as_str()) {
                entrypoint_seen = true;
                headless |= claude_entrypoint_is_headless(entrypoint);
            }
        }
        // A scheduled run's prompt is queued before the session starts, so the
        // very first record already says so; the first user turn repeats it,
        // for transcripts written without the queue record.
        let queued_prompt = (value.get("type").and_then(|value| value.as_str())
            == Some("queue-operation"))
        .then(|| value.get("content").and_then(|value| value.as_str()))
        .flatten();
        if queued_prompt.is_some_and(is_claude_scheduled_task) {
            headless = true;
        }
        if headless && stop_at_headless {
            break;
        }
        if cwd.is_none() {
            cwd = value
                .get("cwd")
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty())
                .map(PathBuf::from);
        }
        if git_branch.is_none() {
            git_branch = value
                .get("gitBranch")
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty())
                .map(str::to_string);
        }
        match value.get("type").and_then(|value| value.as_str()) {
            Some("ai-title") => {
                title = value
                    .get("aiTitle")
                    .and_then(|value| value.as_str())
                    .map(|text| transcript::trim_to_words(text, 10))
                    .filter(|text| !text.is_empty());
            }
            Some("user") => {
                // `isMeta` marks injected context and `isSidechain` marks a
                // subagent's own turns; neither is something the user asked for.
                let is_meta = value
                    .get("isMeta")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let is_sidechain = value
                    .get("isSidechain")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                if prompt.is_none() && !is_meta && !is_sidechain {
                    let text = claude_message_text(&value);
                    if text.as_deref().is_some_and(is_claude_scheduled_task) {
                        headless = true;
                        if stop_at_headless {
                            break;
                        }
                    }
                    prompt = text.as_deref().and_then(transcript::describe_prompt);
                }
            }
            _ => {}
        }
        // The title is the best answer available; once both it and the
        // directory are known there is nothing left to look for.
        if title.is_some() && cwd.is_some() && git_branch.is_some() {
            break;
        }
    }

    ClaudeHead {
        cwd,
        git_branch,
        label: title.or(prompt),
        headless,
        parsed,
    }
}

/// Flatten a Claude `message.content` into plain text.
///
/// Content is either a bare string or an array of typed blocks; only `text`
/// blocks carry prose, and tool results are noise for a label.
fn claude_message_text(value: &serde_json::Value) -> Option<String> {
    let content = value.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let blocks = content.as_array()?;
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(|value| value.as_str()) != Some("text") {
            continue;
        }
        if let Some(text) = block.get("text").and_then(|value| value.as_str()) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
        }
    }
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Enumerate `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`.
///
/// Unlike Claude, Codex files are laid out by date rather than by project, so
/// the project a session belongs to is only knowable by reading its first line.
/// The date nesting is walked to a fixed depth rather than recursively: an
/// unbounded walk of a user's home directory is not something a menu should do.
fn collect_codex_candidates(home: &Path, out: &mut Vec<Candidate>) {
    const DATE_DEPTH: usize = 3;

    let root = home.join(".codex").join("sessions");
    let mut dirs = vec![root];
    for _ in 0..DATE_DEPTH {
        let mut next = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    next.push(path);
                }
            }
        }
        dirs = next;
    }

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            // The session id lives in the file's own `session_meta`, not in the
            // `rollout-<date>-<uuid>` filename, so it is filled in during the
            // detail read and left empty here.
            out.push(Candidate::Deferred {
                adapter_id: "codex",
                session_id: String::new(),
                path,
                modified,
            });
        }
    }
}

/// Read a Codex rollout's head for a label, directory and session id.
///
/// Line one is a `session_meta` carrying both the id and the cwd. The label is
/// the first genuine user message: Codex replays the project's `AGENTS.md` as a
/// synthetic first user turn, so that one has to be skipped.
/// True for a Codex `session_meta.source` that means no interactive user.
///
/// `"cli"` (the TUI) and `"vscode"` are people. A subagent thread — Codex's
/// guardian, or a worker spawned by another thread — records an object
/// `{"subagent": …}` and is resumed through its parent, never on its own.
/// `"exec"` (`codex exec`) and `"mcp"` (a session served to an MCP client)
/// come from Codex's `SessionSource` enum rather than observed rollouts; like
/// `claude -p`, nobody resumes them. A missing or unknown source counts as
/// interactive.
fn codex_source_is_headless(source: &serde_json::Value) -> bool {
    match source {
        serde_json::Value::String(source) => matches!(source.as_str(), "exec" | "mcp"),
        serde_json::Value::Object(source) => source.contains_key("subagent"),
        _ => false,
    }
}

/// A non-interactive rollout (see [`codex_source_is_headless`]) is not offered.
fn read_codex_details(path: &Path) -> Result<SessionDetails, Skip> {
    let mut cwd = None;
    let mut session_id = None;
    let mut label = None;
    let mut parsed = false;

    for line in transcript::head_lines(path, transcript::HEAD_LINES) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        parsed = true;
        let payload = value.get("payload");
        if value.get("type").and_then(|value| value.as_str()) == Some("session_meta")
            && payload
                .and_then(|payload| payload.get("source"))
                .is_some_and(codex_source_is_headless)
        {
            return Err(Skip::Headless);
        }
        if cwd.is_none() {
            cwd = payload
                .and_then(|payload| payload.get("cwd"))
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty())
                .map(PathBuf::from);
        }
        if session_id.is_none() {
            session_id = payload
                .and_then(|payload| payload.get("session_id"))
                .and_then(|value| value.as_str())
                .filter(|text| !text.is_empty())
                .map(str::to_string);
        }
        if label.is_none() {
            let text = payload
                .filter(|payload| {
                    payload.get("type").and_then(|value| value.as_str()) == Some("user_message")
                })
                .and_then(|payload| payload.get("message"))
                .and_then(|value| value.as_str());
            if let Some(text) = text.filter(|text| !is_codex_instruction_blob(text)) {
                label = transcript::describe_prompt(text);
            }
        }
        if cwd.is_some() && session_id.is_some() && label.is_some() {
            break;
        }
    }

    if !parsed {
        return Err(Skip::Unreadable);
    }
    Ok(SessionDetails {
        cwd: cwd.ok_or(Skip::NoCwd)?,
        // Codex's `session_meta` records no branch, so rows are unprefixed.
        git_branch: None,
        label,
        // The filename carries no id to fall back on.
        session_id: Some(session_id.ok_or(Skip::Unreadable)?),
    })
}

// ---------------------------------------------------------------------------
// OpenCode
// ---------------------------------------------------------------------------

/// Read recent sessions out of OpenCode's SQLite database.
///
/// OpenCode moved from per-session JSON files to a single database, so unlike
/// the other vendors there is nothing to enumerate on the filesystem — the
/// database is the only record. It stores the directory and a generated title
/// per session, which is exactly what a row needs, so this is both cheaper and
/// more accurate than any transcript parsing.
///
/// The database is opened **read-only** and never written. That matters: OpenCode
/// has known corruption reports when several processes write the same database
/// (its own concurrent sessions, or a home directory on NFS), and a reader must
/// not be able to contribute to that.
///
/// `opencode session list --format json` exposes the same fields and would be
/// the more polite interface, but it is scoped to the current directory — it
/// cannot answer "sessions across all projects" without one process spawn per
/// project, so the database it is.
fn collect_opencode_candidates(
    home: &Path,
    limit: usize,
    newer_than: Option<SystemTime>,
    out: &mut Vec<Candidate>,
) {
    let db = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if !db.is_file() {
        return;
    }
    // `parent_id IS NULL` drops child sessions (OpenCode's subagents), which are
    // not independently resumable. `time_archived IS NULL` drops the ones the
    // user has already filed away. Times are epoch milliseconds. The age
    // cutoff goes into the query rather than a later filter so that `LIMIT`
    // counts only sessions that can actually be offered.
    const QUERY: &str = "SELECT id, directory, title, time_updated \
                         FROM session \
                         WHERE parent_id IS NULL AND time_archived IS NULL \
                         AND time_updated >= ?2 \
                         ORDER BY time_updated DESC LIMIT ?1";
    let cutoff_ms = newer_than
        .and_then(|cutoff| cutoff.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis().min(i64::MAX as u128) as i64);

    let mut read = || -> rusqlite::Result<()> {
        let conn = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        let mut stmt = conn.prepare(QUERY)?;
        let rows = stmt.query_map([limit as i64, cutoff_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let found: Vec<_> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        for (id, directory, title, updated) in found {
            if directory.is_empty() {
                continue;
            }
            // A session the user never named keeps the placeholder title, so fall
            // back to what they actually typed first.
            let label = match title.as_deref().and_then(opencode_title) {
                Some(title) => Some(title),
                None => opencode_first_prompt(&conn, &id),
            };
            out.push(Candidate::Ready(ready_session(
                "opencode",
                id,
                PathBuf::from(directory),
                // OpenCode records no branch per session.
                None,
                label,
                epoch_millis(updated),
            )));
        }
        Ok(())
    };
    if let Err(err) = read() {
        // A locked, missing or newer-schema database simply means OpenCode
        // contributes nothing, exactly like an agent that is not installed.
        log::debug!("opencode session scan skipped: {err:#}");
    }
}

/// First thing the user typed in an OpenCode session.
///
/// Message and part payloads are opaque JSON blobs in the schema, so the shape
/// is inspected in Rust rather than with SQL JSON functions — one less thing to
/// depend on. Both queries are index-backed and bounded to the opening few rows;
/// this only runs for sessions that never earned a title.
fn opencode_first_prompt(conn: &rusqlite::Connection, session_id: &str) -> Option<String> {
    const FIRST_MESSAGES: &str = "SELECT id, data FROM message \
                                  WHERE session_id = ?1 \
                                  ORDER BY time_created, id LIMIT 4";
    const FIRST_PARTS: &str = "SELECT data FROM part \
                               WHERE message_id = ?1 \
                               ORDER BY time_created, id LIMIT 8";

    let mut messages = conn.prepare(FIRST_MESSAGES).ok()?;
    let candidates: Vec<(String, String)> = messages
        .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?
        .flatten()
        .collect();
    drop(messages);

    for (message_id, data) in candidates {
        let value = serde_json::from_str::<serde_json::Value>(&data).ok();
        let is_user = value
            .as_ref()
            .and_then(|value| value.get("role"))
            .and_then(|role| role.as_str())
            == Some("user");
        if !is_user {
            continue;
        }
        let mut parts = match conn.prepare(FIRST_PARTS) {
            Ok(parts) => parts,
            Err(_) => return None,
        };
        let texts: Vec<String> = match parts.query_map([&message_id], |row| row.get::<_, String>(0))
        {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => return None,
        };
        drop(parts);
        for part in texts {
            let Ok(part) = serde_json::from_str::<serde_json::Value>(&part) else {
                continue;
            };
            if part.get("type").and_then(|kind| kind.as_str()) != Some("text") {
                continue;
            }
            if let Some(text) = part.get("text").and_then(|text| text.as_str()) {
                // Floor of 1: there is exactly one candidate here, so even a
                // two-word prompt beats saying nothing.
                if let Some(label) = transcript::describe_prompt_min(text, 1) {
                    return Some(label);
                }
            }
        }
        // The first user message had no usable text; later ones are replies, not
        // the opening ask.
        return None;
    }
    None
}

/// Reject OpenCode's placeholder titles.
///
/// A session that was never summarized is titled `New session - <timestamp>`,
/// which is worse than saying nothing: it fills the row with a date the row
/// already sorts by.
fn opencode_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || title.starts_with("New session - ") {
        return None;
    }
    Some(transcript::trim_to_words(title, 10))
}

/// Epoch milliseconds to `SystemTime`, clamping anything absurd to the epoch so
/// a bad row sorts last instead of panicking.
fn epoch_millis(millis: i64) -> SystemTime {
    std::convert::TryInto::<u64>::try_into(millis)
        .ok()
        .map(|millis| std::time::UNIX_EPOCH + std::time::Duration::from_millis(millis))
        .unwrap_or(std::time::UNIX_EPOCH)
}

// ---------------------------------------------------------------------------
// GitHub Copilot CLI
// ---------------------------------------------------------------------------

/// Enumerate `~/.copilot/session-state/<sessionId>/`.
///
/// GitHub documents both this directory and `copilot --resume <SESSION-ID>`, and
/// each session keeps a small `workspace.yaml` beside its (large) `events.jsonl`
/// holding the directory, branch and a generated name. Reading the metadata file
/// rather than the event log keeps this to a few hundred bytes per session.
fn collect_copilot_candidates(home: &Path, out: &mut Vec<Candidate>) {
    let root = home.join(".copilot").join("session-state");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(session_id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let workspace = dir.join("workspace.yaml");
        // `copilot --resume` is known to leave behind empty session directories
        // with fresh ids; without metadata there is nothing to show or resume.
        let Ok(metadata) = std::fs::metadata(&workspace) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        // Deferred rather than read here: there are hundreds of these
        // directories, and only the newest few ever reach a menu row.
        out.push(Candidate::Deferred {
            adapter_id: "copilot",
            session_id: session_id.to_string(),
            path: workspace,
            modified,
        });
    }
}

/// Parse the handful of scalars needed out of a Copilot `workspace.yaml`.
///
/// Deserialized into a struct of `Option`s so that unknown, added or reordered
/// keys are ignored rather than failing the read — the file's schema is not
/// documented even though its location is.
fn read_copilot_workspace(path: &Path) -> Result<SessionDetails, Skip> {
    #[derive(serde::Deserialize)]
    struct Raw {
        cwd: Option<String>,
        git_root: Option<String>,
        branch: Option<String>,
        name: Option<String>,
    }

    let text = std::fs::read_to_string(path).map_err(|_| Skip::Unreadable)?;
    let raw: Raw = serde_yaml::from_str(&text).map_err(|_| Skip::Unreadable)?;
    let cwd = raw
        .cwd
        .or(raw.git_root)
        .map(PathBuf::from)
        .filter(|cwd| cwd.as_os_str().len() > 0)
        .ok_or(Skip::NoCwd)?;
    Ok(SessionDetails {
        cwd,
        git_branch: raw.branch.filter(|branch| !branch.trim().is_empty()),
        label: raw
            .name
            .map(|name| transcript::trim_to_words(name.trim(), 10))
            .filter(|name| !name.is_empty()),
        // The directory name is the session id.
        session_id: None,
    })
}

/// True for Codex's synthetic first turn that replays project instructions.
fn is_codex_instruction_blob(text: &str) -> bool {
    let head = text.trim_start();
    head.starts_with("# AGENTS.md")
        || head.starts_with("<INSTRUCTIONS>")
        || head.contains("# AGENTS.md instructions")
}

// ---------------------------------------------------------------------------
// Subagent tree building
// ---------------------------------------------------------------------------

/// Build recursive tree from flat subagent list.
///
/// Depth 0 = roots. Children follow their parent contiguously in the flat list.
pub fn build_subagent_tree(flat: &[super::HerdSubagent]) -> Vec<SubagentNode> {
    let mut nodes: Vec<SubagentNode> = flat
        .iter()
        .map(|sub| SubagentNode {
            agent_id: sub.agent_id.clone(),
            agent_type: sub.agent_type.clone(),
            description: sub.description.clone(),
            status: sub.status,
            depth: sub.depth,
            children: Vec::new(),
            events: Vec::new(),
        })
        .collect();
    assemble_tree(&mut nodes, 0)
}

/// Consume nodes from the front of the slice, building a tree at the given depth.
///
/// Each node's children are the contiguous run of following nodes at `depth + 1`.
fn assemble_tree(nodes: &mut Vec<SubagentNode>, depth: u32) -> Vec<SubagentNode> {
    let mut result = Vec::new();
    while nodes.first().map_or(false, |n| n.depth == depth) {
        let mut node = nodes.remove(0);
        node.children = assemble_tree(nodes, depth + 1);
        result.push(node);
    }
    result
}

/// Link events to subagent tree nodes by parent_id.
///
/// Events with parent_id = Some(agent_id) go into that node's events.
/// Events with parent_id = None stay in top-level HerdActivity.recent.
pub fn link_events_to_tree(tree: &mut Vec<SubagentNode>, events: Vec<HerdEvent>) {
    for event in events {
        if let Some(parent_id) = &event.parent_id {
            if let Some(node) = tree.iter_mut().find_map(|root| root.find_mut(parent_id)) {
                node.events.push(event);
                continue;
            }
        }
    }
}

/// Read subagent transcript events from disk.
///
/// Path: `<project_path>/<session_id>/subagents/<agent_id>.jsonl`
/// Returns events with parent_id = Some(agent_id).
pub fn read_subagent_transcript(
    project_path: &Path,
    session_id: &str,
    agent_id: &str,
) -> Vec<HerdEvent> {
    let path = project_path
        .join(session_id)
        .join("subagents")
        .join(format!("{agent_id}.jsonl"));
    if !path.is_file() {
        return Vec::new();
    }
    let mut events = Vec::new();
    for line in transcript::tail_lines(&path, transcript::HEAD_LINES * 2) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let at = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map(SystemTime::from);
        let Some(blocks) = value
            .get("message")
            .and_then(|msg| msg.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("tool_use") => {
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    events.push(HerdEvent {
                        at,
                        kind: HerdEventKind::Tool,
                        content: HerdContent::SingleLine(name),
                        tool_use_id: block.get("id").and_then(|v| v.as_str()).map(str::to_string),
                        parent_id: Some(agent_id.to_string()),
                    });
                }
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !text.is_empty() {
                        events.push(HerdEvent {
                            at,
                            kind: HerdEventKind::Assistant,
                            content: HerdContent::MultiLine(text),
                            tool_use_id: None,
                            parent_id: Some(agent_id.to_string()),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    events
}

/// Populate HerdActivity.subagent_tree from flat subagents list.
///
/// Builds tree, reads each subagent transcript, links events.
pub fn populate_subagent_tree(
    activity: &mut HerdActivity,
    project_path: &Path,
    session_id: &str,
    flat_subagents: &[super::HerdSubagent],
) {
    let mut tree = build_subagent_tree(flat_subagents);
    for sub in flat_subagents {
        let events = read_subagent_transcript(project_path, session_id, &sub.agent_id);
        link_events_to_tree(&mut tree, events);
    }
    activity.subagent_tree = tree;
}

/// Read a vendor's primary session file, then its bounded on-disk artifact
/// search path. Vendor stores disagree on whether activity lives beside the
/// session record, in JSONL rollouts, or in per-message JSON files.
pub fn activity_from_session_files(
    primary: &Path,
    search_root: &Path,
    session_id: &str,
) -> Option<HerdActivity> {
    let direct = transcript::read_generic_activity(primary, 8);
    if !direct.is_empty() {
        return Some(direct);
    }
    if session_id.is_empty() {
        return None;
    }
    let artifact = find_session_artifact(search_root, session_id)?;
    let activity = transcript::read_generic_activity(&artifact, 8);
    (!activity.is_empty()).then_some(activity)
}

/// Search a vendor's session store for the newest artifact whose file name
/// contains `session_id`. Exposed beyond this module so vendor-specific
/// `transcript_source` lookups (see `agent_herd::transcript_source`) can reuse
/// the same search instead of re-walking the tree themselves.
pub(crate) fn find_session_artifact(root: &Path, session_id: &str) -> Option<PathBuf> {
    const MAX_FILES: usize = 2048;
    let mut stack = vec![root.to_path_buf()];
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_FILES {
                break;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_json = matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("json" | "jsonl")
            );
            let has_id = path
                .file_name()
                .map(|name| name.to_string_lossy().contains(session_id))
                .unwrap_or(false);
            if !is_json || !has_id {
                continue;
            }
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if newest
                .as_ref()
                .is_none_or(|(previous, _)| modified > *previous)
            {
                newest = Some((modified, path));
            }
        }
    }
    newest.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Stamp an mtime so ordering tests do not depend on write speed.
    fn set_mtime(path: &Path, secs: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn claude_session(home: &Path, project: &str, id: &str, lines: &[String], mtime: u64) {
        let path = home
            .join(".claude")
            .join("projects")
            .join(project)
            .join(format!("{id}.jsonl"));
        write(&path, &format!("{}\n", lines.join("\n")));
        set_mtime(&path, mtime);
    }

    fn user_line(cwd: &str, branch: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "gitBranch": branch,
            "message": { "content": [{ "type": "text", "text": text }] },
        })
        .to_string()
    }

    fn title_line(title: &str) -> String {
        serde_json::json!({ "type": "ai-title", "aiTitle": title }).to_string()
    }

    #[test]
    fn ai_title_wins_over_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-1111",
            &[
                user_line("/repo", "main", "please do the thing with the stuff"),
                title_line("Add resume menu to sidebar"),
            ],
            1_000,
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "Add resume menu to sidebar");
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
        assert_eq!(sessions[0].project, "repo");
    }

    #[test]
    fn sessions_older_than_the_cutoff_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-old",
            "aaaa-0001",
            &[user_line("/old", "main", "something from last quarter")],
            1_000,
        );
        claude_session(
            dir.path(),
            "-new",
            "aaaa-0002",
            &[user_line("/new", "main", "something from this morning")],
            5_000,
        );
        let homes = [dir.path().to_path_buf()];

        let cutoff = UNIX_EPOCH + Duration::from_secs(2_000);
        let recent = collect_recent_sessions_since(&homes, 10, Some(cutoff));
        assert_eq!(
            recent
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aaaa-0002"]
        );

        // A session touched exactly at the cutoff is still inside the window.
        let at_cutoff = UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(
            collect_recent_sessions_since(&homes, 10, Some(at_cutoff)).len(),
            2
        );
        assert_eq!(collect_recent_sessions_since(&homes, 10, None).len(), 2);
    }

    #[test]
    fn a_transcript_is_reread_only_when_its_mtime_moves() {
        let dir = tempfile::tempdir().unwrap();
        let session = |title: &str, mtime: u64| {
            claude_session(
                dir.path(),
                "-repo",
                "aaaa-cafe",
                &[
                    user_line("/repo", "main", "please do the thing with the stuff"),
                    title_line(title),
                ],
                mtime,
            )
        };

        session("First title of this session", 1_000);
        assert_eq!(
            collect_recent_sessions(dir.path(), 10)[0].label,
            "First title of this session"
        );

        // Same mtime: the cached head read answers, the file is not reopened.
        session("Second title of this session", 1_000);
        assert_eq!(
            collect_recent_sessions(dir.path(), 10)[0].label,
            "First title of this session"
        );

        // Written again: the new content is read.
        session("Second title of this session", 2_000);
        assert_eq!(
            collect_recent_sessions(dir.path(), 10)[0].label,
            "Second title of this session"
        );
    }

    #[test]
    fn ai_title_is_found_far_into_the_head() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec![user_line("/repo", "main", "kick things off here please")];
        // Real transcripts bury the title behind a couple of hundred lines of
        // hook output and tool results.
        for _ in 0..240 {
            lines.push(serde_json::json!({ "type": "attachment" }).to_string());
        }
        lines.push(title_line("Buried title still found"));
        claude_session(dir.path(), "-repo", "aaaa-2222", &lines, 1_000);

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions[0].label, "Buried title still found");
    }

    #[test]
    fn falls_back_to_the_first_real_prompt() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-3333",
            &[
                // Injected context, a slash command, then the real ask.
                serde_json::json!({
                    "type": "user",
                    "cwd": "/repo",
                    "gitBranch": "main",
                    "isMeta": true,
                    "message": { "content": [{ "type": "text", "text": "Caveat: local commands" }] },
                })
                .to_string(),
                user_line(
                    "/repo",
                    "main",
                    "<command-name>/plan</command-name><command-args>make the sidebar resume old sessions for me</command-args>",
                ),
            ],
            1_000,
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(
            sessions[0].label,
            "make the sidebar resume old sessions for me"
        );
    }

    #[test]
    fn system_reminders_are_stripped_from_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-4444",
            &[user_line(
                "/repo",
                "main",
                "<system-reminder>ignore this entirely</system-reminder>fix the broken hitbox on the pill",
            )],
            1_000,
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions[0].label, "fix the broken hitbox on the pill");
    }

    #[test]
    fn labels_are_trimmed_to_ten_words() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-5555",
            &[user_line(
                "/repo",
                "main",
                "one two three four five six seven eight nine ten eleven twelve",
            )],
            1_000,
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(
            sessions[0].label,
            "one two three four five six seven eight nine ten…"
        );
    }

    #[test]
    fn branch_prefix_only_for_interesting_branches() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-6666",
            &[user_line(
                "/repo",
                "main",
                "work on the default branch here",
            )],
            2_000,
        );
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-7777",
            &[user_line(
                "/repo",
                "feat/sidebar",
                "work on a feature branch here",
            )],
            1_000,
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(
            sessions[0].menu_label(),
            "repo · work on the default branch here"
        );
        assert_eq!(
            sessions[1].menu_label(),
            "[feat/sidebar] repo · work on a feature branch here"
        );
    }

    #[test]
    fn sessions_sort_newest_first_across_projects_and_limit_truncates() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-one",
            "aaaa-8888",
            &[user_line("/one", "main", "oldest session in project one")],
            1_000,
        );
        claude_session(
            dir.path(),
            "-two",
            "aaaa-9999",
            &[user_line("/two", "main", "newest session in project two")],
            3_000,
        );
        claude_session(
            dir.path(),
            "-one",
            "bbbb-0000",
            &[user_line("/one", "main", "middle session in project one")],
            2_000,
        );

        let all = collect_recent_sessions(dir.path(), 10);
        assert_eq!(
            all.iter().map(|s| s.project.as_str()).collect::<Vec<_>>(),
            vec!["two", "one", "one"]
        );

        let limited = collect_recent_sessions(dir.path(), 2);
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].session_id, "aaaa-9999");
    }

    #[test]
    fn subagent_subdirectory_is_not_a_session() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-1234",
            &[user_line("/repo", "main", "the one real session here")],
            1_000,
        );
        // Claude keeps subagent transcripts under `<sessionId>/subagents/`.
        write(
            &dir.path()
                .join(".claude/projects/-repo/aaaa-1234/subagents/agent-1.jsonl"),
            "{}\n",
        );

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "aaaa-1234");
    }

    #[test]
    fn a_session_without_a_cwd_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-4321",
            &[title_line("Has a title but no directory")],
            1_000,
        );

        assert!(collect_recent_sessions(dir.path(), 10).is_empty());
    }

    #[test]
    fn implausible_session_ids_are_rejected() {
        assert!(session_id_is_sane("019f285e-99fc-7313-864a-a31552fff7bc"));
        assert!(session_id_is_sane("abc_123"));
        assert!(!session_id_is_sane(""));
        assert!(!session_id_is_sane("../../etc/passwd"));
        assert!(!session_id_is_sane("has space"));
        assert!(!session_id_is_sane("--flag"));
        assert!(!session_id_is_sane(&"x".repeat(MAX_SESSION_ID_LEN + 1)));
    }

    #[test]
    fn codex_rollouts_are_found_and_labelled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(".codex/sessions/2026/07/03/rollout-2026-07-03T16-25-16-019f285e.jsonl");
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": { "session_id": "019f285e-99fc-7313-864a-a31552fff7bc", "cwd": "/repo" },
        });
        let agents = serde_json::json!({
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "# AGENTS.md instructions\nblah" },
        });
        let real = serde_json::json!({
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "fix the sidebar pill hitbox jumping" },
        });
        write(&path, &format!("{meta}\n{agents}\n{real}\n"));
        set_mtime(&path, 5_000);

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].adapter_id, "codex");
        assert_eq!(
            sessions[0].session_id,
            "019f285e-99fc-7313-864a-a31552fff7bc"
        );
        assert_eq!(sessions[0].label, "fix the sidebar pill hitbox jumping");
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo"));
    }

    fn entrypoint_line(cwd: &str, entrypoint: &str, text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "gitBranch": "HEAD",
            "entrypoint": entrypoint,
            "message": { "content": [{ "type": "text", "text": text }] },
        })
        .to_string()
    }

    /// `claude -p` runs and Agent SDK programs (claude-mem's observer, a
    /// session-note hook) are not offered; terminal and desktop sessions are,
    /// and so is a transcript too old to record an entrypoint at all.
    #[test]
    fn headless_claude_sessions_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            ("aaaa-0c11", Some("cli"), 1_000),
            ("aaaa-0de5", Some("claude-desktop"), 2_000),
            ("aaaa-5dc1", Some("sdk-cli"), 3_000),
            ("aaaa-5d75", Some("sdk-ts"), 4_000),
            ("aaaa-01d0", None, 5_000),
        ];
        for (id, entrypoint, mtime) in cases {
            let line = match entrypoint {
                Some(entrypoint) => entrypoint_line("/repo", entrypoint, "work on the resume menu"),
                None => user_line("/repo", "main", "work on the resume menu"),
            };
            claude_session(dir.path(), "-repo", id, &[line], mtime);
        }

        let mut offered: Vec<_> = collect_recent_sessions(dir.path(), 10)
            .into_iter()
            .map(|session| session.session_id)
            .collect();
        offered.sort();
        assert_eq!(offered, vec!["aaaa-01d0", "aaaa-0c11", "aaaa-0de5"]);
    }

    /// The desktop app's scheduled tasks start as `claude-desktop`, exactly
    /// like a person's session; only the prompt says it was a cron run.
    #[test]
    fn scheduled_task_runs_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let scheduled = "<scheduled-task name=\"team-jira-sweep\" \
            file=\"/Users/me/.claude/scheduled-tasks/team-jira-sweep/SKILL.md\">\n\
            This is an automated run of a scheduled task. The user is not present.";
        let queued = serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "content": scheduled,
        })
        .to_string();
        // Queue record plus first turn, as the desktop app writes it.
        claude_session(
            dir.path(),
            "-chat",
            "aaaa-c201",
            &[
                queued,
                entrypoint_line("/chat", "claude-desktop", scheduled),
            ],
            1_000,
        );
        // First turn only.
        claude_session(
            dir.path(),
            "-chat",
            "aaaa-c202",
            &[entrypoint_line("/chat", "claude-desktop", scheduled)],
            2_000,
        );
        // A person in the desktop app, mentioning the tag in passing.
        claude_session(
            dir.path(),
            "-chat",
            "aaaa-0de5",
            &[entrypoint_line(
                "/chat",
                "claude-desktop",
                "why does my <scheduled-task> sweep keep failing",
            )],
            3_000,
        );

        let offered: Vec<_> = collect_recent_sessions(dir.path(), 10)
            .into_iter()
            .map(|session| session.session_id)
            .collect();
        assert_eq!(offered, vec!["aaaa-0de5"]);
    }

    #[test]
    fn headless_claude_sessions_keep_their_label_in_the_herd() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-vault",
            "aaaa-5dc1",
            &[entrypoint_line(
                "/vault",
                "sdk-cli",
                "write one session note please",
            )],
            1_000,
        );
        let path = dir.path().join(".claude/projects/-vault/aaaa-5dc1.jsonl");
        assert_eq!(
            claude_transcript_label(&path).as_deref(),
            Some("write one session note please")
        );
    }

    #[test]
    fn a_detached_head_is_not_shown_as_a_branch() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-4ead",
            &[entrypoint_line(
                "/repo",
                "cli",
                "look at the detached checkout",
            )],
            1_000,
        );
        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(
            sessions[0].menu_label(),
            "repo · look at the detached checkout"
        );
    }

    /// Subagent threads, `codex exec` and MCP-served sessions are not offered.
    #[test]
    fn headless_codex_rollouts_are_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let sources = [
            (
                "019f0001-0000-0000-0000-000000000001",
                serde_json::json!("cli"),
            ),
            (
                "019f0001-0000-0000-0000-000000000002",
                serde_json::json!("vscode"),
            ),
            (
                "019f0001-0000-0000-0000-000000000003",
                serde_json::json!("exec"),
            ),
            (
                "019f0001-0000-0000-0000-000000000004",
                serde_json::json!("mcp"),
            ),
            (
                "019f0001-0000-0000-0000-000000000005",
                serde_json::json!({ "subagent": { "other": "guardian" } }),
            ),
        ];
        for (index, (id, source)) in sources.iter().enumerate() {
            let path = dir
                .path()
                .join(format!(".codex/sessions/2026/07/03/rollout-{index}.jsonl"));
            let meta = serde_json::json!({
                "type": "session_meta",
                "payload": { "session_id": id, "cwd": "/repo", "source": source },
            });
            let prompt = serde_json::json!({
                "type": "event_msg",
                "payload": { "type": "user_message", "message": "fix the sidebar pill hitbox" },
            });
            write(&path, &format!("{meta}\n{prompt}\n"));
            set_mtime(&path, 1_000 + index as u64);
        }

        let mut offered: Vec<_> = collect_recent_sessions(dir.path(), 10)
            .into_iter()
            .map(|session| session.session_id)
            .collect();
        offered.sort();
        assert_eq!(
            offered,
            vec![
                "019f0001-0000-0000-0000-000000000001",
                "019f0001-0000-0000-0000-000000000002",
            ]
        );
    }

    /// Codex opens a new rollout file each time a session is resumed, so the
    /// same `session_id` shows up in several files. Only the newest may be
    /// offered — two rows resuming the same session is just noise.
    #[test]
    fn repeated_rollouts_of_one_codex_session_collapse_to_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": { "session_id": "019f285e-aaaa-bbbb-cccc-dddddddddddd", "cwd": "/repo" },
        });
        for (day, mtime, text) in [
            ("03", 4_000, "the older rollout of this session"),
            ("05", 6_000, "the newer rollout of this session"),
        ] {
            let path = dir
                .path()
                .join(".codex")
                .join("sessions")
                .join("2026")
                .join("07")
                .join(day)
                .join(format!("rollout-2026-07-{day}T10-00-00-019f285e.jsonl"));
            let msg = serde_json::json!({
                "type": "event_msg",
                "payload": { "type": "user_message", "message": text },
            });
            write(&path, &format!("{meta}\n{msg}\n"));
            set_mtime(&path, mtime);
        }

        let sessions = collect_recent_sessions(dir.path(), 10);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "the newer rollout of this session");
    }

    #[test]
    fn a_missing_store_yields_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(collect_recent_sessions(dir.path(), 10).is_empty());
    }

    #[test]
    fn scan_stats_say_why_a_session_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-0001",
            &[user_line("/repo", "main", "a session somebody typed into")],
            4_000,
        );
        let headless = serde_json::json!({
            "type": "user",
            "cwd": "/repo",
            "entrypoint": "sdk-cli",
            "message": { "content": "summarise the observer queue please" },
        });
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-0002",
            &[headless.to_string()],
            3_000,
        );
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-0003",
            &[title_line("Has a title but no directory")],
            2_000,
        );
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-0004",
            &["not json".into()],
            1_500,
        );
        // Codex takes its id from inside the file, so only there can a
        // filename-safe candidate still carry an id argv must never see.
        let path = dir
            .path()
            .join(".codex/sessions/2026/07/03/rollout-2026-07-03T16-25-16-bad.jsonl");
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": { "session_id": "--dangerously-skip-permissions", "cwd": "/repo" },
        });
        write(&path, &format!("{meta}\n"));
        set_mtime(&path, 1_000);

        let (sessions, stats) = scan_recent_sessions(&[dir.path().to_path_buf()], 10, None);
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            stats,
            SessionScanStats {
                candidates: 5,
                listed: 1,
                headless: 1,
                no_cwd: 1,
                unreadable: 1,
                bad_id: 1,
                duplicates: 0,
                reads: 5,
            }
        );

        // A rescan with nothing written reads nothing, and still explains
        // every row it did not list.
        let (_, again) = scan_recent_sessions(&[dir.path().to_path_buf()], 10, None);
        assert_eq!(again, SessionScanStats { reads: 0, ..stats });
    }

    #[test]
    fn zero_limit_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        claude_session(
            dir.path(),
            "-repo",
            "aaaa-0001",
            &[user_line("/repo", "main", "should never be read at all")],
            1_000,
        );
        assert!(collect_recent_sessions(dir.path(), 0).is_empty());
    }
}
