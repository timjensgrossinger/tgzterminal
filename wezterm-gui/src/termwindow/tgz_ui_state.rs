//! TGZTerminal-specific persisted UI state.
//!
//! Small, best-effort persistence for runtime UI toggles that must survive an
//! app restart (e.g. the sidebar auto-hide toggle button). Stored as JSON in
//! `config::DATA_DIR` (durable across restarts, unlike `RUNTIME_DIR`). All
//! errors are logged and swallowed — a missing or corrupt state file simply
//! falls back to the Lua config defaults.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// On-disk shape. Fields are optional so absent keys fall back to config
/// defaults, and so new toggles can be added without breaking older files.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TgzUiState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sidebar_auto_hide: Option<bool>,
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

/// Persist the sidebar auto-hide preference. Best-effort; errors are logged.
pub fn save_sidebar_auto_hide(value: bool) {
    let path = state_path();
    let mut state = read_state();
    state.sidebar_auto_hide = Some(value);

    let json = match serde_json::to_string_pretty(&state) {
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
pub fn seed_config_overrides() -> wezterm_dynamic::Value {
    use std::collections::BTreeMap;
    use wezterm_dynamic::Value;

    let mut map: BTreeMap<Value, Value> = BTreeMap::new();
    if let Some(v) = load_sidebar_auto_hide() {
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
