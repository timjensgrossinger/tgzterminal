//! Turning a clicked OS notification into "take me to the pane that sent it".
//!
//! A notification click arrives from the OS on an arbitrary thread, long after
//! the notification was posted, and carries nothing but a `PaneId`. Everything
//! interesting — which window holds that pane, whether the pane still exists,
//! whether its workspace is even on screen — has to be resolved at click time,
//! not at post time: panes move between windows and they die.

use crate::frontend::{front_end, try_front_end, WorkspaceSwitcher};
use crate::termwindow::TermWindowNotif;
use ::window::{Connection, ConnectionOps, WindowOps};
use mux::pane::PaneId;
use mux::window::WindowId as MuxWindowId;
use mux::Mux;
use std::sync::Arc;
use wezterm_toast_notification::ToastClick;

/// A click handler for a notification produced by `pane_id`.
///
/// Safe to build from any thread — it captures nothing but a `PaneId`, which is
/// `Copy` — and the handler itself does nothing except hop to the GUI thread,
/// because that is the only thread allowed to touch the frontend.
pub fn focus_pane_on_click(pane_id: PaneId) -> ToastClick {
    Arc::new(move || {
        promise::spawn::spawn_into_main_thread(async move {
            focus_pane(pane_id);
        })
        .detach();
    })
}

/// What we intend to do about a click, decided from four plain facts.
///
/// Split out from the doing so the decision can be tested without a mux, a
/// frontend, or a window server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FocusPlan {
    /// The pane died while the banner sat in Notification Centre.
    PaneGone,
    /// The happy path: raise this window and activate the pane inside it.
    Raise(MuxWindowId),
    /// The pane's window belongs to a workspace that is not on screen.
    SwitchWorkspace {
        workspace: String,
        mux_window_id: MuxWindowId,
    },
    /// The pane is in the active workspace but has no GUI window of its own
    /// (mux client, or a window not yet reconciled). Move mux focus and hope.
    MuxOnly(MuxWindowId),
}

/// `resolved` is the pane's mux window and that window's workspace, or `None`
/// when the pane could not be resolved at all.
pub(crate) fn plan_focus(
    pane_exists: bool,
    resolved: Option<(MuxWindowId, String)>,
    active_workspace: &str,
    has_gui_window: bool,
) -> FocusPlan {
    // A pane that is gone outranks a stale resolution: acting on the latter
    // would focus whatever inherited its slot.
    if !pane_exists {
        return FocusPlan::PaneGone;
    }
    let Some((mux_window_id, workspace)) = resolved else {
        return FocusPlan::PaneGone;
    };
    if workspace != active_workspace {
        return FocusPlan::SwitchWorkspace {
            workspace,
            mux_window_id,
        };
    }
    if has_gui_window {
        FocusPlan::Raise(mux_window_id)
    } else {
        FocusPlan::MuxOnly(mux_window_id)
    }
}

/// GUI thread only.
fn focus_pane(pane_id: PaneId) {
    let Some(mux) = Mux::try_get() else {
        return;
    };
    let Some(fe) = try_front_end() else {
        // Clicked before the frontend exists, or after it went away.
        return;
    };

    let resolved = mux
        .resolve_pane_id(pane_id)
        .and_then(|(_domain, win, _tab)| {
            mux.get_window(win)
                .map(|window| (win, window.get_workspace().to_string()))
        });
    let has_gui_window = resolved
        .as_ref()
        .map(|(win, _)| fe.gui_window_for_mux_window(*win).is_some())
        .unwrap_or(false);

    let plan = plan_focus(
        mux.get_pane(pane_id).is_some(),
        resolved,
        &mux.active_workspace(),
        has_gui_window,
    );

    match plan {
        FocusPlan::PaneGone => {
            log::debug!("notification click: pane {pane_id} is gone");
        }
        FocusPlan::Raise(mux_window_id) => raise_and_activate(mux_window_id, pane_id),
        FocusPlan::SwitchWorkspace {
            workspace,
            mux_window_id,
        } => {
            // Dropping the switcher performs the switch and kicks off
            // reconciliation; the window we want only exists afterwards.
            WorkspaceSwitcher::new(&workspace).do_switch();
            promise::spawn::spawn(async move {
                let _ = front_end().reconcile_workspace().await;
                raise_and_activate(mux_window_id, pane_id);
            })
            .detach();
        }
        FocusPlan::MuxOnly(_) => {
            if let Err(err) = mux.focus_pane_and_containing_tab(pane_id) {
                log::warn!("notification click: could not focus pane {pane_id}: {err:#}");
            }
        }
    }
}

/// GUI thread only.
fn raise_and_activate(mux_window_id: MuxWindowId, pane_id: PaneId) {
    let Some(gui_win) = try_front_end().and_then(|fe| fe.gui_window_for_mux_window(mux_window_id))
    else {
        if let Err(err) = Mux::get().focus_pane_and_containing_tab(pane_id) {
            log::warn!("notification click: could not focus pane {pane_id}: {err:#}");
        }
        return;
    };

    // Switch first, raise second: both land on the same main-thread queue, so
    // the window is already showing the right pane when it comes forward.
    gui_win
        .window
        .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
            unzoom_for_focus(pane_id);
            if !term_window.activate_sidebar_pane(pane_id) {
                // The pane moved to another window between resolving and here.
                if let Err(err) = Mux::get().focus_pane_and_containing_tab(pane_id) {
                    log::warn!("notification click: could not focus pane {pane_id}: {err:#}");
                }
            }
        })));

    // The app has to come forward before raising a window inside it does
    // anything visible.
    if let Some(conn) = Connection::get() {
        conn.activate_application();
    }
    gui_win.window.focus();
}

/// `Tab::set_active_pane` refuses to move focus out of a zoomed pane when
/// `unzoom_on_switch_pane` is off, which would make a click do nothing at all.
/// A click is an explicit request, so un-zoom regardless — but only here, so the
/// setting still means what it says for ordinary pane switching.
fn unzoom_for_focus(pane_id: PaneId) {
    let mux = Mux::get();
    let Some((_domain, _window_id, tab_id)) = mux.resolve_pane_id(pane_id) else {
        return;
    };
    let Some(tab) = mux.get_tab(tab_id) else {
        return;
    };
    let zoomed_elsewhere = tab
        .get_zoomed_pane()
        .map(|zoomed| zoomed.pane_id() != pane_id)
        .unwrap_or(false);
    if zoomed_elsewhere {
        tab.set_zoomed(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(workspace: &str) -> Option<(MuxWindowId, String)> {
        Some((7, workspace.to_string()))
    }

    #[test]
    fn dead_pane_is_never_focused() {
        assert_eq!(
            plan_focus(false, resolved("default"), "default", true),
            FocusPlan::PaneGone
        );
        // Unresolvable counts the same way.
        assert_eq!(plan_focus(true, None, "default", true), FocusPlan::PaneGone);
    }

    #[test]
    fn live_pane_with_a_window_is_raised() {
        assert_eq!(
            plan_focus(true, resolved("default"), "default", true),
            FocusPlan::Raise(7)
        );
    }

    #[test]
    fn foreign_workspace_switches_first() {
        assert_eq!(
            plan_focus(true, resolved("other"), "default", true),
            FocusPlan::SwitchWorkspace {
                workspace: "other".to_string(),
                mux_window_id: 7,
            }
        );
    }

    #[test]
    fn no_gui_window_falls_back_to_the_mux() {
        assert_eq!(
            plan_focus(true, resolved("default"), "default", false),
            FocusPlan::MuxOnly(7)
        );
    }
}
