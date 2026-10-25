//! Local-only work in the engine: working out when a flight must stay
//! inside your network, refusing cloud models for it, and keeping its
//! details out of cloud agents' conversations. See `privacy.rs` for the
//! settings.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use crate::privacy::{self, ReturnMode};

/// How far up the handoff chain a parent's mark is looked for.
const MAX_DEPTH: usize = 16;

impl Engine {
    /// Why `flight_id` must stay local, if it must: its own mark, its
    /// intake source or scout, its folder or repo, or a parent's.
    pub(super) fn local_only_reason(&self, flight_id: &str) -> Option<String> {
        let mut id = flight_id.to_string();
        for depth in 0..MAX_DEPTH {
            let f = self.flight(&id)?;
            let own = depth == 0;
            if f.local_only {
                return Some(if own {
                    "it is marked local-only".to_string()
                } else {
                    format!("{} it came from is local-only", f.key)
                });
            }
            if let Some(w) = self.st.flight_source.get(&id) {
                if self
                    .cfg
                    .watches()
                    .iter()
                    .any(|x| x.name() == w && x.local_only())
                {
                    return Some(format!("it came from {w}, a local-only source"));
                }
            }
            if let Some(s) = self.st.scout_flights.get(&id) {
                if self.cfg.scouts.iter().any(|x| &x.name == s && x.local_only) {
                    return Some(format!("scout {s} is local-only"));
                }
            }
            if let Some(dir) = f.workdir.as_deref() {
                let dir = std::path::Path::new(dir);
                let repo = config::repo_privacy(dir);
                if let Some(why) = privacy::folder_reason(dir, &self.cfg.privacy, &repo) {
                    return Some(why);
                }
            }
            id = f.parent_id.clone()?;
        }
        None
    }

    /// Show the mark on the flight (for the board) and why, once.
    pub(super) fn mark_local_only(&mut self, flight_id: &str, why: &str) {
        let newly = match self.flight_mut(flight_id) {
            Some(f) if !f.local_only => {
                f.local_only = true;
                true
            }
            _ => false,
        };
        if newly {
            self.note(flight_id, format!("Local only: {why}"));
        }
    }

    /// Refuse a model outside your network on local-only work.
    pub(super) fn check_local(
        &mut self,
        flight_id: &str,
        role: &RoleCfg,
        model: &ModelRef,
    ) -> Result<(), String> {
        let Some(why) = self.local_only_reason(flight_id) else {
            return Ok(());
        };
        if model.locality == Locality::Local {
            return Ok(());
        }
        let what = if model.id.is_empty() {
            "its default model".to_string()
        } else {
            model.label.clone()
        };
        self.note(
            flight_id,
            format!("Refused {} on {what}: this work must stay local", role.name),
        );
        Err(format!(
            "{} would run {what} outside your network, but this work must stay local ({why}). Pick a role on a local model.",
            role.name
        ))
    }

    /// The routing engines `flight_id` may use: local ones only when it
    /// must stay local.
    pub(super) fn engines_for(&self, flight_id: &str) -> Vec<EngineInfo> {
        if self.local_only_reason(flight_id).is_none() {
            return self.engines.clone();
        }
        self.engines
            .iter()
            .filter(|e| e.locality == Locality::Local)
            .cloned()
            .collect()
    }

    /// May `triage` read this flight? Not a cloud model on local-only work.
    pub(super) fn triage_refusal(&self, flight_id: &str, triage: &RoleCfg) -> Option<String> {
        let why = self.local_only_reason(flight_id)?;
        // No model named (or routed): the program's own default, which
        // Tower cannot vouch for.
        let local = !triage.routed
            && triage
                .ladder
                .first()
                .map(|m| triage.model_ref(m).locality == Locality::Local)
                .unwrap_or(false);
        (!local).then(|| {
            format!(
                "Triage skipped: this work must stay local ({why}) and {} runs outside your network",
                triage.name
            )
        })
    }

    /// The repo folder and sensitive-path globs for work in `dir`.
    pub(super) fn sensitive_globs(&self, dir: &str) -> Vec<(PathBuf, String)> {
        config::repo_privacy(std::path::Path::new(dir)).globs
    }

    /// A sensitive path an ACP agent's permission request names, if any.
    pub(super) fn sensitive_hit(&self, agent_id: &str, input: &str) -> Option<String> {
        let a = self.agent(agent_id)?;
        if a.model.locality == Locality::Local {
            return None;
        }
        let dir = a
            .flight_id
            .as_deref()
            .and_then(|f| self.flight(f))
            .and_then(|f| f.workdir.clone())?;
        let globs = self.sensitive_globs(&dir);
        let root = PathBuf::from(&dir);
        if globs.is_empty() {
            return None;
        }
        let mut paths = vec![];
        match serde_json::from_str::<Value>(input) {
            Ok(v) => collect_paths(&v, &mut paths),
            // Cut short: look at every quoted or bare word instead.
            Err(_) => paths.extend(
                input
                    .split(|c: char| c == '"' || c.is_whitespace())
                    .filter(|w| w.contains('/') || w.contains('.'))
                    .map(str::to_string),
            ),
        }
        paths
            .into_iter()
            .find(|p| privacy::is_sensitive(p, &root, &globs))
    }

    /// Does `to` get only the status of `child_id`'s result? When the work
    /// is local-only, `to` runs outside your network, and `[privacy]
    /// return` is not `summary`.
    pub(super) fn redacts(&self, child_id: &str, to: &str) -> bool {
        let to_cloud = self
            .agent(to)
            .map(|a| a.model.locality != Locality::Local)
            .unwrap_or(true);
        to_cloud
            && self.cfg.privacy.return_mode != ReturnMode::Summary
            && self.local_only_reason(child_id).is_some()
    }

    /// What the sender of a handoff may see of its result: everything,
    /// unless the work is local-only and the sender runs outside your
    /// network.
    pub(super) fn redact_for(&self, child_id: &str, to: &str, full: String) -> String {
        if !self.redacts(child_id, to) {
            return full;
        }
        let key = self
            .flight(child_id)
            .map(|f| f.key.clone())
            .unwrap_or_default();
        format!(
            "Done by a local model ({key}). The work is local-only, so its summary stays out of this conversation; the human can read it in Tower."
        )
    }
}

/// File paths in a tool call, wherever its JSON names one.
fn collect_paths(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let pathy = matches!(
                    k.as_str(),
                    "path" | "file_path" | "filePath" | "abs_path" | "filename" | "file"
                );
                match x {
                    Value::String(s) if pathy => out.push(s.clone()),
                    _ => collect_paths(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_paths(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::collect_paths;
    use serde_json::json;

    #[test]
    fn paths_are_found_anywhere_in_a_tool_call() {
        let v = json!({"toolCall": {"title": "Read", "locations": [{"path": "/w/secrets/a.key"}], "rawInput": {"file_path": "b.txt", "n": 3}}});
        let mut out = vec![];
        collect_paths(&v, &mut out);
        out.sort();
        assert_eq!(out, vec!["/w/secrets/a.key", "b.txt"]);
    }
}
