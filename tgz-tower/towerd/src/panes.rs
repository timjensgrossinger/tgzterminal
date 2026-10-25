//! CLIs you start in ordinary TGZTerminal panes: they reach Tower through
//! the user-wide MCP entry ("Connect agents") and may use the delegation
//! tools. Tower adopts one as an agent on its first tool call, gives it a
//! flight on its first handoff, and never types into its pane: results are
//! fetched with `tower_await`.
//!
//! A child module of `engine`, so it works on the engine's own state.

use super::*;
use tower_proto::PaneAgentInfo;

/// The tools a pane agent gets.
pub const PANE_TOOLS: [&str; 3] = ["tower_models", "tower_delegate", "tower_await"];

/// `claude`, `codex`, … from a process name; anything odd becomes `agent`.
pub fn vendor_slug(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let slug: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(20)
        .collect::<String>()
        .to_ascii_lowercase();
    if slug.is_empty() {
        "agent".into()
    } else {
        slug
    }
}

impl Engine {
    pub(super) fn on_pane_tool(&mut self, p: PaneAgentInfo, name: &str, args: &Value) -> Res {
        if !PANE_TOOLS.contains(&name) {
            return Err(format!(
                "{name} is for agents Tower launched. In an ordinary pane you have tower_models, tower_delegate and tower_await."
            ));
        }
        if name == "tower_models" {
            return self.tool_models();
        }
        let id = self.adopt_pane_agent(&p);
        if name == "tower_delegate" {
            self.give_pane_flight(&id, &p);
        }
        if self.agent(&id).and_then(|a| a.flight_id.clone()).is_none() {
            return Err("You have not handed anything over yet: use tower_delegate first".into());
        }
        self.on_tool(&id, name, args)
    }

    /// The agent for this CLI process, made on first contact. A new process
    /// in the same pane means the old one ended.
    fn adopt_pane_agent(&mut self, p: &PaneAgentInfo) -> String {
        let vendor = vendor_slug(&p.vendor);
        let id = format!("{vendor}-pane{}-{}", p.pane_id, p.pid);
        if self.agent(&id).is_some_and(|a| a.status.is_live()) {
            return id;
        }
        for a in self.st.agents.iter_mut() {
            if a.adopted && a.pane_id == Some(p.pane_id) && a.id != id {
                a.status = AgentStatus::Offline;
            }
        }
        self.st.agents.retain(|a| a.id != id);
        self.st.agents.push(Agent {
            id: id.clone(),
            role_id: vendor.clone(),
            status: AgentStatus::Working,
            model: ModelRef {
                id: String::new(),
                label: vendor,
                locality: Locality::Cloud,
                host: None,
            },
            driver: DriverKind::Pane,
            pane_id: Some(p.pane_id),
            started_unix: now_unix(),
            adopted: true,
            activity: Some("Working in its own pane".into()),
            ..Default::default()
        });
        self.changed();
        id
    }

    /// Every 30 s: adopted CLIs whose process has ended go offline, and
    /// their flight lands. Tower spawned none of them, so no pane event
    /// tells it.
    pub(super) fn check_adopted_panes(&mut self) {
        if self
            .last_pane_check
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.last_pane_check = Some(Instant::now());
        let gone: Vec<(String, Option<String>)> = self
            .st
            .agents
            .iter()
            .filter(|a| a.adopted && a.status.is_live())
            .filter(|a| {
                a.id.rsplit('-')
                    .next()
                    .and_then(|p| p.parse::<u32>().ok())
                    .is_some_and(|pid| !crate::local_guard::process_alive(pid))
            })
            .map(|a| (a.id.clone(), a.flight_id.clone()))
            .collect();
        for (id, flight) in gone {
            if let Some(a) = self.agent_mut(&id) {
                a.status = AgentStatus::Offline;
            }
            if let Some(f) = flight {
                self.note(&f, "The session ended");
                self.set_stage(&f, Stage::Landed);
            }
            self.changed();
        }
    }

    /// A flight for a pane agent's handoffs to hang off, once.
    fn give_pane_flight(&mut self, agent_id: &str, p: &PaneAgentInfo) {
        if self
            .agent(agent_id)
            .and_then(|a| a.flight_id.clone())
            .is_some()
        {
            return;
        }
        let folder = std::path::Path::new(&p.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| p.cwd.clone());
        let vendor = self
            .agent(agent_id)
            .map(|a| a.role_id.clone())
            .unwrap_or_default();
        self.st.counters.flight += 1;
        let now = now_unix();
        let n = self.st.counters.flight;
        let f = Flight {
            id: format!("f-{n}"),
            key: format!("TWR-{n}"),
            title: format!("{vendor} in pane {}: {folder}", p.pane_id),
            brief: format!(
                "A {vendor} session you started yourself in pane {}. Its handoffs to local models hang off this flight.",
                p.pane_id
            ),
            stage: Stage::InFlight,
            agent_id: Some(agent_id.to_string()),
            source: Some("pane".into()),
            created_unix: now,
            updated_unix: now,
            workdir: std::path::Path::new(&p.cwd)
                .is_dir()
                .then(|| p.cwd.clone()),
            ..Default::default()
        };
        let fid = self.push_flight(f, "pane");
        if let Some(a) = self.agent_mut(agent_id) {
            a.flight_id = Some(fid);
        }
        self.changed();
    }

    /// "Connect agents" / "Disconnect".
    /// Once after start: when the CLIs were connected, refresh Tower's own
    /// copy of the binary (this build) and point their entries at it.
    pub(super) fn refresh_connection(&mut self) {
        if self.connection_refreshed {
            return;
        }
        self.connection_refreshed = true;
        if self.demo.is_none() && !self.connected_clis.is_empty() {
            if let Err(e) = self.apply_connection(true) {
                self.post_system(&format!("Connect agents: {e}"), now_unix());
            }
        }
    }

    pub(super) fn connect_agents(&mut self, on: bool) -> Res {
        let changed = self.apply_connection(on)?;
        let what = if changed.is_empty() {
            "Nothing to change".to_string()
        } else {
            changed.join(" and ")
        };
        self.post_system(
            &if on {
                format!("Connected: {what}. New Claude and Codex sessions in TGZTerminal panes can hand work to local models, and stay out of local-only folders.")
            } else {
                format!("Disconnected: {what}.")
            },
            now_unix(),
        );
        Ok(what)
    }

    /// Write (or remove) the MCP entries and Claude's guard hooks; returns
    /// what changed.
    fn apply_connection(&mut self, on: bool) -> Result<Vec<&'static str>, String> {
        let exe = if on {
            crate::connect::install_exe(&self.paths.exe, &self.paths.data)?
                .display()
                .to_string()
        } else {
            String::new()
        };
        let want = on.then_some(exe.as_str());
        let mut changed = vec![];
        let mut errors = vec![];
        if let Some(p) = crate::claude_trust::config_path() {
            let installed = p.exists();
            match crate::connect::apply_claude(&p, want) {
                Ok(true) => changed.push("Claude Code"),
                Ok(false) => {}
                Err(e) => errors.push(e),
            }
            // The guard hooks keep a Claude session out of local-only work.
            let hooks = crate::connect::claude_settings_path()
                .filter(|_| installed || !on)
                .map(|s| crate::connect::apply_claude_hooks(&s, want));
            match hooks {
                Some(Ok(true)) => changed.push("Claude Code's local-only guard"),
                Some(Err(e)) => errors.push(e),
                _ => {}
            }
        }
        let codex_wanted = !on || (self.on_path)("codex");
        if let Some(p) = crate::connect::codex_config_path().filter(|_| codex_wanted) {
            match crate::connect::apply_codex(&p, want) {
                Ok(true) => changed.push("Codex"),
                Ok(false) => {}
                Err(e) => errors.push(e),
            }
        }
        self.connected_clis = crate::connect::status();
        self.changed();
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::vendor_slug;

    #[test]
    fn vendors_are_named_from_the_process() {
        assert_eq!(vendor_slug("claude"), "claude");
        assert_eq!(vendor_slug("/opt/homebrew/bin/codex"), "codex");
        assert_eq!(vendor_slug("Claude Code"), "claudecode");
        assert_eq!(vendor_slug("../$(rm -rf)"), "rm-rf");
        assert_eq!(vendor_slug(""), "agent");
    }
}
