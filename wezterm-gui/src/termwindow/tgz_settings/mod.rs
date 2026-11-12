//! The Settings tab: fork settings and a few basics, edited in a tab and
//! written back to the user's `wezterm.lua`.
//!
//! - [`catalog`]: which settings there are and how each reads its value.
//! - [`lua_edit`]: the text edit of a Lua config (pure).
//! - [`store`]: reading, checking and writing the file.
//! - [`pane`]: the terminal-less pane the tab is made of, and its state.
//! - [`paint`]: drawing and clicks.

pub(crate) mod catalog;
pub(crate) mod lua_edit;
pub(crate) mod paint;
pub(crate) mod pane;
pub(crate) mod store;

pub(crate) use paint::is_settings_pane;
pub(crate) use pane::SettingsHit;
