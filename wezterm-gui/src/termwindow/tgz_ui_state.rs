//! TGZTerminal-specific persisted UI state.
//!
//! Small, best-effort persistence for runtime UI toggles that must survive an
//! app restart (e.g. the sidebar auto-hide toggle button). Stored as JSON in
//! `config::DATA_DIR` (durable across restarts, unlike `RUNTIME_DIR`). All
//! errors are logged and swallowed — a missing or corrupt state file simply
//! falls back to the Lua config defaults.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// On-disk shape. Fields are optional so absent keys fall back to config
/// defaults, and so new toggles can be added without breaking older files.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TgzUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_auto_hide: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_launcher_project_root: Option<bool>,

    /// Tab indices whose pane children are expanded in the sidebar. Stored as
    /// a plain list because a tab index is the only identity the sidebar row
    /// model has; indices shift when tabs are reordered or closed, so on
    /// restart this is a hint, not a guarantee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_expanded_tabs: Option<Vec<usize>>,

    /// Whether the agent section in the sidebar is collapsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_section_collapsed: Option<bool>,

    /// Agent section scope: `"current"` (current project only) or `"all"`
    /// (every project's agents). `None` falls back to the current-project view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_section_view: Option<String>,
}

fn state_path() -> PathBuf {
    config::DATA_DIR.join("tgz-ui-state.json")
}

fn read_state() -> TgzUiState {
    let path = state_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|err| {
            log::warn!("failed to parse {}: {err:#}", path.display());
            TgzUiState::default()
        }),
        // Missing file is the common first-run case; not worth logging.
        Err(_) => TgzUiState::default(),
    }
}

/// Persisted sidebar auto-hide preference, or `None` when unset (use the Lua
/// config default in that case).
pub fn load_sidebar_auto_hide() -> Option<bool> {
    read_state().sidebar_auto_hide
}

/// Persisted "launch agents at the project root" preference, or `None` when
/// unset (use `agent_ui.launcher.cwd` from the Lua config in that case).
pub fn load_agent_launcher_project_root() -> Option<bool> {
    read_state().agent_launcher_project_root
}

/// Persist the agent launcher project-root preference. Best-effort.
pub fn save_agent_launcher_project_root(value: bool) {
    let mut state = read_state();
    state.agent_launcher_project_root = Some(value);
    write_state(&state);
}

/// Persisted set of tabs whose pane rows are expanded, or `None` when unset.
pub fn load_sidebar_expanded_tabs() -> Option<HashSet<usize>> {
    read_state()
        .sidebar_expanded_tabs
        .map(|tabs| tabs.into_iter().collect())
}

/// Persist the expanded-tab set. Best-effort.
pub fn save_sidebar_expanded_tabs(tabs: &HashSet<usize>) {
    let mut state = read_state();
    // Sorted so the file is stable across writes and easy to eyeball.
    let mut tabs: Vec<usize> = tabs.iter().copied().collect();
    tabs.sort_unstable();
    state.sidebar_expanded_tabs = Some(tabs);
    write_state(&state);
}

/// Persisted agent section collapsed state, or `None` when unset.
pub fn load_agent_section_collapsed() -> Option<bool> {
    read_state().agent_section_collapsed
}

/// Persist the agent section collapsed state. Best-effort.
pub fn save_agent_section_collapsed(value: bool) {
    let mut state = read_state();
    state.agent_section_collapsed = Some(value);
    write_state(&state);
}

/// Persisted agent section view scope, or `None` when unset.
pub fn load_agent_section_view() -> Option<crate::agent_herd::HerdView> {
    match read_state().agent_section_view.as_deref() {
        Some("all") => Some(crate::agent_herd::HerdView::AllGrouped),
        Some("current") => Some(crate::agent_herd::HerdView::CurrentProject),
        _ => None,
    }
}

/// Persist the agent section view scope. Best-effort.
pub fn save_agent_section_view(value: crate::agent_herd::HerdView) {
    let mut state = read_state();
    state.agent_section_view = Some(match value {
        crate::agent_herd::HerdView::CurrentProject => "current".to_string(),
        crate::agent_herd::HerdView::AllGrouped => "all".to_string(),
    });
    write_state(&state);
}

/// Persist the sidebar auto-hide preference. Best-effort; errors are logged.
pub fn save_sidebar_auto_hide(value: bool) {
    let mut state = read_state();
    state.sidebar_auto_hide = Some(value);
    write_state(&state);
}

fn write_state(state: &TgzUiState) {
    let path = state_path();
    let json = match serde_json::to_string_pretty(state) {
        Ok(json) => json,
        Err(err) => {
            log::warn!("failed to serialize tgz ui state: {err:#}");
            return;
        }
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            log::warn!("failed to create {}: {err:#}", parent.display());
            return;
        }
    }
    if let Err(err) = std::fs::write(&path, json) {
        log::warn!("failed to write {}: {err:#}", path.display());
    }
}

/// Build a config-override `Value` seeded from persisted UI state, suitable for
/// `config::overridden_config`. Returns `Value::Null` when nothing is persisted.
///
/// Only top-level config keys belong here. `agent_launcher_project_root` is
/// deliberately absent: it maps to the nested `agent_ui.launcher.cwd` key, and
/// a nested override would replace the user's whole `agent_ui` table. It is
/// read directly by the sidebar instead.
pub fn seed_config_overrides() -> wezterm_dynamic::Value {
    overrides_from_state(&TgzUiState {
        sidebar_auto_hide: load_sidebar_auto_hide(),
        ..TgzUiState::default()
    })
}

fn overrides_from_state(state: &TgzUiState) -> wezterm_dynamic::Value {
    use std::collections::BTreeMap;
    use wezterm_dynamic::Value;

    let mut map: BTreeMap<Value, Value> = BTreeMap::new();
    if let Some(v) = state.sidebar_auto_hide {
        map.insert(
            Value::String("sidebar_auto_hide".to_string()),
            Value::Bool(v),
        );
    }

    if map.is_empty() {
        Value::Null
    } else {
        Value::Object(map.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_dynamic::Value;

    #[test]
    fn default_state_has_no_persisted_toggles() {
        assert_eq!(TgzUiState::default().sidebar_auto_hide, None);
    }

    #[test]
    fn json_round_trip_preserves_sidebar_auto_hide() {
        let state = TgzUiState {
            sidebar_auto_hide: Some(true),
            ..TgzUiState::default()
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sidebar_auto_hide, Some(true));
    }

    #[test]
    fn json_round_trip_preserves_agent_launcher_project_root() {
        let state = TgzUiState {
            agent_launcher_project_root: Some(true),
            ..TgzUiState::default()
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_launcher_project_root, Some(true));
    }

    #[test]
    fn writing_one_toggle_preserves_the_others() {
        // `write_state` re-reads before writing, so the serialized shape must
        // keep every toggle; otherwise saving one would clear the rest.
        let state = TgzUiState {
            sidebar_auto_hide: Some(false),
            agent_launcher_project_root: Some(true),
            sidebar_expanded_tabs: Some(vec![0, 2]),
            agent_section_collapsed: None,
            agent_section_view: None,
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sidebar_auto_hide, Some(false));
        assert_eq!(parsed.agent_launcher_project_root, Some(true));
        assert_eq!(parsed.sidebar_expanded_tabs, Some(vec![0, 2]));
    }

    #[test]
    fn json_round_trip_preserves_sidebar_expanded_tabs() {
        let state = TgzUiState {
            sidebar_expanded_tabs: Some(vec![1, 3, 7]),
            ..TgzUiState::default()
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sidebar_expanded_tabs, Some(vec![1, 3, 7]));
    }

    #[test]
    fn absent_field_deserializes_to_none() {
        let parsed: TgzUiState = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.sidebar_auto_hide, None);
        assert_eq!(parsed.agent_launcher_project_root, None);
        assert_eq!(parsed.sidebar_expanded_tabs, None);
    }

    #[test]
    fn launcher_toggle_is_not_seeded_into_config_overrides() {
        // It maps to a nested agent_ui key; seeding it would clobber the
        // user's agent_ui table. See `seed_config_overrides`.
        let overrides = overrides_from_state(&TgzUiState {
            sidebar_auto_hide: None,
            agent_launcher_project_root: Some(true),
            sidebar_expanded_tabs: Some(vec![0]),
            agent_section_collapsed: None,
            agent_section_view: None,
        });
        assert_eq!(overrides, Value::Null);
    }

    #[test]
    fn corrupt_json_fails_to_parse_so_callers_fall_back_to_default() {
        // Mirrors the fallback in `read_state`: a parse error must be
        // recoverable, not a panic, so a corrupt file just loses the toggle.
        assert!(serde_json::from_str::<TgzUiState>("not json").is_err());
    }

    #[test]
    fn overrides_from_state_is_null_when_nothing_persisted() {
        assert_eq!(overrides_from_state(&TgzUiState::default()), Value::Null);
    }

    #[test]
    fn overrides_from_state_includes_persisted_sidebar_auto_hide() {
        let overrides = overrides_from_state(&TgzUiState {
            sidebar_auto_hide: Some(false),
            ..TgzUiState::default()
        });
        match overrides {
            Value::Object(map) => {
                assert_eq!(
                    map.get_by_str("sidebar_auto_hide"),
                    Some(&Value::Bool(false))
                );
            }
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }
}
