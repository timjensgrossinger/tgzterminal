use crate::agent_herd::claude::session_is_live;
use crate::agent_herd::vendor::{
    AgentVendor, SessionOrigin, SessionRoot, SessionSource, VendorSession,
};
use crate::agent_herd::{HerdActivity, HerdContent, HerdEvent, HerdEventKind, HerdStatus};
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
enum OpencodeSchema {
    /// `session` + `part` (OpenCode 1.x CLI).
    Legacy,
    /// `session_v2` + `session_message` (OpenCode 2.x CLI / desktop app).
    V2,
}

impl OpencodeSchema {
    fn sessions_table(self) -> &'static str {
        match self {
            Self::Legacy => "session",
            Self::V2 => "session_v2",
        }
    }

    fn events_table(self) -> &'static str {
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

fn detect_opencode_schema(conn: &Connection) -> Option<OpencodeSchema> {
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
fn part_objects<'a>(data: &'a serde_json::Value) -> Vec<&'a serde_json::Value> {
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

fn opencode_db(home: &Path) -> PathBuf {
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

    rows.filter_map(|row| {
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
            // This store exposes no turn boundary; freshness is all it has.
            turn: crate::agent_herd::TurnState::Unknown,
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
    .collect()
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
