//! Config edits from the Tower view: model servers and models
//! (`models.toml`), roles (`roles/<id>.toml`), routing tiers, policy levels
//! and local-only folders (`tower.toml`). Checked here against the rest of
//! the config, written by `config_write`, then picked up with a reload like
//! a hand edit.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::config_write;
use tower_proto::{EndpointSpec, ModelSpec, RoleSpec};

/// The server kinds Tower can probe and point agents at.
pub const ENDPOINT_KINDS: [&str; 6] = [
    "ollama",
    "lmstudio",
    "llama.cpp",
    "vllm",
    "mlx",
    "openai-compatible",
];

fn env_name_ok(v: &str) -> bool {
    !v.is_empty()
        && !v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A model server as typed into the form: trimmed, or why it can't be saved.
pub fn check_endpoint(mut spec: EndpointSpec) -> Result<EndpointSpec, String> {
    spec.name = spec.name.trim().to_string();
    spec.kind = spec.kind.trim().to_string();
    spec.base_url = spec.base_url.trim().trim_end_matches('/').to_string();
    spec.api_key_env = spec
        .api_key_env
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let name_ok = !spec.name.is_empty()
        && spec
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !name_ok {
        return Err("Name: use letters, digits, '-', '_' and '.'".into());
    }
    if !ENDPOINT_KINDS.contains(&spec.kind.as_str()) {
        return Err(format!("Kind must be one of {}", ENDPOINT_KINDS.join(", ")));
    }
    let scheme_ok = ["http://", "https://"]
        .iter()
        .any(|s| spec.base_url.len() > s.len() && spec.base_url.starts_with(s));
    if !scheme_ok || spec.base_url.contains(char::is_whitespace) {
        return Err("Address must start with http:// or https://".into());
    }
    if let Some(v) = &spec.api_key_env {
        if !env_name_ok(v) {
            return Err(format!("{v:?} is not an environment variable name"));
        }
    }
    Ok(spec)
}

/// A model as typed into the form: trimmed, or why it can't be saved.
pub fn check_model(mut spec: ModelSpec) -> Result<ModelSpec, String> {
    spec.id = spec.id.trim().to_string();
    spec.label = spec.label.trim().to_string();
    spec.size = spec.size.trim().to_string();
    spec.notes = spec.notes.trim().to_string();
    spec.good_for = spec
        .good_for
        .into_iter()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .collect();
    match spec.id.split_once('/') {
        Some((ep, m))
            if !ep.is_empty() && !m.is_empty() && !spec.id.contains(char::is_whitespace) => {}
        _ => return Err("Model id must be <server>/<model name on it>".into()),
    }
    if !(1..=64).contains(&spec.max_concurrency) {
        return Err("At once must be between 1 and 64".into());
    }
    if spec.context == Some(0) {
        spec.context = None;
    }
    Ok(spec)
}

/// `Review Bot` -> `review-bot`, for a new role's file name.
pub fn role_id_from(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
        if id.len() >= 40 {
            break;
        }
    }
    id.trim_end_matches('-').to_string()
}

fn role_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Engine {
    fn models_file(&self) -> PathBuf {
        self.paths.config.join("models.toml")
    }

    /// Roles and engines whose model id satisfies `uses`, for "still used by …".
    fn model_users(&self, uses: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut out: Vec<String> = self
            .cfg
            .roles
            .iter()
            // The built-in Local role follows models.toml by itself.
            .filter(|r| !(r.id == crate::config::LOCAL_ROLE && !r.file.exists()))
            .filter(|r| r.ladder.iter().any(|m| uses(m)))
            .map(|r| format!("role {}", r.name))
            .collect();
        out.extend(
            self.cfg
                .routing
                .engines
                .iter()
                .filter(|(_, e)| e.model.as_deref().map(uses).unwrap_or(false))
                .map(|(id, _)| format!("engine {id}")),
        );
        out
    }

    pub(super) fn save_endpoint(&mut self, spec: EndpointSpec, old_name: Option<&str>) -> Res {
        let spec = check_endpoint(spec)?;
        if let Some(old) = old_name.filter(|o| *o != spec.name) {
            let prefix = format!("{old}/");
            let users = self.model_users(&|m| m.starts_with(&prefix));
            if !users.is_empty() {
                return Err(format!(
                    "{} still use {old}/…: change them before renaming the server",
                    users.join(", ")
                ));
            }
        }
        config_write::edit(&self.models_file(), |d| {
            config_write::upsert_endpoint(d, &spec, old_name)
        })?;
        self.reload();
        Ok(spec.name)
    }

    pub(super) fn delete_endpoint(&mut self, name: &str) -> Res {
        let prefix = format!("{name}/");
        let users = self.model_users(&|m| m.starts_with(&prefix));
        if !users.is_empty() {
            return Err(format!("{} still use {name}", users.join(", ")));
        }
        let models: Vec<String> = self
            .cfg
            .models
            .iter()
            .filter(|m| m.id.starts_with(&prefix))
            .map(|m| m.id.clone())
            .collect();
        config_write::edit(&self.models_file(), |d| {
            for id in &models {
                config_write::delete_model(d, id)?;
            }
            config_write::delete_endpoint(d, name)
        })?;
        self.reload();
        Ok(name.to_string())
    }

    pub(super) fn save_model(&mut self, spec: ModelSpec, old_id: Option<&str>) -> Res {
        let spec = check_model(spec)?;
        let ep = spec.id.split_once('/').map(|(e, _)| e).unwrap_or("");
        if !self.cfg.endpoints.iter().any(|e| e.name == ep) {
            return Err(format!("There is no model server {ep:?}: add it first"));
        }
        if let Some(old) = old_id.filter(|o| *o != spec.id) {
            let users = self.model_users(&|m| m == old);
            if !users.is_empty() {
                return Err(format!(
                    "{} still use {old}: change them before renaming the model",
                    users.join(", ")
                ));
            }
        }
        config_write::edit(&self.models_file(), |d| {
            config_write::upsert_model(d, &spec, old_id)
        })?;
        self.reload();
        Ok(spec.id)
    }

    pub(super) fn delete_model(&mut self, id: &str) -> Res {
        config_write::edit(&self.models_file(), |d| config_write::delete_model(d, id))?;
        self.reload();
        Ok(id.to_string())
    }
    fn tower_file(&self) -> PathBuf {
        self.paths.config.join("tower.toml")
    }

    /// What names role `id` and would break if it were renamed: schedules,
    /// triage, intake watches and scouts, and handoffs still waiting.
    fn role_users(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .cfg
            .schedules
            .iter()
            .filter(|s| s.role == id)
            .map(|s| format!("schedule {}", s.name))
            .collect();
        if self.cfg.triage.role.as_deref() == Some(id) {
            out.push("[triage]".into());
        }
        for w in self.cfg.watches() {
            if w.role().map(String::as_str) == Some(id)
                || w.triage_role().map(String::as_str) == Some(id)
            {
                out.push(format!("watch {}", w.name()));
            }
        }
        for s in &self.cfg.scouts {
            if s.role == id || s.target_role.as_deref() == Some(id) {
                out.push(format!("scout {}", s.name));
            }
        }
        let waiting = self
            .st
            .flights
            .iter()
            .filter_map(|f| self.child_packet(&f.id).filter(|_| f.parent_id.is_some()))
            .filter(|p| p.to_role == id && p.state == HandoffState::Draft)
            .count();
        if waiting > 0 {
            out.push(format!("{waiting} waiting handoff(s)"));
        }
        out
    }

    /// Live agents of a role, for "still in use".
    fn role_in_use(&self, id: &str) -> usize {
        self.st
            .agents
            .iter()
            .filter(|a| a.role_id == id && a.status.is_live())
            .count()
    }

    pub(super) fn save_role(&mut self, mut spec: RoleSpec, old_id: Option<&str>) -> Res {
        spec.name = spec.name.trim().to_string();
        if spec.name.is_empty() {
            return Err("Give the role a name".into());
        }
        if spec.id.trim().is_empty() {
            spec.id = role_id_from(&spec.name);
            if spec.id.is_empty() {
                return Err("Use some letters or digits in the role's name".into());
            }
        }
        spec.id = spec.id.trim().to_string();
        if !role_id_ok(&spec.id) {
            return Err("Role id: lowercase letters, digits and dashes".into());
        }
        if let Some(old) = old_id {
            if !role_id_ok(old) {
                return Err(format!("{old:?} is not a role id"));
            }
        }
        spec.command = spec
            .command
            .into_iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        spec.ladder = spec
            .ladder
            .into_iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        if !(1..=64).contains(&spec.max_concurrency) {
            return Err("At once must be between 1 and 64".into());
        }
        let roles = self.paths.config.join("roles");
        let file = roles.join(format!("{}.toml", spec.id));
        let renamed = old_id.filter(|o| *o != spec.id);
        if let Some(old) = renamed {
            if self.role_in_use(old) > 0 {
                return Err(format!(
                    "{old} has live agents: stop them before renaming it"
                ));
            }
            let users = self.role_users(old);
            if !users.is_empty() {
                return Err(format!(
                    "{} still name {old}: change them before renaming it",
                    users.join(", ")
                ));
            }
        }
        if (old_id.is_none() || renamed.is_some()) && file.exists() {
            return Err(format!("There is already a role {:?}", spec.id));
        }
        // Start from the old file (renaming keeps its other settings) and
        // check the result parses like a hand-written role before writing.
        let source = renamed
            .map(|o| roles.join(format!("{o}.toml")))
            .unwrap_or_else(|| file.clone());
        let text = std::fs::read_to_string(&source).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e| format!("roles/{}.toml: {e}", spec.id))?;
        config_write::set_role(&mut doc, &spec);
        let out = doc.to_string();
        config::parse_role(&spec.id, &file, &out).map_err(|e| format!("Not saved: {e}"))?;
        let previous = if renamed.is_some() {
            String::new()
        } else {
            text
        };
        config_write::write_atomic(&file, &out, &previous)?;
        if renamed.is_some() && source.exists() {
            // Kept as roles/<old>.toml.bak, which Tower does not load.
            let bak = roles.join(format!("{}.toml.bak", old_id.unwrap_or_default()));
            std::fs::rename(&source, &bak).map_err(|e| format!("{}: {e}", source.display()))?;
        }
        self.reload();
        Ok(spec.id)
    }

    pub(super) fn delete_role(&mut self, id: &str) -> Res {
        if !role_id_ok(id) {
            return Err(format!("{id:?} is not a role id"));
        }
        let n = self.role_in_use(id);
        if n > 0 {
            return Err(format!("{id} has {n} live agent(s): stop them first"));
        }
        let roles = self.paths.config.join("roles");
        let file = roles.join(format!("{id}.toml"));
        if !file.exists() {
            return Err(format!("No role file roles/{id}.toml"));
        }
        let bak = roles.join(format!("{id}.toml.bak"));
        std::fs::rename(&file, &bak).map_err(|e| format!("{}: {e}", file.display()))?;
        self.reload();
        Ok(id.to_string())
    }

    pub(super) fn set_routing(&mut self, tier: &str, engines: Vec<String>) -> Res {
        if core::router::Tier::parse(tier).is_none() {
            return Err(format!("Unknown tier {tier:?}"));
        }
        let engines: Vec<String> = engines
            .into_iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
        if let Some(bad) = engines
            .iter()
            .find(|e| !self.cfg.routing.engines.contains_key(e.as_str()))
        {
            return Err(format!("There is no engine {bad:?} in [routing.engines]"));
        }
        config_write::edit(&self.tower_file(), |d| {
            config_write::set_routing_tier(d, tier, &engines)
        })?;
        self.reload();
        Ok(tier.to_string())
    }

    pub(super) fn set_policy_level(&mut self, action: &str, level: Option<&str>) -> Res {
        if crate::policy::Action::from_key(action).is_none() {
            return Err(format!("Unknown action {action:?}"));
        }
        if let Some(l) = level {
            if !["off", "draft", "confirm", "auto"].contains(&l) {
                return Err(format!("Unknown level {l:?}"));
            }
        }
        config_write::edit(&self.tower_file(), |d| {
            config_write::set_policy_level(d, action, level)
        })?;
        self.reload();
        Ok(action.to_string())
    }

    pub(super) fn set_local_only_paths(&mut self, paths: Vec<String>) -> Res {
        let mut clean: Vec<String> = vec![];
        for p in paths.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            if !(p.starts_with('/') || p.starts_with('~') || p.contains(":\\")) {
                return Err(format!("{p:?}: give a full path (/… or ~/…)"));
            }
            if !clean.iter().any(|c| c == p) {
                clean.push(p.to_string());
            }
        }
        config_write::edit(&self.tower_file(), |d| {
            config_write::set_local_only_paths(d, &clean)
        })?;
        self.reload();
        Ok(format!("{} local-only folder(s)", clean.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str, url: &str) -> EndpointSpec {
        EndpointSpec {
            name: name.into(),
            kind: "vllm".into(),
            base_url: url.into(),
            local: None,
            api_key_env: None,
        }
    }

    #[test]
    fn endpoint_forms_are_checked() {
        let ok = check_endpoint(ep(" company ", " https://llm.example.internal/v1/ ")).unwrap();
        assert_eq!(ok.name, "company");
        assert_eq!(ok.base_url, "https://llm.example.internal/v1");
        assert!(check_endpoint(ep("a/b", "http://x")).is_err());
        assert!(check_endpoint(ep("a", "ftp://x")).is_err());
        assert!(check_endpoint(ep("a", "http://")).is_err());
        let mut bad_kind = ep("a", "http://x");
        bad_kind.kind = "magic".into();
        assert!(check_endpoint(bad_kind).is_err());
        let mut bad_env = ep("a", "http://x");
        bad_env.api_key_env = Some("MY KEY".into());
        assert!(check_endpoint(bad_env).is_err());
        let mut blank_env = ep("a", "http://x");
        blank_env.api_key_env = Some("  ".into());
        assert_eq!(check_endpoint(blank_env).unwrap().api_key_env, None);
    }

    #[test]
    fn role_ids_are_made_from_names() {
        assert_eq!(role_id_from("Review Bot"), "review-bot");
        assert_eq!(role_id_from("  Ground (local)!"), "ground-local");
        assert!(role_id_ok("ground-2"));
        assert!(!role_id_ok("Ground"));
        assert!(!role_id_ok("../x"));
        assert!(!role_id_ok(""));
    }

    #[test]
    fn model_forms_are_checked() {
        let spec = |id: &str, n: u32| ModelSpec {
            id: id.into(),
            max_concurrency: n,
            good_for: vec![" docs ".into(), "".into()],
            ..Default::default()
        };
        let ok = check_model(spec("company/qwen", 1)).unwrap();
        assert_eq!(ok.good_for, vec!["docs"]);
        assert!(check_model(spec("qwen", 1)).is_err());
        assert!(check_model(spec("company/", 1)).is_err());
        assert!(check_model(spec("company/q w", 1)).is_err());
        assert!(check_model(spec("company/qwen", 0)).is_err());
        assert!(check_model(spec("company/qwen", 65)).is_err());
    }
}
