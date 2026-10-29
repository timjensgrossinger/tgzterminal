use crate::agent_herd::claude::session_is_live;
use crate::agent_herd::vendor::{
    AgentVendor, SessionOrigin, SessionRoot, SessionSource, VendorSession,
};
use crate::agent_herd::{
    HerdActivity, HerdContent, HerdEvent, HerdEventKind, HerdStatus, HerdSubagent, TurnState,
};
use rusqlite::{Connection, OpenFlags};
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const OPENCODE_ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
const OPENCODE_WORKING_WINDOW: Duration = Duration::from_secs(2 * 60);

/// Which of the two known store layouts `opencode.db` uses.
///
/// OpenCode 2.x (and the desktop app) renamed `session` to `session_v2` and
/// replaced the per-part `part` table with whole-message rows in
/// `session_message`, each carrying its parts in the row's JSON. The column
/// names the herd needs (`id`, `directory`, `title`, `model`, `cost`,
/// `tokens_input`, `tokens_output`, `time_created`, `time_updated`,
/// `time_archived`, `parent_id`) survive the rename verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpencodeSchema {
    /// `session` + `part` (OpenCode 1.x CLI).
    Legacy,
    /// `session_v2` + `session_message` (OpenCode 2.x CLI / desktop app).
    V2,
}

impl OpencodeSchema {
    pub(crate) fn sessions_table(self) -> &'static str {
        match self {
            Self::Legacy => "session",
            Self::V2 => "session_v2",
        }
    }

    pub(crate) fn events_table(self) -> &'static str {
        match self {
            Self::Legacy => "part",
            Self::V2 => "session_message",
        }
    }

    /// Secondary sort of the event rows: legacy `part` has an `id`, v2
    /// `session_message` rows carry a `seq`.
    fn events_order(self) -> &'static str {
        match self {
            Self::Legacy => "id DESC",
            Self::V2 => "seq DESC",
        }
    }
}

pub(crate) fn detect_opencode_schema(conn: &Connection) -> Option<OpencodeSchema> {
    let has_v2 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'table' AND name IN ('session_v2', 'session_message')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .unwrap_or(0)
        >= 2;
    if has_v2 {
        return Some(OpencodeSchema::V2);
    }
    let has_legacy = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master \
             WHERE type = 'table' AND name = 'session'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .unwrap_or(0)
        > 0;
    has_legacy.then_some(OpencodeSchema::Legacy)
}

/// Pull part-like JSON objects out of one stored row.
///
/// Legacy `part` rows are the part payload itself. v2 `session_message` rows
/// are whole messages whose parts sit in an array (observed both as a bare
/// array and as an object with a `parts` field), so every shape is tolerated
/// rather than coupled to one build.
pub(crate) fn part_objects<'a>(data: &'a serde_json::Value) -> Vec<&'a serde_json::Value> {
    if let Some(array) = data.as_array() {
        return array.iter().collect();
    }
    if let Some(parts) = data.get("parts").and_then(|parts| parts.as_array()) {
        return parts.iter().collect();
    }
    if data.get("type").is_some() {
        return vec![data];
    }
    Vec::new()
}

fn opencode_config_dir(home: &Path) -> PathBuf {
    home.join(".config").join("opencode")
}

pub(crate) fn opencode_db(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db")
}

fn session_files(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "json"))
        .collect()
}

pub struct OpenCodeDetector;

impl SessionSource for OpenCodeDetector {
    fn vendor(&self) -> AgentVendor {
        AgentVendor::OpenCode
    }

    fn collect_sessions(&self, root: &SessionRoot) -> Vec<VendorSession> {
        let home = root.home.as_path();
        let mut sessions = collect_database_sessions_cached(home);
        if !sessions.is_empty() {
            return sessions;
        }

        // Keep support for older OpenCode builds that wrote one JSON file per
        // live process. New builds use SQLite, but this fallback costs nothing.
        let dir = opencode_config_dir(home);
        let files = session_files(&dir);
        sessions = Vec::new();
        for file in files {
            if let Ok(data) = std::fs::read_to_string(&file) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                    let pid = match json.get("pid").and_then(|v| v.as_u64()) {
                        Some(pid) if pid <= u32::MAX as u64 => pid as u32,
                        // No usable pid means we cannot verify liveness; skip
                        // this session rather than show a phantom row.
                        _ => continue,
                    };
                    if !session_is_live(root, pid, &file) {
                        continue;
                    }
                    let session_id = json
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let cwd = json
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| dir.clone());
                    let name = json
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let activity = crate::agent_herd::sessions::activity_from_session_files(
                        &file,
                        &home.join(".local/share/opencode/storage"),
                        &session_id,
                    );
                    let status = json
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(|s| match s {
                            "busy" | "running" | "thinking" => Some(HerdStatus::Working),
                            "idle" | "done" => Some(HerdStatus::Idle),
                            "waiting" => Some(HerdStatus::Blocked),
                            _ => None,
                        })
                        .unwrap_or(HerdStatus::Unknown);
                    sessions.push(VendorSession {
                        origin: SessionOrigin::Host,
                        home: None,
                        pane_hint: None,
                        pid,
                        // This store does not distinguish harness-spawned
                        // sessions from interactive ones.
                        interactive: true,
                        vendor: AgentVendor::OpenCode,
                        // This store exposes no turn boundary; freshness is all it has.
                        turn: crate::agent_herd::TurnState::Unknown,
                        session_id,
                        cwd,
                        project_root: None,
                        name,
                        model: None,
                        status,
                        blocked_reason: None,
                        started_at: None,
                        status_changed_at: None,
                        subagents: Vec::new(),
                        activity,
                        input_tokens: None,
                        output_tokens: None,
                        cost: None,
                    });
                }
            }
        }
        sessions
    }
}

/// How long an unchanged database's answer may be reused. Only time moves the
/// result while the files stand still -- sessions age out of the active and
/// working windows -- and both windows are minutes long.
const OPENCODE_DB_CACHE_TTL: Duration = Duration::from_secs(30);

/// Size and mtime of the database and its write-ahead log.
type DbFingerprint = [(u64, Option<SystemTime>); 2];

fn db_fingerprint(db: &Path) -> Option<DbFingerprint> {
    let stamp = |path: &Path| {
        std::fs::metadata(path)
            .map(|meta| (meta.len(), meta.modified().ok()))
            .unwrap_or((0, None))
    };
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    let main = std::fs::metadata(db).ok()?;
    Some([(main.len(), main.modified().ok()), stamp(Path::new(&wal))])
}

/// [`collect_database_sessions`], reused while the database is unchanged.
///
/// Opening `opencode.db` replays its whole write-ahead log, and a store inside
/// WSL is read through the 9p share: with a 7 MB log that is ~16 s per open on
/// a slow machine, every scan, for a database nobody had written to in hours.
/// That stalled the whole herd scan, and with it how soon a newly started agent
/// of *any* vendor was seen and recorded for "Reopen last window".
fn collect_database_sessions_cached(home: &Path) -> Vec<VendorSession> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;

    static CACHE: Mutex<Option<HashMap<PathBuf, (DbFingerprint, Instant, Vec<VendorSession>)>>> =
        Mutex::new(None);

    let db = opencode_db(home);
    let Some(fingerprint) = db_fingerprint(&db) else {
        return Vec::new();
    };
    if let Some((cached_print, at, sessions)) = CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&db)
    {
        if *cached_print == fingerprint && at.elapsed() < OPENCODE_DB_CACHE_TTL {
            return sessions.clone();
        }
    }
    let sessions = collect_database_sessions(home);
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(db, (fingerprint, Instant::now(), sessions.clone()));
    sessions
}

/// Cost in US dollars and tokens (input + output) of every OpenCode session
/// updated at or after each of `since_ms` (Unix milliseconds), read from
/// `opencode.db`. A session's figures are its running totals, so one that
/// started yesterday and is still going counts whole. `None` when there is
/// no readable store.
///
/// Blocking SQLite read: call it off the GUI thread.
pub(crate) fn spend_since(home: &Path, since_ms: &[i64]) -> Option<Vec<(f64, u64)>> {
    let db = opencode_db(home);
    if !db.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()?;
    let schema = detect_opencode_schema(&conn)?;
    let sql = format!(
        "SELECT COALESCE(SUM(cost), 0.0), COALESCE(SUM(tokens_input + tokens_output), 0) \
         FROM {} WHERE time_updated >= ?1",
        schema.sessions_table()
    );
    since_ms
        .iter()
        .map(|since| {
            conn.query_row(&sql, [since], |row| {
                Ok((row.get::<_, f64>(0)?, row.get::<_, i64>(1)?))
            })
            .ok()
            .map(|(cost, tokens)| (cost.max(0.), u64::try_from(tokens).unwrap_or(0)))
        })
        .collect()
}

fn collect_database_sessions(home: &Path) -> Vec<VendorSession> {
    let db = opencode_db(home);
    if !db.is_file() {
        return Vec::new();
    }

    let Ok(conn) = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };
    let Some(schema) = detect_opencode_schema(&conn) else {
        log::debug!("opencode database has neither the session nor the session_v2 table");
        return Vec::new();
    };

    let now = SystemTime::now();
    let cutoff = now
        .checked_sub(OPENCODE_ACTIVE_WINDOW)
        .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);

    let sessions_sql = format!(
        "SELECT id, directory, title, model, cost, tokens_input, tokens_output, \
                time_created, time_updated \
         FROM {} \
         WHERE parent_id IS NULL AND time_archived IS NULL AND time_updated >= ?1 \
         ORDER BY time_updated DESC LIMIT 32",
        schema.sessions_table()
    );
    let mut stmt = match conn.prepare(&sessions_sql) {
        Ok(stmt) => stmt,
        Err(err) => {
            log::debug!("opencode database session query failed: {err:#}");
            return Vec::new();
        }
    };

    let rows = match stmt.query_map([cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            log::debug!("opencode database session query failed: {err:#}");
            return Vec::new();
        }
    };

    let mut sessions: Vec<VendorSession> = rows
        .filter_map(|row| {
            let (
                session_id,
                directory,
                title,
                model,
                cost,
                input_tokens,
                output_tokens,
                created,
                updated,
            ) = row.ok()?;
            if session_id.is_empty() || directory.is_empty() {
                return None;
            }
            let updated_at = epoch_millis(updated)?;
            let age = now.duration_since(updated_at).ok()?;
            // Derive status from the most recent part the session actually wrote,
            // not from the row's `time_updated` (which the UI touches on open):
            //   - newest part is a tool call  -> the agent is mid-action
            //   - newest part is text/reasoning/step-finish -> it has stopped
            // A session with no parts at all is just the start screen: idle.
            let last_part_type = last_stored_part_type(&conn, &schema, &session_id);
            let status = match last_part_type.as_deref() {
                None => HerdStatus::Idle,
                Some("tool") | Some("reasoning") if age <= OPENCODE_WORKING_WINDOW => {
                    HerdStatus::Working
                }
                _ => HerdStatus::Idle,
            };
            let name = title.as_deref().and_then(clean_title);
            let model = model.and_then(|raw| model_label(&raw));
            let started_at = epoch_millis(created);
            let cost = (cost > 0.0).then(|| format!("${cost:.4}"));
            let activity = opencode_activity(&conn, schema, &session_id);
            let turn = opencode_turn_state(&conn, schema, &session_id);
            Some(VendorSession {
                origin: SessionOrigin::Host,
                home: None,
                pane_hint: None,
                // OpenCode's current database has no process id. Binding falls back
                // to the unique cwd match, while pane detection still handles live
                // sessions whose database row is too old.
                pid: 0,
                interactive: true,
                vendor: AgentVendor::OpenCode,
                // The newest message's own sign-off (see `opencode_turn_state`).
                turn,
                session_id,
                cwd: PathBuf::from(directory),
                project_root: None,
                name,
                model,
                status,
                blocked_reason: None,
                started_at,
                status_changed_at: Some(updated_at),
                subagents: Vec::new(),
                activity,
                input_tokens: u64_count(input_tokens),
                output_tokens: u64_count(output_tokens),
                cost,
            })
        })
        .collect();
    drop(stmt);
    attach_child_sessions(&conn, schema, &mut sessions, now);
    sessions
}

/// What the newest message of a session says about its turn.
///
/// OpenCode stamps an assistant message with `time.completed` when it is done
/// and with a `finish` reason: `tool-calls` means the model asked for tools
/// and the turn goes on, anything else (`stop`, `length`, `other`, ...) is
/// the turn over. An assistant message without `time.completed` is still
/// streaming; a user message is a prompt just submitted. Verified against the
/// 1.x `message` table; a v2 row is read the same way when it carries the
/// same fields, and gives `Unknown` otherwise.
pub(crate) fn turn_state_from_message(message: &serde_json::Value) -> TurnState {
    match message.get("role").and_then(|role| role.as_str()) {
        Some("assistant") => {
            let completed = message
                .get("time")
                .and_then(|time| time.get("completed"))
                .is_some_and(|completed| completed.is_number());
            if !completed {
                return TurnState::Working;
            }
            match message.get("finish").and_then(|finish| finish.as_str()) {
                Some("tool-calls") => TurnState::Working,
                _ => TurnState::Finished,
            }
        }
        Some("user") => TurnState::Working,
        _ => TurnState::Unknown,
    }
}

/// Whether a part is a `question` tool call still waiting on the human.
///
/// `question` is the one tool that cannot finish on its own. Permission asks
/// are not stored at all; those only reach us through the TUI's OSC 7501
/// program status.
fn is_pending_question(part: &serde_json::Value) -> bool {
    part.get("type").and_then(|kind| kind.as_str()) == Some("tool")
        && part.get("tool").and_then(|tool| tool.as_str()) == Some("question")
        && matches!(
            part.get("state")
                .and_then(|state| state.get("status"))
                .and_then(|status| status.as_str()),
            Some("pending") | Some("running")
        )
}

fn newest_rows(conn: &Connection, sql: &str, session_id: &str) -> Vec<serde_json::Value> {
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([session_id], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };
    rows.filter_map(Result::ok)
        .filter_map(|data| serde_json::from_str(&data).ok())
        .collect()
}

/// The session's turn, from its own records: a pending question holds it on
/// the human; otherwise the newest message's sign-off decides.
fn opencode_turn_state(conn: &Connection, schema: OpencodeSchema, session_id: &str) -> TurnState {
    match schema {
        OpencodeSchema::Legacy => {
            let parts = newest_rows(
                conn,
                "SELECT data FROM part WHERE session_id = ?1 \
                 ORDER BY time_created DESC LIMIT 8",
                session_id,
            );
            if parts.iter().any(is_pending_question) {
                return TurnState::AwaitingHuman;
            }
            newest_rows(
                conn,
                "SELECT data FROM message WHERE session_id = ?1 \
                 ORDER BY time_created DESC LIMIT 1",
                session_id,
            )
            .first()
            .map(turn_state_from_message)
            .unwrap_or_default()
        }
        OpencodeSchema::V2 => {
            let rows = newest_rows(
                conn,
                "SELECT data FROM session_message WHERE session_id = ?1 \
                 ORDER BY time_created DESC, seq DESC LIMIT 2",
                session_id,
            );
            if rows
                .iter()
                .any(|row| part_objects(row).into_iter().any(is_pending_question))
            {
                return TurnState::AwaitingHuman;
            }
            rows.first()
                .map(|row| row.get("info").unwrap_or(row))
                .map(turn_state_from_message)
                .unwrap_or_default()
        }
    }
}

/// `(@review-security subagent)` → `review-security`, with the title left
/// without the suffix.
fn split_subagent_title(title: &str) -> (Option<String>, String) {
    let title = title.trim();
    if let Some(start) = title.rfind("(@") {
        if let Some(rest) = title[start + 2..].strip_suffix(" subagent)") {
            let description = title[..start].trim();
            return (Some(rest.to_string()), description.to_string());
        }
    }
    (None, title.to_string())
}

/// Child sessions are OpenCode's subagents: its `task` tool runs each in a
/// session whose `parent_id` is the caller. Attach the direct children of
/// every listed session, so a parent keeps animating while they run.
///
/// A child whose own turn is still open is *pending* -- the OpenCode reading
/// of Claude's launched-but-not-handed-back -- so it counts as working
/// through a long silent tool call (see `subagent_is_working`).
fn attach_child_sessions(
    conn: &Connection,
    schema: OpencodeSchema,
    sessions: &mut [VendorSession],
    now: SystemTime,
) {
    if sessions.is_empty() {
        return;
    }
    let placeholders = vec!["?"; sessions.len()].join(", ");
    let sql = format!(
        "SELECT id, parent_id, title, time_updated FROM {} \
         WHERE time_archived IS NULL AND parent_id IN ({placeholders}) \
         ORDER BY time_created LIMIT 256",
        schema.sessions_table()
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return;
    };
    let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    }) else {
        return;
    };
    let children: Vec<(String, String, Option<String>, i64)> =
        rows.filter_map(Result::ok).collect();
    drop(stmt);

    for (id, parent_id, title, updated) in children {
        let Some(parent) = sessions.iter_mut().find(|s| s.session_id == parent_id) else {
            continue;
        };
        let last_activity = epoch_millis(updated);
        let recent = last_activity
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age <= OPENCODE_WORKING_WINDOW);
        let turn = opencode_turn_state(conn, schema, &id);
        let last_part = last_stored_part_type(conn, &schema, &id);
        let status = match turn {
            TurnState::Finished => HerdStatus::Done,
            TurnState::AwaitingHuman => HerdStatus::Blocked,
            _ if recent && matches!(last_part.as_deref(), Some("tool") | Some("reasoning")) => {
                HerdStatus::Working
            }
            _ => HerdStatus::Unknown,
        };
        let (agent_type, description) = split_subagent_title(title.as_deref().unwrap_or(""));
        parent.subagents.push(HerdSubagent {
            agent_id: id,
            agent_type: agent_type.unwrap_or_else(|| "subagent".to_string()),
            description,
            status,
            depth: 1,
            last_activity,
            pending: turn == TurnState::Working,
        });
    }
    for session in sessions.iter_mut() {
        if session.subagents.is_empty() {
            continue;
        }
        if let Some(activity) = session.activity.as_mut() {
            activity.subagent_tree =
                crate::agent_herd::sessions::build_subagent_tree(&session.subagents);
        }
    }
}

/// First thing the user typed in a v2 session (`session_message` rows embed
/// their parts). Shapes are tolerated rather than assumed: a row with no
/// role is read as long as its first text part is there.
pub(crate) fn first_prompt_v2(conn: &Connection, session_id: &str) -> Option<String> {
    let rows = {
        let mut stmt = conn
            .prepare(
                "SELECT data FROM session_message WHERE session_id = ?1 \
                 ORDER BY time_created, seq LIMIT 4",
            )
            .ok()?;
        let mapped = stmt
            .query_map([session_id], |row| row.get::<_, String>(0))
            .ok()?;
        mapped.filter_map(Result::ok).collect::<Vec<String>>()
    };
    for data in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        let info = value.get("info").unwrap_or(&value);
        match info.get("role").and_then(|role| role.as_str()) {
            Some("user") | None => {}
            Some(_) => continue,
        }
        let text = part_objects(&value).into_iter().find_map(|part| {
            (part.get("type").and_then(|kind| kind.as_str()) == Some("text"))
                .then(|| part.get("text").and_then(|text| text.as_str()))
                .flatten()
        });
        if let Some(label) =
            text.and_then(|text| crate::agent_herd::transcript::describe_prompt_min(text, 1))
        {
            return Some(label);
        }
    }
    None
}

/// What an OpenCode session (and its subagents' child sessions) recorded
/// about the files it changed, for the Changes panel.
pub enum FileRecord {
    /// Paths a `patch` part lists: OpenCode's own snapshot of the files a
    /// step changed, absolute.
    Changed(Vec<String>),
    /// An `edit`/`write` tool call's `filePath`.
    Edited(String),
    /// An `apply_patch` call's patch text.
    Patch(String),
}

fn file_records_of_part(part: &serde_json::Value, out: &mut Vec<FileRecord>) {
    match part.get("type").and_then(|kind| kind.as_str()) {
        Some("patch") => {
            let files: Vec<String> = part
                .get("files")
                .and_then(|files| files.as_array())
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|file| file.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !files.is_empty() {
                out.push(FileRecord::Changed(files));
            }
        }
        Some("tool") => {
            let input = part.get("state").and_then(|state| state.get("input"));
            match part.get("tool").and_then(|tool| tool.as_str()) {
                Some("edit") | Some("write") | Some("multiedit") => {
                    if let Some(path) = input
                        .and_then(|input| input.get("filePath"))
                        .and_then(|path| path.as_str())
                    {
                        out.push(FileRecord::Edited(path.to_string()));
                    }
                }
                Some("apply_patch") | Some("patch") => {
                    if let Some(text) = input
                        .and_then(|input| input.get("patchText"))
                        .and_then(|text| text.as_str())
                    {
                        out.push(FileRecord::Patch(text.to_string()));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Upper bound on the stored rows read for one session's file records.
const FILE_RECORD_ROWS: usize = 20_000;

/// File records of a session and of its direct child sessions, read from the
/// database under `home` (`None` = this machine). Blocking: call it off the
/// GUI thread.
pub fn file_records(home: Option<&Path>, session_id: &str) -> std::io::Result<Vec<FileRecord>> {
    let home = match home {
        Some(home) => home.to_path_buf(),
        None => dirs_home().ok_or_else(|| std::io::Error::other("no home directory"))?,
    };
    let db = opencode_db(&home);
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(std::io::Error::other)?;
    let schema = detect_opencode_schema(&conn)
        .ok_or_else(|| std::io::Error::other("unknown opencode database schema"))?;
    // Only the rows that can name a file: the JSON filters run inside SQLite
    // so the rest of a long session never crosses into Rust.
    let sql = match schema {
        OpencodeSchema::Legacy => format!(
            "SELECT data FROM part \
             WHERE (session_id = ?1 OR session_id IN \
                    (SELECT id FROM session WHERE parent_id = ?1)) \
             AND (json_extract(data, '$.type') = 'patch' \
                  OR json_extract(data, '$.tool') IN \
                     ('edit', 'write', 'multiedit', 'apply_patch', 'patch')) \
             ORDER BY time_created LIMIT {FILE_RECORD_ROWS}"
        ),
        OpencodeSchema::V2 => format!(
            "SELECT data FROM session_message \
             WHERE session_id = ?1 OR session_id IN \
                   (SELECT id FROM session_v2 WHERE parent_id = ?1) \
             ORDER BY time_created LIMIT {FILE_RECORD_ROWS}"
        ),
    };
    let mut stmt = conn.prepare(&sql).map_err(std::io::Error::other)?;
    let rows = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))
        .map_err(std::io::Error::other)?;
    let mut out = Vec::new();
    for data in rows.filter_map(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            continue;
        };
        for part in part_objects(&value) {
            file_records_of_part(part, &mut out);
        }
    }
    Ok(out)
}

/// The type of the most recently stored part of a session, as the JSON sees
/// it ("tool", "text", "reasoning", …).
///
/// Legacy `part` rows carry their type inside the row JSON; v2 message rows
/// are whole messages, so the newest row's parts are inspected last-first.
fn last_stored_part_type(
    conn: &Connection,
    schema: &OpencodeSchema,
    session_id: &str,
) -> Option<String> {
    match schema {
        OpencodeSchema::Legacy => {
            let value: Option<String> = conn
                .query_row(
                    "SELECT json_extract(data, '$.type') \
                     FROM part WHERE session_id = ?1 \
                     ORDER BY time_created DESC LIMIT 1",
                    [session_id],
                    |row| row.get(0),
                )
                .ok()
                .flatten();
            value
        }
        OpencodeSchema::V2 => {
            let rows: Vec<String> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT data FROM session_message WHERE session_id = ?1 \
                         ORDER BY time_created DESC, seq DESC LIMIT 4",
                    )
                    .ok()?;
                let mapped = stmt
                    .query_map([session_id], |row| row.get::<_, String>(0))
                    .ok()?;
                mapped.filter_map(Result::ok).collect()
            };
            for row in rows {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&row) else {
                    continue;
                };
                if let Some(last) = part_objects(&value)
                    .into_iter()
                    .rev()
                    .find_map(|part| part.get("type").and_then(|kind| kind.as_str()))
                {
                    return Some(last.to_string());
                }
            }
            None
        }
    }
}

fn epoch_millis(value: i64) -> Option<SystemTime> {
    let millis = u64::try_from(value).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

fn u64_count(value: i64) -> Option<u64> {
    u64::try_from(value).ok().filter(|value| *value > 0)
}

fn model_label(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    value
        .get("id")
        .or_else(|| value.get("modelID"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
}

fn opencode_activity(
    conn: &Connection,
    schema: OpencodeSchema,
    session_id: &str,
) -> Option<HerdActivity> {
    let events = opencode_recent_events(conn, schema, session_id, 64)?;
    // The detection path keeps a shallow window; the overlay reads deeper.
    let mut recent = events;
    if recent.len() > 8 {
        recent.drain(..recent.len() - 8);
    }
    let current = recent
        .last()
        .filter(|event| event.kind == HerdEventKind::Tool)
        .cloned();
    Some(HerdActivity {
        current,
        recent,
        subagent_tree: Vec::new(),
    })
}

/// Map one part-shaped JSON value into a `HerdEvent`.
///
/// Shared by both stores: the part payloads keep the same inner shape across
/// the legacy `part` rows and the parts array inside v2 message rows.
fn map_part_event(value: &serde_json::Value, at: SystemTime) -> Option<HerdEvent> {
    let kind = value.get("type").and_then(|kind| kind.as_str())?;
    match kind {
        "tool" => {
            let name = value
                .get("tool")
                .or_else(|| value.get("name"))
                .and_then(|tool| tool.as_str())
                .unwrap_or("tool")
                .to_string();
            let args = value
                .get("state")
                .and_then(|state| state.get("input"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Some(HerdEvent {
                at: Some(at),
                kind: HerdEventKind::Tool,
                content: HerdContent::ToolArgs { name, args },
                tool_use_id: value
                    .get("callID")
                    .or_else(|| value.get("id"))
                    .and_then(|id| id.as_str())
                    .map(str::to_string),
                parent_id: None,
            })
        }
        "text" | "reasoning" => {
            let Some(text) = value.get("text").and_then(|text| text.as_str()) else {
                return None;
            };
            if text.trim().is_empty() {
                return None;
            }
            let event_kind = if kind == "text" {
                HerdEventKind::Assistant
            } else {
                HerdEventKind::Thinking
            };
            Some(HerdEvent {
                at: Some(at),
                kind: event_kind,
                content: HerdContent::SingleLine(
                    text.split_whitespace().collect::<Vec<_>>().join(" "),
                ),
                tool_use_id: None,
                parent_id: None,
            })
        }
        _ => None,
    }
}

/// The time a part happened: the part's own `time.created` when the payload
/// carries one (v2 messages embed it), else the row's `time_created`.
fn part_event_at(part: &serde_json::Value, row_at: i64) -> SystemTime {
    part.get("time")
        .and_then(|time| time.get("created"))
        .and_then(|created| created.as_i64())
        .and_then(epoch_millis)
        .unwrap_or_else(|| epoch_millis(row_at).unwrap_or(SystemTime::UNIX_EPOCH))
}

/// The most recent events of a session, oldest first, capped to `cap` rows.
fn opencode_recent_events(
    conn: &Connection,
    schema: OpencodeSchema,
    session_id: &str,
    cap: usize,
) -> Option<Vec<HerdEvent>> {
    let sql = format!(
        "SELECT data, time_created FROM {} \
         WHERE session_id = ?1 ORDER BY time_created DESC, {} LIMIT {cap}",
        schema.events_table(),
        schema.events_order()
    );
    let mut stmt = conn.prepare(&sql).ok()?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .ok()?;
    let rows: Vec<(String, i64)> = rows.filter_map(Result::ok).collect();

    // Each row mapped to its events, newest row first.
    let per_row: Vec<Vec<HerdEvent>> = rows
        .into_iter()
        .filter_map(|(data, time)| {
            let value = serde_json::from_str::<serde_json::Value>(&data).ok()?;
            let events: Vec<HerdEvent> = match schema {
                OpencodeSchema::Legacy => {
                    let at = epoch_millis(time)?;
                    map_part_event(&value, at).into_iter().collect()
                }
                // A v2 row is a whole message whose JSON embeds its parts.
                OpencodeSchema::V2 => part_objects(&value)
                    .into_iter()
                    .filter_map(|part| map_part_event(part, part_event_at(part, time)))
                    .collect(),
            };
            (!events.is_empty()).then_some(events)
        })
        .collect();

    // Rows arrive newest-first; walk them oldest-first so the collected
    // vector is chronological and the parts inside each row stay in order.
    let mut events = Vec::new();
    for row_events in per_row.iter().rev() {
        events.extend(row_events.iter().cloned());
    }
    if events.len() > cap {
        events.drain(..events.len() - cap);
    }
    if events.is_empty() {
        return None;
    }
    Some(events)
}

/// Re-read a live OpenCode session's activity for the agent log overlay.
pub fn read_session_activity(session_id: &str, max_events: usize) -> Option<HerdActivity> {
    let home = dirs_home()?;
    let db = opencode_db(&home);
    if !db.is_file() {
        return None;
    }
    let Ok(conn) = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return None;
    };
    let schema = detect_opencode_schema(&conn)?;
    let mut activity = opencode_activity(&conn, schema, session_id)?;
    // The overlay wants a deep history; the detection path keeps a shallow 8.
    if max_events > activity.recent.len() {
        if let Some(mut ev) = opencode_recent_events(&conn, schema, session_id, max_events.max(256))
        {
            ev.truncate(max_events);
            activity.recent = ev;
            activity.current = activity
                .recent
                .last()
                .filter(|e| e.kind == HerdEventKind::Tool)
                .cloned();
        }
    }
    Some(activity)
}

/// The user's home directory.
///
/// `dirs_next::home_dir`, not `$HOME`: that variable is a unix convention and is
/// normally unset on Windows, where it would make every caller here silently
/// decide the user has no home -- so transcript lookup returned `None` for every
/// agent and the Log action was dead on that platform.
fn dirs_home() -> Option<PathBuf> {
    dirs_next::home_dir()
}

fn clean_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || title.starts_with("New session - ") {
        None
    } else {
        Some(crate::agent_herd::transcript::trim_to_words(title, 10))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn session_json(pid: u32) -> String {
        format!(r#"{{"pid":{pid},"session_id":"sess-{pid}","cwd":"/repo","status":"busy"}}"#)
    }

    #[test]
    fn sqlite_session_provides_identity_status_model_and_usage() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                parent_id TEXT,
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                model TEXT,
                cost REAL NOT NULL,
                tokens_input INTEGER NOT NULL,
                tokens_output INTEGER NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_archived INTEGER
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO session
             (id, project_id, directory, title, model, cost, tokens_input,
              tokens_output, time_created, time_updated)
             VALUES (?1, 'project', '/repo', 'Fix sidebar',
                     '{\"id\":\"gpt-5\"}', 0.125, 12, 34, ?2, ?2)",
            rusqlite::params!["session-1", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part
             (id, message_id, session_id, time_created, time_updated, data)
             VALUES ('part-1', 'message-1', 'session-1', ?1, ?1,
                     '{\"type\":\"tool\",\"tool\":\"opencode-mem\",\"state\":{\"input\":{\"query\":\"sidebar fixes\"}}}')",
            rusqlite::params![now],
        )
        .unwrap();
        drop(conn);

        let sessions = OpenCodeDetector.collect_sessions(&SessionRoot::host(temp.path()));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(sessions[0].name.as_deref(), Some("Fix sidebar"));
        assert_eq!(sessions[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(sessions[0].status, HerdStatus::Working);
        assert_eq!(sessions[0].input_tokens, Some(12));
        assert_eq!(sessions[0].output_tokens, Some(34));
        assert_eq!(sessions[0].cost.as_deref(), Some("$0.1250"));
        let spend = spend_since(temp.path(), &[0, now + 1]).unwrap();
        assert_eq!(spend[0], (0.125, 46));
        assert_eq!(spend[1], (0.0, 0));
        assert_eq!(
            sessions[0]
                .activity
                .as_ref()
                .and_then(|activity| activity.current.as_ref())
                .map(|event| event.display_text()),
            Some("opencode-mem {\"query\":\"sidebar fixes\"}".to_string())
        );
    }

    #[test]
    fn a_start_screen_session_is_idle_not_working() {
        // A session that was opened but only wrote a step-finish (no tool/text
        // activity) must not read as Working — that is what made the OpenCode
        // start screen show a fake Stop button.
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                parent_id TEXT, directory TEXT NOT NULL, title TEXT NOT NULL,
                model TEXT, cost REAL NOT NULL, tokens_input INTEGER NOT NULL,
                tokens_output INTEGER NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, time_archived INTEGER
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL
            );",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        conn.execute(
            "INSERT INTO session (id, project_id, directory, title, model, cost,
                 tokens_input, tokens_output, time_created, time_updated)
             VALUES (?1, 'project', '/repo', 'New session - x', NULL, 0, 0, 0, ?2, ?2)",
            rusqlite::params!["session-start", now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created,
             time_updated, data)
             VALUES ('p1', 'm1', 'session-start', ?1, ?1,
                     '{\"type\":\"step-finish\",\"reason\":\"stop\"}')",
            rusqlite::params![now],
        )
        .unwrap();
        drop(conn);

        let sessions = OpenCodeDetector.collect_sessions(&SessionRoot::host(temp.path()));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, HerdStatus::Idle);
    }

    /// A 1.x store with the `message` table the turn boundary is read from.
    fn legacy_store(temp: &Path) -> Connection {
        let db = temp.join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
                parent_id TEXT, directory TEXT NOT NULL, title TEXT NOT NULL,
                model TEXT, cost REAL NOT NULL, tokens_input INTEGER NOT NULL,
                tokens_output INTEGER NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, time_archived INTEGER
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL, data TEXT NOT NULL
            );",
        )
        .unwrap();
        conn
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    fn add_session(conn: &Connection, id: &str, parent: Option<&str>, title: &str, at: i64) {
        conn.execute(
            "INSERT INTO session (id, project_id, parent_id, directory, title, model,
                 cost, tokens_input, tokens_output, time_created, time_updated)
             VALUES (?1, 'p', ?2, '/repo', ?3, NULL, 0, 0, 0, ?4, ?4)",
            rusqlite::params![id, parent, title, at],
        )
        .unwrap();
    }

    fn add_message(conn: &Connection, id: &str, session: &str, data: &str, at: i64) {
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, session, at, data],
        )
        .unwrap();
    }

    fn add_part(conn: &Connection, id: &str, session: &str, data: &str, at: i64) {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, 'm', ?2, ?3, ?3, ?4)",
            rusqlite::params![id, session, at, data],
        )
        .unwrap();
    }

    #[test]
    fn the_newest_message_says_whether_the_turn_is_over() {
        let msg = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
        assert_eq!(
            turn_state_from_message(&msg(
                r#"{"role":"assistant","finish":"stop","time":{"created":1,"completed":2}}"#
            )),
            TurnState::Finished
        );
        assert_eq!(
            turn_state_from_message(&msg(
                r#"{"role":"assistant","finish":"tool-calls","time":{"created":1,"completed":2}}"#
            )),
            TurnState::Working,
            "the model asked for tools; the turn goes on"
        );
        assert_eq!(
            turn_state_from_message(&msg(r#"{"role":"assistant","time":{"created":1}}"#)),
            TurnState::Working,
            "still streaming"
        );
        assert_eq!(
            turn_state_from_message(&msg(r#"{"role":"user","time":{"created":1}}"#)),
            TurnState::Working
        );
        assert_eq!(turn_state_from_message(&msg(r#"{}"#)), TurnState::Unknown);
    }

    #[test]
    fn a_pending_question_holds_the_turn_on_the_human() {
        let temp = tempfile::tempdir().unwrap();
        let conn = legacy_store(temp.path());
        let now = now_ms();
        add_session(&conn, "s1", None, "Ask me", now);
        add_message(
            &conn,
            "m1",
            "s1",
            r#"{"role":"assistant","time":{"created":1}}"#,
            now,
        );
        add_part(
            &conn,
            "p1",
            "s1",
            r#"{"type":"tool","tool":"question","state":{"status":"running"}}"#,
            now,
        );
        assert_eq!(
            opencode_turn_state(&conn, OpencodeSchema::Legacy, "s1"),
            TurnState::AwaitingHuman
        );
        // Answered: the question no longer holds anything.
        conn.execute(
            "UPDATE part SET data = ?1 WHERE id = 'p1'",
            [r#"{"type":"tool","tool":"question","state":{"status":"completed"}}"#],
        )
        .unwrap();
        assert_eq!(
            opencode_turn_state(&conn, OpencodeSchema::Legacy, "s1"),
            TurnState::Working
        );
    }

    #[test]
    fn child_sessions_are_the_parents_subagents() {
        let temp = tempfile::tempdir().unwrap();
        let conn = legacy_store(temp.path());
        let now = now_ms();
        add_session(&conn, "parent", None, "Review the PR", now);
        add_message(
            &conn,
            "pm",
            "parent",
            r#"{"role":"assistant","finish":"stop","time":{"created":1,"completed":2}}"#,
            now,
        );
        // A child still mid-turn (silent for ten minutes) and one that is done.
        let ten_min_ago = now - 600_000;
        add_session(
            &conn,
            "child-1",
            Some("parent"),
            "Review security (@review-security subagent)",
            ten_min_ago,
        );
        add_message(
            &conn,
            "c1",
            "child-1",
            r#"{"role":"assistant","finish":"tool-calls","time":{"created":1,"completed":2}}"#,
            ten_min_ago,
        );
        add_session(
            &conn,
            "child-2",
            Some("parent"),
            "Review types (@review-types subagent)",
            now,
        );
        add_message(
            &conn,
            "c2",
            "child-2",
            r#"{"role":"assistant","finish":"stop","time":{"created":1,"completed":2}}"#,
            now,
        );
        drop(conn);

        let sessions = OpenCodeDetector.collect_sessions(&SessionRoot::host(temp.path()));
        assert_eq!(sessions.len(), 1, "children are not rows of their own");
        let parent = &sessions[0];
        assert_eq!(parent.turn, TurnState::Finished);
        assert_eq!(parent.subagents.len(), 2);
        let running = &parent.subagents[0];
        assert_eq!(running.agent_type, "review-security");
        assert_eq!(running.description, "Review security");
        assert!(running.pending);
        assert!(
            crate::agent_herd::subagent_is_working(running, SystemTime::now()),
            "an open child turn counts through a long silence"
        );
        let done = &parent.subagents[1];
        assert_eq!(done.status, HerdStatus::Done);
        assert!(!crate::agent_herd::subagent_is_working(
            done,
            SystemTime::now()
        ));
    }

    #[test]
    fn file_records_cover_patches_edits_and_children() {
        let temp = tempfile::tempdir().unwrap();
        let conn = legacy_store(temp.path());
        let now = now_ms();
        add_session(&conn, "parent", None, "Edit things", now);
        add_session(&conn, "child", Some("parent"), "Sub (@build subagent)", now);
        add_session(&conn, "other", None, "Unrelated", now);
        add_part(
            &conn,
            "p1",
            "parent",
            r#"{"type":"patch","hash":"x","files":["/repo/a.rs","/repo/b.rs"]}"#,
            now,
        );
        add_part(
            &conn,
            "p2",
            "parent",
            r#"{"type":"tool","tool":"edit","state":{"input":{"filePath":"/repo/c.rs"}}}"#,
            now,
        );
        add_part(
            &conn,
            "p3",
            "child",
            r#"{"type":"tool","tool":"apply_patch","state":{"input":{"patchText":"*** Begin Patch\n*** Update File: /repo/d.rs\n@@\n*** End Patch"}}}"#,
            now,
        );
        add_part(
            &conn,
            "p4",
            "parent",
            r#"{"type":"tool","tool":"read","state":{"input":{"filePath":"/repo/read-only.rs"}}}"#,
            now,
        );
        add_part(
            &conn,
            "p5",
            "other",
            r#"{"type":"patch","files":["/repo/not-mine.rs"]}"#,
            now,
        );
        drop(conn);

        let records = file_records(Some(temp.path()), "parent").unwrap();
        let touched = crate::agent_herd::touched::touched_by_source(
            &crate::agent_herd::touched::TouchSource::OpenCode {
                session_id: "parent".into(),
                home: Some(temp.path().to_path_buf()),
            },
        )
        .unwrap();
        assert_eq!(records.len(), 3);
        let mut spelled: Vec<&str> = touched.iter().map(|(_, spelled, _)| spelled).collect();
        spelled.sort();
        assert_eq!(
            spelled,
            vec!["/repo/a.rs", "/repo/b.rs", "/repo/c.rs", "/repo/d.rs"]
        );
    }

    #[test]
    fn a_session_whose_process_is_gone_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        // Implausibly high pid: something genuinely dead to reject.
        let dead = 0x7fff_fff0u32;
        write(
            &temp
                .path()
                .join(".config")
                .join("opencode")
                .join("dead.json"),
            &session_json(dead),
        );
        assert!(OpenCodeDetector
            .collect_sessions(&SessionRoot::host(temp.path()))
            .is_empty());
    }

    #[test]
    fn a_session_whose_process_is_alive_is_returned() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp
                .path()
                .join(".config")
                .join("opencode")
                .join("live.json"),
            &session_json(me),
        );
        let sessions = OpenCodeDetector.collect_sessions(&SessionRoot::host(temp.path()));
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].pid, me);
    }

    #[test]
    fn activity_is_read_from_opencode_storage_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let me = std::process::id();
        write(
            &temp.path().join(".config/opencode").join("live.json"),
            &session_json(me),
        );
        write(
            &temp
                .path()
                .join(".local/share/opencode/storage/session")
                .join(format!("sess-{me}.json")),
            r#"{"type":"tool","name":"bash","input":{"command":"cargo check"}}"#,
        );

        let sessions = OpenCodeDetector.collect_sessions(&SessionRoot::host(temp.path()));
        assert!(sessions[0]
            .activity
            .as_ref()
            .and_then(|activity| activity.current.as_ref())
            .is_some());
    }
}
