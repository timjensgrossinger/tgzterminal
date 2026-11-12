//! Hover tooltips for the fork's icon controls.
//!
//! An icon says nothing to somebody who has not used it yet. Resting the
//! pointer on one for [`CHROME_TOOLTIP_DELAY`] names it, adds what a right
//! click does where that is not the same as a left click, and shows the
//! shortcut when the action has one bound.
//!
//! This module holds what is pure: which controls have a tooltip and what it
//! says. Painting lives with the other floating surfaces in
//! `render/sidebar.rs`.

use crate::termwindow::{DiffPanelAction, FileTreeAction, UIItemType};
use config::keyassignment::{KeyAssignment, SpawnTabDomain};
use std::time::{Duration, Instant};

/// How long the pointer rests on a control before its tooltip shows.
pub(crate) const CHROME_TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// The control the pointer is resting on, and since when.
#[derive(Clone, Debug)]
pub(crate) struct ChromeTooltipHover {
    pub item: UIItemType,
    pub since: Instant,
    /// Set by a press or an open menu: the tooltip then stays away until the
    /// pointer moves to another control, instead of popping back up over
    /// whatever the click just opened.
    pub dismissed: bool,
}

/// What a tooltip says: the control's name, and the action whose shortcut is
/// shown beside it when one is bound.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChromeTooltip {
    pub label: &'static str,
    pub action: Option<KeyAssignment>,
}

/// The tooltip of `item`, or `None` for anything that already says what it
/// is: rows, labelled buttons, menu entries.
pub(crate) fn chrome_tooltip(item: &UIItemType) -> Option<ChromeTooltip> {
    let plain = |label| {
        Some(ChromeTooltip {
            label,
            action: None,
        })
    };
    let keyed = |label, action| {
        Some(ChromeTooltip {
            label,
            action: Some(action),
        })
    };
    match item {
        UIItemType::SidebarAutoHideToggle => {
            keyed("Auto-hide sidebar", KeyAssignment::ToggleSidebarAutoHide)
        }
        UIItemType::SidebarWorktreeButton => {
            keyed("Worktree (files)", KeyAssignment::OpenFileBrowser)
        }
        UIItemType::SidebarDiffPanelButton => {
            keyed("Changes panel", KeyAssignment::ToggleDiffPanel)
        }
        UIItemType::SidebarToolbeltToggle => {
            keyed("Agent toolbelt", KeyAssignment::TogglePaneToolbelt)
        }
        UIItemType::SidebarSecureInputToggle => keyed(
            "Secure Keyboard Entry",
            KeyAssignment::ToggleSecureKeyboardEntry,
        ),
        UIItemType::SidebarAgentLaunchButton => keyed(
            "Start agent \u{00b7} right-click: more",
            KeyAssignment::LaunchDefaultAgent,
        ),
        UIItemType::SidebarNewTabButton => keyed(
            "New tab \u{00b7} right-click: shells",
            KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain),
        ),
        UIItemType::SidebarSshLaunchButton => plain("SSH connect \u{00b7} right-click: hosts"),
        UIItemType::SidebarUsageButton => keyed("Usage and limits", KeyAssignment::ShowUsageMenu),
        // Named after what they are in the `+` menu; a button of the user's
        // or one for a named agent carries its own label.
        UIItemType::SidebarFooterAction(item) => {
            crate::termwindow::tgz_footer::library_name(*item).and_then(plain)
        }
        UIItemType::SidebarSessionsButton => {
            keyed("Recent sessions", KeyAssignment::ShowSessionsMenu)
        }
        UIItemType::SidebarWaitingCounter => {
            keyed("Jump to waiting agent", KeyAssignment::CycleWaitingAgent)
        }
        UIItemType::SidebarCloseTab(_) => plain("Close tab \u{00b7} right-click: more"),
        UIItemType::SidebarTabCopy { .. } => plain("Copy \u{00b7} right-click: more"),
        UIItemType::SidebarPaneClose { .. } => plain("Close pane"),
        UIItemType::SidebarTabExpand { .. } => plain("Show or hide panes"),
        UIItemType::DiffPanelButton(action) => match action {
            DiffPanelAction::Close => plain("Close Changes"),
            DiffPanelAction::Refresh => plain("Refresh"),
            // Text buttons: they name themselves.
            DiffPanelAction::ResetBaseline
            | DiffPanelAction::ToggleChips
            | DiffPanelAction::ToggleSessionFilter => None,
        },
        UIItemType::FileTreeButton(action) => plain(match action {
            FileTreeAction::Close => "Close Worktree",
            FileTreeAction::Refresh => "Refresh",
            FileTreeAction::CollapseAll => "Collapse all folders",
            FileTreeAction::Filter => "Filter files",
            FileTreeAction::Preview => "Quick preview",
            FileTreeAction::ShowChanges => "Show in Changes",
        }),
        _ => None,
    }
}

/// What to do about the tooltip this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TooltipPhase {
    /// Nothing to show and nothing to wait for.
    Hidden,
    /// Not yet: paint again at this instant.
    Waiting(Instant),
    Shown,
}

/// Advance the hover state for the control under the pointer and say whether
/// its tooltip is due. `blocked` is a held mouse button or an open menu.
pub(crate) fn advance_hover(
    hover: Option<ChromeTooltipHover>,
    under_pointer: Option<&UIItemType>,
    blocked: bool,
    now: Instant,
) -> (Option<ChromeTooltipHover>, TooltipPhase) {
    let Some(item) = under_pointer.filter(|item| chrome_tooltip(item).is_some()) else {
        return (None, TooltipPhase::Hidden);
    };
    let mut hover = match hover {
        Some(hover) if hover.item == *item => hover,
        _ => ChromeTooltipHover {
            item: item.clone(),
            since: now,
            dismissed: false,
        },
    };
    hover.dismissed |= blocked;
    let phase = if hover.dismissed {
        TooltipPhase::Hidden
    } else {
        let due = hover.since + CHROME_TOOLTIP_DELAY;
        if now < due {
            TooltipPhase::Waiting(due)
        } else {
            TooltipPhase::Shown
        }
    };
    (Some(hover), phase)
}

/// Top-left corner of a tooltip of `size` for a control at `item`
/// (`x, y, width, height`): below it, or above when there is no room below,
/// and never off the window.
pub(crate) fn tooltip_origin(
    item: (f32, f32, f32, f32),
    size: (f32, f32),
    window: (f32, f32),
    gap: f32,
) -> (f32, f32) {
    let (item_x, item_y, _, item_h) = item;
    let (w, h) = size;
    let below = item_y + item_h + gap;
    let y = if below + h + gap <= window.1 {
        below
    } else {
        (item_y - h - gap).max(gap)
    };
    let x = item_x.clamp(gap, (window.0 - w - gap).max(gap));
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_controls_have_a_tooltip_and_rows_do_not() {
        assert_eq!(
            chrome_tooltip(&UIItemType::SidebarToolbeltToggle).map(|t| t.label),
            Some("Agent toolbelt")
        );
        assert!(chrome_tooltip(&UIItemType::SidebarNewTabButton)
            .unwrap()
            .action
            .is_some());
        assert!(chrome_tooltip(&UIItemType::SidebarTab {
            tab_idx: 0,
            active: true
        })
        .is_none());
        assert!(chrome_tooltip(&UIItemType::SidebarDropdownBody).is_none());
        // A button that is already text needs no caption.
        assert!(
            chrome_tooltip(&UIItemType::DiffPanelButton(DiffPanelAction::ResetBaseline)).is_none()
        );
    }

    #[test]
    fn tooltip_waits_then_shows_and_restarts_on_another_control() {
        let t0 = Instant::now();
        let lock = UIItemType::SidebarSecureInputToggle;
        let (hover, phase) = advance_hover(None, Some(&lock), false, t0);
        assert_eq!(phase, TooltipPhase::Waiting(t0 + CHROME_TOOLTIP_DELAY));

        let later = t0 + CHROME_TOOLTIP_DELAY;
        let (hover, phase) = advance_hover(hover, Some(&lock), false, later);
        assert_eq!(phase, TooltipPhase::Shown);

        // Another control starts its own wait.
        let wrench = UIItemType::SidebarToolbeltToggle;
        let (hover, phase) = advance_hover(hover, Some(&wrench), false, later);
        assert_eq!(phase, TooltipPhase::Waiting(later + CHROME_TOOLTIP_DELAY));

        // Off every control: nothing kept.
        let (hover, phase) = advance_hover(hover, None, false, later);
        assert!(hover.is_none());
        assert_eq!(phase, TooltipPhase::Hidden);
    }

    #[test]
    fn a_press_keeps_the_tooltip_away_until_the_pointer_moves_on() {
        let t0 = Instant::now();
        let lock = UIItemType::SidebarSecureInputToggle;
        let (hover, _) = advance_hover(None, Some(&lock), true, t0);
        let much_later = t0 + CHROME_TOOLTIP_DELAY * 10;
        let (hover, phase) = advance_hover(hover, Some(&lock), false, much_later);
        assert_eq!(phase, TooltipPhase::Hidden);

        let wrench = UIItemType::SidebarToolbeltToggle;
        let (_, phase) = advance_hover(hover, Some(&wrench), false, much_later);
        assert!(matches!(phase, TooltipPhase::Waiting(_)));
    }

    #[test]
    fn tooltip_stays_inside_the_window() {
        // Room below.
        assert_eq!(
            tooltip_origin((10., 10., 20., 20.), (100., 30.), (400., 300.), 4.),
            (10., 34.)
        );
        // A footer button: flips above.
        assert_eq!(
            tooltip_origin((10., 270., 20., 20.), (100., 30.), (400., 300.), 4.),
            (10., 236.)
        );
        // Near the right edge: pulled back in.
        let (x, _) = tooltip_origin((380., 10., 20., 20.), (100., 30.), (400., 300.), 4.);
        assert_eq!(x, 296.);
        // Wider than the window: pinned to the gap rather than negative.
        let (x, _) = tooltip_origin((0., 10., 20., 20.), (500., 30.), (400., 300.), 4.);
        assert_eq!(x, 4.);
    }
}
