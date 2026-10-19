//! Tower: an experimental agent-team view, painted as a GUI inside a tab.
//!
//! Compiled into wezterm-gui only with `TGZ_TOWER=1` and this folder present
//! (see `experimental_tab_views` in wezterm-gui/build.rs). The seam calls the
//! four functions re-exported below; everything else stays in this folder.
//!
//! Layout: `proto` (shared wire types), pure `clip`/`theme`/`text`/`layout`/
//! `model`, and GPU/host glue in `fonts`/`render`/`pane`/`client`/`glue`.

#![allow(dead_code)]

#[path = "../proto/src/lib.rs"]
pub mod proto;

mod client;
mod clip;
mod fonts;
mod forms;
mod glue;
mod layout;
mod model;
mod pane;
mod render;
mod text;
mod theme;

pub use glue::{is_tower_pane, on_emit_event, open, paint};
