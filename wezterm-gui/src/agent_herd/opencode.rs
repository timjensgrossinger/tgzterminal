use crate::agent_herd::claude::process_is_alive;
use crate::agent_herd::vendor::{AgentVendor, SessionSource, VendorSession};
use crate::agent_herd::HerdStatus;
use rusqlite::{Connection, OpenFlags};
use std::convert::TryFrom;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const OPENCODE_ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
const OPENCODE_WORKING_WINDOW: Duration = Duration::from_secs(2 * 60);

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

    fn collect_sessions(&self, home: &Path) -> Vec<VendorSession> {
        let mut sessions = collect_database_sessions(home);
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
                    if !process_is_alive(pid) {
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
                        pid,
                        // This store does not distinguish harness-spawned
                        // sessions from interactive ones.
                        interactive: true,
                        vendor: AgentVendor::OpenCode,
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

fn collect_database_sessions(home: &Path) -> Vec<VendorSession> {
    let db = opencode_db(home);
    if !db.is_file() {
        return Vec::new();
    }

    let now = SystemTime::now();
    let cutoff = now
        .checked_sub(OPENCODE_ACTIVE_WINDOW)
        .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);
    let Ok(conn) = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };

    let mut stmt = match conn.prepare(
        "SELECT id, directory, title, model, cost, tokens_input, tokens_output, \
                time_created, time_updated \
         FROM session \
         WHERE parent_id IS NULL AND time_archived IS NULL AND time_updated >= ?1 \
         ORDER BY time_updated DESC LIMIT 32",
    ) {
        Ok(stmt) => stmt,
        Err(err) => {
            log::debug!("opencode database schema not recognized: {err:#}");
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
        let status = if age <= OPENCODE_WORKING_WINDOW {
            HerdStatus::Working
        } else {
            HerdStatus::Idle
        };
        let name = title.as_deref().and_then(clean_title);
        let model = model.and_then(|raw| model_label(&raw));
        let started_at = epoch_millis(created);
        let cost = (cost > 0.0).then(|| format!("${cost:.4}"));
        Some(VendorSession {
            // OpenCode's current database has no process id. Binding falls back
            // to the unique cwd match, while pane detection still handles live
            // sessions whose database row is too old.
            pid: 0,
            interactive: true,
            vendor: AgentVendor::OpenCode,
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
            activity: None,
            input_tokens: u64_count(input_tokens),
            output_tokens: u64_count(output_tokens),
            cost,
        })
    })
    .collect()
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
        drop(conn);

        let sessions = OpenCodeDetector.collect_sessions(temp.path());
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "session-1");
        assert_eq!(sessions[0].name.as_deref(), Some("Fix sidebar"));
        assert_eq!(sessions[0].model.as_deref(), Some("gpt-5"));
        assert_eq!(sessions[0].status, HerdStatus::Working);
        assert_eq!(sessions[0].input_tokens, Some(12));
        assert_eq!(sessions[0].output_tokens, Some(34));
        assert_eq!(sessions[0].cost.as_deref(), Some("$0.1250"));
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
        assert!(OpenCodeDetector.collect_sessions(temp.path()).is_empty());
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
        let sessions = OpenCodeDetector.collect_sessions(temp.path());
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

        let sessions = OpenCodeDetector.collect_sessions(temp.path());
        assert!(sessions[0]
            .activity
            .as_ref()
            .and_then(|activity| activity.current.as_ref())
            .is_some());
    }
}
