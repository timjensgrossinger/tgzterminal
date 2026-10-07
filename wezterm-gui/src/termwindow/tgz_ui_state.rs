//! TGZTerminal-specific persisted UI state.
//!
//! Small, best-effort persistence for runtime UI toggles that must survive an
//! app restart (e.g. the sidebar auto-hide toggle button). Stored as JSON in
//! `config::DATA_DIR` (durable across restarts, unlike `RUNTIME_DIR`). All
//! errors are logged and swallowed — a missing or corrupt state file simply
//! falls back to the Lua config defaults.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

/// On-disk shape. Fields are optional so absent keys fall back to config
/// defaults, and so new toggles can be added without breaking older files.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TgzUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_auto_hide: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_launcher_project_root: Option<bool>,

    /// Domain names of recently used SSH quick-launch hosts, most recent
    /// first. Names only: no addresses, users or keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh_recent_hosts: Option<Vec<String>>,

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

    /// WSL distros agents have run in (Windows only). The resume menus keep
    /// scanning these after their last pane closes, and after a restart, so a
    /// finished session stays listed. See `wsl_paths::remember_session_distros`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wsl_session_distros: Option<Vec<String>>,

    /// Dragged width of the Changes panel, in physical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_panel_width: Option<usize>,

    /// Dragged height of the Changes panel in physical pixels; 0 means it is
    /// snapped to the bottom. `None` when the user has never dragged it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_panel_height: Option<usize>,

    /// Dragged position of the agent pane toolbelt as `[right, top]`: the
    /// distance of the strip's top-right corner from the pane's top-right
    /// corner, in DPI-independent pixels. `None` keeps the configured spot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pane_toolbelt_offset: Option<[f32; 2]>,

    /// Whether the agent pane toolbelt is hidden from the sidebar toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pane_toolbelt_hidden: Option<bool>,
}

fn state_path() -> PathBuf {
    config::DATA_DIR.join(config::brand::state_file_name("tgz-ui-state", "json"))
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

/// Recently used SSH quick-launch hosts, most recent first; empty when unset.
pub fn load_ssh_recent_hosts() -> Vec<String> {
    read_state().ssh_recent_hosts.unwrap_or_default()
}

/// Persist the recently used SSH quick-launch hosts. Best-effort.
pub fn save_ssh_recent_hosts(hosts: &[String]) {
    let mut state = read_state();
    state.ssh_recent_hosts = Some(hosts.to_vec());
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

/// Persisted WSL distros agents have run in; empty when unset.
pub fn load_wsl_session_distros() -> Vec<String> {
    read_state().wsl_session_distros.unwrap_or_default()
}

/// Persist the WSL distros agents have run in. Best-effort.
pub fn save_wsl_session_distros(distros: &BTreeSet<String>) {
    let mut state = read_state();
    // A `BTreeSet` iterates sorted, so the file stays stable across writes.
    state.wsl_session_distros = Some(distros.iter().cloned().collect());
    write_state(&state);
}

/// Persist the sidebar auto-hide preference. Best-effort; errors are logged.
pub fn save_sidebar_auto_hide(value: bool) {
    *PERSISTED_SIDEBAR_AUTO_HIDE.lock().unwrap() = Some(Some(value));
    let mut state = read_state();
    state.sidebar_auto_hide = Some(value);
    write_state(&state);
}

/// Persisted Changes panel size as `(width, height)`; a height of 0 means
/// "snapped to the bottom". Either is `None` until the user drags that edge.
pub fn load_diff_panel_size() -> (Option<usize>, Option<usize>) {
    let state = read_state();
    (state.diff_panel_width, state.diff_panel_height)
}

/// Persist the Changes panel size. Best-effort.
pub fn save_diff_panel_size(width: Option<usize>, height: Option<usize>) {
    let mut state = read_state();
    state.diff_panel_width = width;
    state.diff_panel_height = height;
    write_state(&state);
}

/// Persisted toolbelt position as `[right, top]`, or `None` when the user has
/// never dragged it (or reset it). Non-finite values are dropped.
pub fn load_pane_toolbelt_offset() -> Option<[f32; 2]> {
    read_state()
        .pane_toolbelt_offset
        .filter(|[right, top]| right.is_finite() && top.is_finite())
}

/// Persist the toolbelt position; `None` clears it. Best-effort.
pub fn save_pane_toolbelt_offset(offset: Option<[f32; 2]>) {
    let mut state = read_state();
    state.pane_toolbelt_offset = offset;
    write_state(&state);
}

/// Persisted toolbelt hidden flag, or `None` when unset.
pub fn load_pane_toolbelt_hidden() -> Option<bool> {
    read_state().pane_toolbelt_hidden
}

/// Persist the toolbelt hidden flag. Best-effort.
pub fn save_pane_toolbelt_hidden(value: bool) {
    let mut state = read_state();
    state.pane_toolbelt_hidden = Some(value);
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

/// `value` with the persisted UI toggles filled back in where it does not set
/// them itself.
///
/// Lua's `window:set_config_overrides` replaces a window's overrides
/// wholesale, so a script that only meant to change the font size also threw
/// away the seeded `sidebar_auto_hide` and the sidebar silently reverted to
/// the Lua default until the next restart.
///
/// Lua commonly calls `set_config_overrides` from `update-status`, i.e. per
/// repaint, so the persisted value is read from disk once per process and
/// kept current by `save_sidebar_auto_hide` rather than re-read here.
pub fn reapply_persisted_overrides(value: wezterm_dynamic::Value) -> wezterm_dynamic::Value {
    let sidebar_auto_hide = *PERSISTED_SIDEBAR_AUTO_HIDE
        .lock()
        .unwrap()
        .get_or_insert_with(load_sidebar_auto_hide);
    overlay_state(
        value,
        &TgzUiState {
            sidebar_auto_hide,
            ..TgzUiState::default()
        },
    )
}

/// In-memory copy of the persisted `sidebar_auto_hide`: outer `None` = not
/// loaded yet, inner `None` = nothing persisted.
static PERSISTED_SIDEBAR_AUTO_HIDE: std::sync::Mutex<Option<Option<bool>>> =
    std::sync::Mutex::new(None);

fn overlay_state(value: wezterm_dynamic::Value, state: &TgzUiState) -> wezterm_dynamic::Value {
    use wezterm_dynamic::Value;

    let Value::Object(persisted) = overrides_from_state(state) else {
        return value;
    };
    match value {
        Value::Null => Value::Object(persisted),
        Value::Object(explicit) => {
            let mut merged = persisted;
            for (key, val) in explicit.iter() {
                merged.insert(key.clone(), val.clone());
            }
            Value::Object(merged)
        }
        other => other,
    }
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
    fn explicit_overrides_keep_the_persisted_toggle_unless_they_set_it() {
        use std::collections::BTreeMap;
        let key = || Value::String("sidebar_auto_hide".to_string());
        let state = TgzUiState {
            sidebar_auto_hide: Some(true),
            ..TgzUiState::default()
        };
        let font = BTreeMap::from([(Value::String("font_size".into()), Value::F64(14.0.into()))]);
        let merged = overlay_state(Value::Object(font.into()), &state);
        let Value::Object(map) = &merged else {
            panic!("expected an object, got {merged:?}");
        };
        assert_eq!(map.get(&key()), Some(&Value::Bool(true)));
        assert!(map.get(&Value::String("font_size".into())).is_some());

        let explicit = BTreeMap::from([(key(), Value::Bool(false))]);
        let merged = overlay_state(Value::Object(explicit.into()), &state);
        let Value::Object(map) = &merged else {
            panic!("expected an object, got {merged:?}");
        };
        assert_eq!(map.get(&key()), Some(&Value::Bool(false)));

        assert_eq!(
            overlay_state(Value::Null, &TgzUiState::default()),
            Value::Null
        );
    }

    #[test]
    fn a_file_without_toolbelt_keys_loads_with_the_toolbelt_at_its_default() {
        let parsed: TgzUiState = serde_json::from_str(r#"{"sidebar_auto_hide":true}"#).unwrap();
        assert_eq!(parsed.pane_toolbelt_offset, None);
        assert_eq!(parsed.pane_toolbelt_hidden, None);
    }

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
            ssh_recent_hosts: None,
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
            ssh_recent_hosts: None,
            sidebar_expanded_tabs: Some(vec![0, 2]),
            agent_section_collapsed: None,
            agent_section_view: None,
            wsl_session_distros: Some(vec!["Ubuntu".into()]),
            diff_panel_width: Some(640),
            diff_panel_height: Some(0),
            pane_toolbelt_offset: Some([120.0, 300.0]),
            pane_toolbelt_hidden: Some(true),
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sidebar_auto_hide, Some(false));
        assert_eq!(parsed.diff_panel_width, Some(640));
        assert_eq!(parsed.diff_panel_height, Some(0));
        assert_eq!(parsed.pane_toolbelt_offset, Some([120.0, 300.0]));
        assert_eq!(parsed.pane_toolbelt_hidden, Some(true));
        assert_eq!(parsed.agent_launcher_project_root, Some(true));
        assert_eq!(parsed.sidebar_expanded_tabs, Some(vec![0, 2]));
        assert_eq!(parsed.wsl_session_distros, Some(vec!["Ubuntu".into()]));
    }

    #[test]
    fn json_round_trip_preserves_wsl_session_distros() {
        let state = TgzUiState {
            wsl_session_distros: Some(vec!["Debian".into(), "Ubuntu-24.04".into()]),
            ..TgzUiState::default()
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: TgzUiState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.wsl_session_distros,
            Some(vec!["Debian".into(), "Ubuntu-24.04".into()])
        );
        // A file written before the key existed still parses.
        let old: TgzUiState = serde_json::from_str(r#"{"sidebar_auto_hide":true}"#).unwrap();
        assert_eq!(old.wsl_session_distros, None);
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
            ssh_recent_hosts: None,
            sidebar_expanded_tabs: Some(vec![0]),
            agent_section_collapsed: None,
            agent_section_view: None,
            wsl_session_distros: Some(vec!["Ubuntu".into()]),
            diff_panel_width: Some(640),
            diff_panel_height: None,
            // Toolbelt state is window state, not config: never an override.
            pane_toolbelt_offset: Some([10.0, 20.0]),
            pane_toolbelt_hidden: Some(true),
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
