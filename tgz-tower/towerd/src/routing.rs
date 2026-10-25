//! Turning a router decision into a runnable engine.
//!
//! The scoring itself is pure (`tower_core::router`, `tower_core::bandit`);
//! this module knows about `[routing]` engines, their availability and the
//! flags each driver takes for effort and subagents.

use crate::config::{self, Config, EngineCfg};
use std::path::Path;
use tower_core::bandit::Bandit;
use tower_core::router::{Route, Tier};
use tower_proto::{DriverKind, Endpoint, EngineInfo};

pub fn arm(tier: Tier, engine: &str) -> String {
    format!("{tier}:{engine}")
}

pub fn parse_arm(arm: &str) -> Option<(Tier, &str)> {
    let (t, e) = arm.split_once(':')?;
    Some((Tier::parse(t)?, e))
}

/// Every engine of every tier with its availability, low tier first.
pub fn engine_infos(
    cfg: &Config,
    endpoints: &[Endpoint],
    bandit: &Bandit,
    on_path: &dyn Fn(&str) -> bool,
) -> Vec<EngineInfo> {
    let has_env = |v: &str| std::env::var_os(v).is_some();
    let mut out = vec![];
    for t in Tier::ALL {
        for id in cfg.routing.tier(t) {
            let Some(e) = cfg.routing.engines.get(id) else {
                continue;
            };
            let why = config::engine_blocked(e, endpoints, on_path, &has_env);
            out.push(EngineInfo {
                id: id.clone(),
                tier: t.as_str().into(),
                driver: e.driver,
                model: e.model.clone().unwrap_or_default(),
                effort: e.effort.clone().unwrap_or_default(),
                available: why.is_none(),
                why,
                trained: bandit.arms.get(&arm(t, id)).map(|a| a.n).unwrap_or(0),
                locality: cfg.engine_model(e).locality,
            });
        }
    }
    out
}

fn first_available(infos: &[EngineInfo], t: Tier) -> Option<&EngineInfo> {
    infos.iter().find(|i| i.tier == t.as_str() && i.available)
}

/// The engine for `tier`: its first available one, else the nearest tier
/// above (never below `floor`), else the nearest one below it.
pub fn pick(infos: &[EngineInfo], tier: Tier, floor: Tier) -> Option<(Tier, String)> {
    let mut order = vec![tier];
    let mut t = tier;
    while let Some(up) = t.up() {
        order.push(up);
        t = up;
    }
    for low in Tier::ALL.iter().rev() {
        if *low < tier && *low >= floor {
            order.push(*low);
        }
    }
    order
        .into_iter()
        .find_map(|t| first_available(infos, t).map(|i| (t, i.id.clone())))
}

/// Arms the learned policy may choose from: available, at or above `floor`.
pub fn candidates(infos: &[EngineInfo], floor: Tier) -> Vec<String> {
    infos
        .iter()
        .filter(|i| i.available)
        .filter_map(|i| {
            let t = Tier::parse(&i.tier)?;
            (t >= floor).then(|| arm(t, &i.id))
        })
        .collect()
}

/// The effort an agent starts with: role/engine setting first, then the
/// router's when `[routing] effort` is on.
pub fn effort(fixed: Option<&str>, route: &Route, cfg: &Config) -> Option<String> {
    fixed.map(str::to_string).or_else(|| {
        cfg.routing
            .effort
            .then(|| route.effort.as_str().to_string())
    })
}

fn is_claude(command: &[String]) -> bool {
    command
        .first()
        .and_then(|p| Path::new(p).file_stem())
        .map(|s| s == "claude")
        .unwrap_or(false)
}

/// Extra CLI flags for a pane agent. Only Claude Code's are known; other
/// programs get none rather than flags they might reject.
pub fn pane_args(command: &[String], effort: Option<&str>, agent: Option<&str>) -> Vec<String> {
    if !is_claude(command) {
        return vec![];
    }
    let mut out = vec![];
    if let Some(e) = effort {
        out.extend(["--effort".to_string(), e.to_string()]);
    }
    if let Some(a) = agent {
        out.extend(["--agent".to_string(), a.to_string()]);
    }
    out
}

/// ACP session options for effort and subagent, set only if the agent offers
/// them (Claude's adapter does; OpenCode has neither).
pub fn acp_optional(effort: Option<&str>, agent: Option<&str>) -> Vec<(String, String)> {
    let mut out = vec![];
    if let Some(e) = effort {
        out.push(("effort".to_string(), e.to_string()));
    }
    if let Some(a) = agent {
        out.push(("agent".to_string(), a.to_string()));
    }
    out
}

pub fn engine_label(e: &EngineCfg) -> String {
    let first = e.command.first().map(String::as_str);
    // `npx -y @scope/pkg`: the package says more than the runner.
    let prog = match first {
        Some("npx" | "bunx" | "uvx" | "pnpx") => e
            .command
            .iter()
            .skip(1)
            .find(|a| !a.starts_with('-'))
            .map(String::as_str)
            .unwrap_or("npx"),
        Some(p) => p,
        None if e.driver == DriverKind::Llm => "llm",
        None => "?",
    };
    match &e.model {
        Some(m) => format!("{prog} {m}"),
        None => prog.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, tier: &str, available: bool) -> EngineInfo {
        EngineInfo {
            id: id.into(),
            tier: tier.into(),
            available,
            ..Default::default()
        }
    }

    #[test]
    fn picking_prefers_the_tier_then_up_then_down_to_the_floor() {
        let infos = vec![
            info("qwen", "low", false),
            info("haiku", "low", true),
            info("sonnet", "medium", false),
            info("opus", "high", true),
        ];
        assert_eq!(
            pick(&infos, Tier::Low, Tier::Low),
            Some((Tier::Low, "haiku".into()))
        );
        assert_eq!(
            pick(&infos, Tier::Medium, Tier::Low),
            Some((Tier::High, "opus".into()))
        );
        let only_low = vec![info("haiku", "low", true)];
        assert_eq!(
            pick(&only_low, Tier::High, Tier::Low),
            Some((Tier::Low, "haiku".into()))
        );
        // Never below the risk floor.
        assert_eq!(pick(&only_low, Tier::High, Tier::Medium), None);
        assert_eq!(
            candidates(&infos, Tier::Medium),
            vec!["high:opus".to_string()]
        );
        assert_eq!(parse_arm("medium:a:b"), Some((Tier::Medium, "a:b")));
    }

    #[test]
    fn flags_only_for_programs_that_take_them() {
        let claude = vec!["/opt/bin/claude".to_string()];
        assert_eq!(
            pane_args(&claude, Some("high"), Some("reviewer")),
            vec!["--effort", "high", "--agent", "reviewer"]
        );
        assert!(pane_args(&["aider".to_string()], Some("high"), None).is_empty());
        assert_eq!(
            acp_optional(Some("low"), None),
            vec![("effort".to_string(), "low".to_string())]
        );
    }

    #[test]
    fn seeded_engines_and_effort() {
        let d = std::env::temp_dir().join(format!("tower-route-{}", crate::paths::random_u64()));
        config::seed(&d).unwrap();
        let cfg = config::load(&d);
        let infos = engine_infos(&cfg, &[], &Bandit::default(), &|p| p == "claude");
        let ids: Vec<_> = infos
            .iter()
            .map(|i| (i.tier.as_str(), i.id.as_str(), i.available))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("low", "qwen-local", false),
                ("low", "claude-haiku", false),
                ("medium", "claude-sonnet", true),
                ("medium", "opencode-kimi", false),
                ("high", "claude-opus", true),
            ]
        );
        let r = tower_core::router::classify("fix a typo");
        assert_eq!(
            pick(&infos, r.tier, r.floor),
            Some((Tier::Medium, "claude-sonnet".into()))
        );
        assert_eq!(effort(None, &r, &cfg).as_deref(), Some("low"));
        assert_eq!(effort(Some("max"), &r, &cfg).as_deref(), Some("max"));
        assert_eq!(
            engine_label(&cfg.routing.engines["claude-haiku"]),
            "@agentclientprotocol/claude-agent-acp haiku"
        );
    }
}
