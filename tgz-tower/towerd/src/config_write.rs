//! Changes to the config folder made from the Tower view.
//!
//! Every edit is surgical (`toml_edit`): only the keys a form owns change,
//! so comments and settings written by hand survive. Writes are atomic
//! (temp file + rename) and keep the previous version as `<file>.bak`.
//!
//! The functions here are pure document edits plus the one write; checking
//! that a change makes sense against the rest of the config is the engine's
//! job (`settings.rs`).

use std::path::Path;
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};
use tower_proto::{EndpointSpec, ModelSpec, RoleSpec};

/// Read `path` (missing = empty), apply `f`, write it back if it changed.
pub fn edit(
    path: &Path,
    f: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{name}: {e}")),
    };
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{name}: {e}"))?;
    f(&mut doc)?;
    let out = doc.to_string();
    if out == text {
        return Ok(());
    }
    write_atomic(path, &out, &text)
}

/// Write `text` to `path` through a temp file; `previous` (if any) goes to
/// `<file>.bak` first.
pub fn write_atomic(path: &Path, text: &str, previous: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| format!("{} is not a file", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{name}: {e}"))?;
    }
    if !previous.is_empty() {
        std::fs::write(path.with_file_name(format!("{name}.bak")), previous)
            .map_err(|e| format!("{name}.bak: {e}"))?;
    }
    let tmp = path.with_file_name(format!("{name}.tmp"));
    std::fs::write(&tmp, text).map_err(|e| format!("{name}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{name}: {e}"))
}

/// `[[key]]` in `doc`, created when missing.
fn tables_mut<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut ArrayOfTables, String> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc[key]
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("{key} must be a list of [[{key}]] tables"))
}

fn position(list: &ArrayOfTables, field: &str, want: &str) -> Option<usize> {
    list.iter()
        .position(|t| t.get(field).and_then(|v| v.as_str()) == Some(want))
}

/// Set `key` to a non-empty string, or remove it.
fn set_str(t: &mut Table, key: &str, v: Option<&str>) {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            t[key] = value(s);
        }
        None => {
            t.remove(key);
        }
    }
}

/// The table to write `want` into: the one named `old` (renaming) or
/// `want`, else a new one at the end. Refuses a rename onto another entry.
fn upsert<'a>(
    list: &'a mut ArrayOfTables,
    field: &str,
    want: &str,
    old: Option<&str>,
    what: &str,
) -> Result<&'a mut Table, String> {
    let at = position(list, field, old.unwrap_or(want));
    if let Some(other) = position(list, field, want) {
        if Some(other) != at {
            return Err(format!("There is already a {what} {want:?}"));
        }
    }
    let i = match at {
        Some(i) => i,
        None if old.is_some() => return Err(format!("No {what} {:?}", old.unwrap_or(want))),
        None => {
            list.push(Table::new());
            list.len() - 1
        }
    };
    Ok(list.get_mut(i).expect("index from position or push"))
}

/// Add or change an `[[endpoint]]`; `old_name` renames it, and the
/// `[[model]]` ids on it follow.
pub fn upsert_endpoint(
    doc: &mut DocumentMut,
    spec: &EndpointSpec,
    old_name: Option<&str>,
) -> Result<(), String> {
    let t = upsert(
        tables_mut(doc, "endpoint")?,
        "name",
        &spec.name,
        old_name,
        "model server",
    )?;
    t["name"] = value(spec.name.as_str());
    t["kind"] = value(spec.kind.as_str());
    t["base_url"] = value(spec.base_url.as_str());
    match spec.local {
        Some(b) => {
            t["local"] = value(b);
        }
        None => {
            t.remove("local");
        }
    }
    set_str(t, "api_key_env", spec.api_key_env.as_deref());
    if let Some(old) = old_name.filter(|o| *o != spec.name) {
        let prefix = format!("{old}/");
        for m in tables_mut(doc, "model")?.iter_mut() {
            let renamed = m
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|id| id.strip_prefix(&prefix))
                .map(|rest| format!("{}/{rest}", spec.name));
            if let Some(id) = renamed {
                m["id"] = value(id);
            }
        }
    }
    Ok(())
}

pub fn delete_endpoint(doc: &mut DocumentMut, name: &str) -> Result<(), String> {
    remove(doc, "endpoint", "name", name, "model server")
}

/// Add or change a `[[model]]`; `old_id` renames it.
pub fn upsert_model(
    doc: &mut DocumentMut,
    spec: &ModelSpec,
    old_id: Option<&str>,
) -> Result<(), String> {
    let t = upsert(tables_mut(doc, "model")?, "id", &spec.id, old_id, "model")?;
    t["id"] = value(spec.id.as_str());
    set_str(t, "label", Some(&spec.label));
    set_str(t, "size", Some(&spec.size));
    match spec.context {
        Some(c) => {
            t["context"] = value(i64::from(c));
        }
        None => {
            t.remove("context");
        }
    }
    let good: Array = spec
        .good_for
        .iter()
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        .collect();
    if good.is_empty() {
        t.remove("good_for");
    } else {
        t["good_for"] = value(good);
    }
    set_str(t, "notes", Some(&spec.notes));
    t["max_concurrency"] = value(i64::from(spec.max_concurrency.max(1)));
    Ok(())
}

pub fn delete_model(doc: &mut DocumentMut, id: &str) -> Result<(), String> {
    remove(doc, "model", "id", id, "model")
}

/// Remove the `[[key]]` whose `field` is `want`. The comments above it
/// (often the file's own header) move to the next table, or to the end.
fn remove(
    doc: &mut DocumentMut,
    key: &str,
    field: &str,
    want: &str,
    what: &str,
) -> Result<(), String> {
    let list = tables_mut(doc, key)?;
    let i = position(list, field, want).ok_or_else(|| format!("No {what} {want:?}"))?;
    let comments = list
        .get(i)
        .and_then(|t| t.decor().prefix())
        .and_then(|p| p.as_str())
        .filter(|p| p.contains('#'))
        .map(str::to_string);
    list.remove(i);
    if let Some(c) = comments {
        match list.get_mut(i) {
            Some(next) => {
                let mine = next
                    .decor()
                    .prefix()
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                next.decor_mut().set_prefix(format!("{c}{mine}"));
            }
            None => {
                let tail = doc.trailing().as_str().unwrap_or("").to_string();
                doc.set_trailing(format!("{c}{tail}"));
            }
        }
    }
    Ok(())
}

/// Write a role into its own file's document (`roles/<id>.toml`): the
/// keys the Roster edits; MCP servers, ACP options, effort and sign-in
/// stay as they are.
pub fn set_role(doc: &mut DocumentMut, spec: &RoleSpec) {
    let t = doc.as_table_mut();
    t["name"] = value(spec.name.trim());
    set_str(t, "color", Some(&spec.color));
    t["driver"] = value(spec.driver.as_str());
    if spec.command.is_empty() {
        t.remove("command");
    } else {
        t["command"] = value(spec.command.iter().map(String::as_str).collect::<Array>());
    }
    t["mission"] = multiline(spec.mission.trim());
    if spec.ladder.is_empty() {
        t.remove("ladder");
    } else {
        t["ladder"] = value(spec.ladder.iter().map(String::as_str).collect::<Array>());
    }
    t["max_concurrency"] = value(i64::from(spec.max_concurrency.max(1)));
}

/// A string written as a `"""` block, as people write missions by hand;
/// a plain string when the text would not survive that.
fn multiline(text: &str) -> Item {
    if !text.contains("\"\"\"") && !text.ends_with('"') {
        let block = format!("v = \"\"\"\n{}\n\"\"\"\n", text.replace('\\', "\\\\"));
        if let Ok(d) = block.parse::<DocumentMut>() {
            if d.get("v").and_then(|v| v.as_str()) == Some(&format!("{text}\n")) {
                return d["v"].clone();
            }
        }
    }
    value(text)
}

/// `[routing] <tier> = [engines…]`.
pub fn set_routing_tier(
    doc: &mut DocumentMut,
    tier: &str,
    engines: &[String],
) -> Result<(), String> {
    let routing = table_mut(doc, "routing")?;
    routing[tier] = value(engines.iter().map(String::as_str).collect::<Array>());
    Ok(())
}

/// `[policy.actions] "<action>" = "<level>"`, or back to the default.
pub fn set_policy_level(
    doc: &mut DocumentMut,
    action: &str,
    level: Option<&str>,
) -> Result<(), String> {
    let policy = table_mut(doc, "policy")?;
    if !policy.contains_key("actions") {
        policy.insert("actions", Item::Table(Table::new()));
    }
    let actions = policy["actions"]
        .as_table_like_mut()
        .ok_or("tower.toml: [policy] actions must be a table")?;
    match level {
        Some(l) => {
            actions.insert(action, value(l));
        }
        None => {
            actions.remove(action);
        }
    }
    Ok(())
}

/// `[privacy] local_only_paths`.
pub fn set_local_only_paths(doc: &mut DocumentMut, paths: &[String]) -> Result<(), String> {
    let privacy = table_mut(doc, "privacy")?;
    if paths.is_empty() {
        privacy.remove("local_only_paths");
    } else {
        privacy["local_only_paths"] = value(paths.iter().map(String::as_str).collect::<Array>());
    }
    Ok(())
}

/// `[key]` in `doc`, created when missing.
fn table_mut<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Table, String> {
    if !doc.contains_key(key) {
        doc.insert(key, Item::Table(Table::new()));
    }
    doc[key]
        .as_table_mut()
        .ok_or_else(|| format!("[{key}] must be a table"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = "# Model servers, hand-written.\n[[endpoint]]\nname = \"localhost\" # my Mac\nkind = \"ollama\"\nbase_url = \"http://localhost:11434\"\n\n# a model I like\n[[model]]\nid = \"localhost/qwen3.6:27b\"\nlabel = \"Qwen\"\n";

    fn doc(s: &str) -> DocumentMut {
        s.parse().unwrap()
    }

    fn endpoint(name: &str) -> EndpointSpec {
        EndpointSpec {
            name: name.into(),
            kind: "vllm".into(),
            base_url: "https://llm.example.internal/v1".into(),
            local: Some(true),
            api_key_env: Some("COMPANY_LLM_KEY".into()),
        }
    }

    #[test]
    fn adding_an_endpoint_keeps_the_comments() {
        let mut d = doc(MODELS);
        upsert_endpoint(&mut d, &endpoint("company"), None).unwrap();
        let out = d.to_string();
        assert!(out.contains("# Model servers, hand-written."));
        assert!(out.contains("# my Mac"));
        assert!(out.contains("# a model I like"));
        let cfg: toml::Value = toml::from_str(&out).unwrap();
        let eps = cfg["endpoint"].as_array().unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[1]["local"].as_bool(), Some(true));
        assert_eq!(eps[1]["api_key_env"].as_str(), Some("COMPANY_LLM_KEY"));
    }

    #[test]
    fn renaming_an_endpoint_moves_its_models_and_refuses_clashes() {
        let mut d = doc(MODELS);
        let mut spec = endpoint("mac");
        spec.kind = "ollama".into();
        spec.local = None;
        spec.api_key_env = None;
        upsert_endpoint(&mut d, &spec, Some("localhost")).unwrap();
        let cfg: toml::Value = toml::from_str(&d.to_string()).unwrap();
        assert_eq!(cfg["endpoint"].as_array().unwrap().len(), 1);
        assert!(cfg["endpoint"][0].get("local").is_none(), "unset = guessed");
        assert_eq!(cfg["model"][0]["id"].as_str(), Some("mac/qwen3.6:27b"));

        upsert_endpoint(&mut d, &endpoint("company"), None).unwrap();
        let err = upsert_endpoint(&mut d, &endpoint("mac"), Some("company")).unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert!(upsert_endpoint(&mut d, &endpoint("x"), Some("nope")).is_err());
    }

    #[test]
    fn models_are_upserted_and_deleted() {
        let mut d = doc(MODELS);
        let spec = ModelSpec {
            id: "localhost/qwen3.6:27b".into(),
            label: "Qwen 3.6 27B".into(),
            size: "27B".into(),
            context: Some(131_072),
            good_for: vec!["docs".into(), " ".into(), "tests".into()],
            notes: String::new(),
            max_concurrency: 2,
        };
        upsert_model(&mut d, &spec, None).unwrap();
        let out = d.to_string();
        assert!(out.contains("# a model I like"));
        let cfg: toml::Value = toml::from_str(&out).unwrap();
        let m = &cfg["model"].as_array().unwrap();
        assert_eq!(m.len(), 1, "same id = same entry");
        assert_eq!(m[0]["label"].as_str(), Some("Qwen 3.6 27B"));
        assert_eq!(m[0]["good_for"].as_array().unwrap().len(), 2);
        assert!(m[0].get("notes").is_none());
        assert_eq!(m[0]["max_concurrency"].as_integer(), Some(2));
        delete_model(&mut d, "localhost/qwen3.6:27b").unwrap();
        assert!(delete_model(&mut d, "localhost/qwen3.6:27b").is_err());
        delete_endpoint(&mut d, "localhost").unwrap();
        let cfg: toml::Value = toml::from_str(&d.to_string()).unwrap();
        assert!(
            cfg.get("endpoint").is_none(),
            "an empty list writes nothing"
        );
        assert!(d.to_string().contains("# Model servers, hand-written."));
    }

    #[test]
    fn edit_writes_atomically_with_a_backup() {
        let dir = std::env::temp_dir().join(format!("tower-cw-{}", crate::paths::random_u64()));
        let p = dir.join("models.toml");
        edit(&p, |d| upsert_endpoint(d, &endpoint("company"), None)).unwrap();
        assert!(!dir.join("models.toml.bak").exists(), "nothing to back up");
        let first = std::fs::read_to_string(&p).unwrap();
        edit(&p, |d| delete_endpoint(d, "company")).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("models.toml.bak")).unwrap(),
            first
        );
        assert!(!dir.join("models.toml.tmp").exists());
        // A failed edit leaves the file alone.
        let before = std::fs::read_to_string(&p).unwrap();
        assert!(edit(&p, |d| delete_endpoint(d, "company")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn roles_keep_what_the_form_does_not_edit() {
        let mut d = doc("# my reviewer\nname = \"Review\"\ndriver = \"acp\"\ncommand = [\"opencode\", \"acp\"]\nmission = \"old\"\neffort = \"high\"\n\n[mcp.github]\ncommand = \"gh-mcp\"\n");
        let spec = RoleSpec {
            id: "review".into(),
            name: "Reviewer".into(),
            color: "#bb9af7".into(),
            driver: "acp".into(),
            command: vec!["opencode".into(), "acp".into()],
            mission: "Review the diff.\nNever edit code; say \"LGTM\".".into(),
            ladder: vec!["localhost/qwen3.6:27b".into()],
            max_concurrency: 2,
        };
        set_role(&mut d, &spec);
        let out = d.to_string();
        assert!(out.contains("# my reviewer"));
        let p =
            crate::config::parse_role("review", std::path::Path::new("review.toml"), &out).unwrap();
        assert_eq!(p.name, "Reviewer");
        assert_eq!(p.mission.trim(), spec.mission);
        assert_eq!(p.effort.as_deref(), Some("high"), "kept");
        assert!(p.mcp.contains_key("github"), "kept");
        assert_eq!(p.ladder, spec.ladder);
        assert_eq!(p.max_concurrency, 2);
        // A mission with a backslash or triple quotes still round-trips.
        let mut odd = spec.clone();
        odd.mission = "C:\\path and \"\"\"quotes\"\"\"".into();
        set_role(&mut d, &odd);
        let p = crate::config::parse_role("review", std::path::Path::new("r.toml"), &d.to_string())
            .unwrap();
        assert_eq!(p.mission.trim(), odd.mission);
    }

    #[test]
    fn routing_policy_and_privacy_are_written_into_tower_toml() {
        let mut d = doc("# tower\n[routing]\nlow = [\"a\"]\n# engines\n[routing.engines.a]\ndriver = \"llm\"\nmodel = \"x\"\n");
        set_routing_tier(&mut d, "low", &["b".into(), "a".into()]).unwrap();
        set_policy_level(&mut d, "handoff.release", Some("auto")).unwrap();
        set_policy_level(&mut d, "land", Some("off")).unwrap();
        set_policy_level(&mut d, "land", None).unwrap();
        set_local_only_paths(&mut d, &["~/src/payroll".into()]).unwrap();
        let out = d.to_string();
        assert!(out.contains("# engines"));
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["routing"]["low"].as_array().unwrap().len(), 2);
        assert_eq!(v["routing"]["engines"]["a"]["model"].as_str(), Some("x"));
        assert_eq!(
            v["policy"]["actions"]["handoff.release"].as_str(),
            Some("auto")
        );
        assert!(v["policy"]["actions"].get("land").is_none());
        assert_eq!(
            v["privacy"]["local_only_paths"][0].as_str(),
            Some("~/src/payroll")
        );
        set_local_only_paths(&mut d, &[]).unwrap();
        let v: toml::Value = toml::from_str(&d.to_string()).unwrap();
        assert!(v["privacy"].get("local_only_paths").is_none());
    }
}
