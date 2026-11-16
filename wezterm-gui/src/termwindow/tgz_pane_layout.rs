//! Pane layouts for the active tab (TGZTerminal).
//!
//! The rearranging itself is `mux::tab::Tab::apply_layout`; this is the
//! window's side of it: topping a tab up with shells when a preset wants more
//! panes than the tab has, opening a `tgz_layouts` entry as a tab of its own,
//! opening the layout menu from the keyboard, and saying so when a window is
//! too small for the panes it holds.

use super::{SidebarContextMenuKind, SidebarContextMenuState, TermWindow, UIItemType};
use crate::spawn::SpawnWhere;
use ::window::WindowOps;
use config::keyassignment::{PaneLayout, SpawnCommand, SpawnTabDomain};
use config::TermConfig;
use mux::tab::{SplitDirection, SplitRequest};
use mux::Mux;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn toast(message: String) {
    crate::termwindow::tgz_notify::status(wezterm_toast_notification::ToastNotification {
        title: "Pane layout".to_string(),
        message,
        url: None,
        timeout: Some(Duration::from_millis(2500)),
    });
}

fn apply_or_say_why(tab: &mux::tab::Tab, layout: PaneLayout) {
    if !tab.apply_layout(layout) {
        toast(format!(
            "The window is too small for {} as {}",
            match tab.count_panes() {
                Some(n) => format!("{n} panes"),
                None => "these panes".to_string(),
            },
            layout.label().to_lowercase()
        ));
    }
}

impl TermWindow {
    pub(crate) fn balance_panes(&mut self) {
        if let Some(tab) = Mux::get().get_active_tab_for_window(self.mux_window_id) {
            tab.balance_panes();
        }
    }

    /// Rearranges the active tab as `layout`. A tab with fewer panes than the
    /// layout needs first gets shells, in the active pane's directory and
    /// domain, as an ordinary split would; the pane that was active keeps
    /// the main slot.
    pub(crate) fn apply_pane_layout(&mut self, layout: PaneLayout) {
        let mux = Mux::get();
        let Some(tab) = mux.get_active_tab_for_window(self.mux_window_id) else {
            return;
        };
        let count = tab.count_panes().unwrap_or(1);
        let missing = layout.min_panes().saturating_sub(count);
        if missing == 0 {
            apply_or_say_why(&tab, layout);
            return;
        }
        let Some(main) = tab.get_active_pane() else {
            return;
        };

        let spawn = SpawnCommand {
            domain: SpawnTabDomain::CurrentPaneDomain,
            cwd: self.wsl_inherited_cwd(&SpawnTabDomain::CurrentPaneDomain),
            ..Default::default()
        };
        let size = self.terminal_size;
        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
        let window_id = self.mux_window_id;
        let tab_id = tab.tab_id();

        promise::spawn::spawn(async move {
            for i in 0..missing {
                // Alternate the direction so a narrow tab does not run out of
                // columns before the layout takes over the sizing.
                let direction = if i % 2 == 0 {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                };
                let added = crate::spawn::spawn_command_internal(
                    spawn.clone(),
                    SpawnWhere::SplitPane(SplitRequest {
                        direction,
                        ..Default::default()
                    }),
                    size,
                    Some(window_id),
                    Arc::clone(&term_config),
                )
                .await;
                if let Err(err) = added {
                    log::error!("pane layout: adding a pane failed: {err:#}");
                    toast(format!("Could not add a pane: {err:#}"));
                    break;
                }
            }
            if let Some(tab) = Mux::get().get_tab(tab_id) {
                tab.set_active_pane(&main);
                apply_or_say_why(&tab, layout);
            }
        })
        .detach();
    }

    /// Opens the `tgz_layouts` entry called `name` in a new tab: its first
    /// pane as the tab, the others split off it, then arranged as its preset
    /// with the first pane in the main slot.
    pub(crate) fn open_user_pane_layout(&mut self, name: &str) {
        let Some(def) = self
            .config
            .tgz_layouts
            .iter()
            .find(|def| def.name == name)
            .cloned()
        else {
            toast(format!("No layout named `{name}` in tgz_layouts"));
            return;
        };
        if def.panes.is_empty() {
            toast(format!("The layout `{name}` has no panes"));
            return;
        }

        let base_cwd = self.user_layout_cwd(def.cwd);
        let panes: Vec<SpawnCommand> = def
            .panes
            .iter()
            .map(|pane| SpawnCommand {
                cwd: pane.cwd.clone().or_else(|| base_cwd.clone()),
                ..pane.clone()
            })
            .collect();
        let size = self.terminal_size;
        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
        let window_id = self.mux_window_id;
        let tab_ids = |window_id| -> Vec<mux::tab::TabId> {
            Mux::get()
                .get_window(window_id)
                .map(|window| window.iter_tabs().map(|tab| tab.tab_id()).collect())
                .unwrap_or_default()
        };
        let before = tab_ids(window_id);

        promise::spawn::spawn(async move {
            let mut panes = panes.into_iter();
            let Some(first) = panes.next() else {
                return;
            };
            let opened = crate::spawn::spawn_command_internal(
                first,
                SpawnWhere::NewTab,
                size,
                Some(window_id),
                Arc::clone(&term_config),
            )
            .await;
            if let Err(err) = opened {
                toast(format!("Could not open `{}`: {err:#}", def.name));
                return;
            }
            let mux = Mux::get();
            let Some(tab) = tab_ids(window_id)
                .into_iter()
                .find(|id| !before.contains(id))
                .and_then(|id| mux.get_tab(id))
            else {
                return;
            };
            let Some(main) = tab.get_active_pane() else {
                return;
            };
            for (i, pane) in panes.enumerate() {
                // Splits always come off the active pane of the window's
                // active tab, which the new tab is until someone clicks away.
                let direction = if i % 2 == 0 {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                };
                let added = crate::spawn::spawn_command_internal(
                    pane,
                    SpawnWhere::SplitPane(SplitRequest {
                        direction,
                        ..Default::default()
                    }),
                    size,
                    Some(window_id),
                    Arc::clone(&term_config),
                )
                .await;
                if let Err(err) = added {
                    toast(format!("Could not open a pane of `{}`: {err:#}", def.name));
                    break;
                }
            }
            tab.set_active_pane(&main);
            apply_or_say_why(&tab, def.layout);
        })
        .detach();
    }

    /// The directory a user layout's panes start in, by its `cwd` rule.
    /// `None` lets the domain decide.
    fn user_layout_cwd(&self, rule: config::AgentLauncherCwd) -> Option<PathBuf> {
        let raw = self
            .wsl_active_pane_session_cwd()
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .or_else(|| super::composer::active_pane_cwd(self))?;
        Some(match rule {
            config::AgentLauncherCwd::ActivePane => PathBuf::from(raw),
            config::AgentLauncherCwd::ProjectRoot => {
                let distro = self
                    .active_pane_domain_name()
                    .and_then(|name| super::wsl_paths::distro_for_domain(&name, &self.config));
                self.project_root_for(&raw, distro.as_deref())
            }
        })
    }

    /// Opens the layout menu at the footer's Layouts button, or at the
    /// window's corner when the footer has none. Asking again closes it.
    pub(crate) fn show_pane_layout_menu(&mut self) {
        let was_open = matches!(
            self.sidebar_context_menu,
            Some(SidebarContextMenuState {
                kind: SidebarContextMenuKind::PaneLayouts,
                ..
            })
        );
        self.close_floating_menus();
        if !was_open {
            let (x, y) = self
                .ui_items
                .iter()
                .find(|item| {
                    item.item_type
                        == UIItemType::SidebarFooterAction(config::SidebarFooterItem::Layouts)
                })
                .map_or((8, 40), |item| (item.x, item.y));
            self.sidebar_context_menu = Some(SidebarContextMenuState {
                x,
                y,
                kind: SidebarContextMenuKind::PaneLayouts,
            });
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }
}
