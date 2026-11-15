//! Keyboard focus on the sidebar's own controls (TGZTerminal).
//!
//! `FocusSidebar` (or Tab / Down from the tab search field) puts the keyboard
//! on the sidebar: Tab and Shift+Tab walk every control top to bottom, Up and
//! Down do too, Enter or Space acts on the focused one as a click would, and
//! Esc or any click hands the keyboard back. Nothing reaches the terminal
//! while the sidebar has it, and the terminal never loses Tab otherwise.
//!
//! The controls are read from what the sidebar last painted, so the order is
//! always the one on screen and a control that is not shown is not visited.
//! What is pure (which controls, in what order, where a step lands) is here
//! and tested; the ring is drawn by `render::sidebar`.

use crate::termwindow::{TermWindow, UIItem, UIItemType};
use ::window::{KeyCode, Modifiers, WindowOps};

/// Whether keyboard focus can land on `item`: the sidebar's buttons and tab
/// rows, not its scroll bars, resize handles or menu rows (menus have their
/// own arrow keys).
pub(crate) fn is_focus_target(item: &UIItemType) -> bool {
    matches!(
        item,
        UIItemType::SidebarAutoHideToggle
            | UIItemType::SidebarSearch
            | UIItemType::SidebarTab { .. }
            | UIItemType::SidebarUsageButton
            | UIItemType::SidebarSessionsButton
            | UIItemType::SidebarWorktreeButton
            | UIItemType::SidebarDiffPanelButton
            | UIItemType::SidebarToolbeltToggle
            | UIItemType::SidebarSecureInputToggle
            | UIItemType::SidebarAgentLaunchButton
            | UIItemType::SidebarSshLaunchButton
            | UIItemType::SidebarFooterAction(_)
            | UIItemType::SidebarNewTabButton
    )
}

/// Whether `a` and `b` are the same control. A tab row is its tab, whether
/// or not it is the active one: activating it must not lose the focus.
pub(crate) fn same_target(a: &UIItemType, b: &UIItemType) -> bool {
    match (a, b) {
        (UIItemType::SidebarTab { tab_idx: a, .. }, UIItemType::SidebarTab { tab_idx: b, .. }) => {
            a == b
        }
        _ => a == b,
    }
}

/// The focusable controls of `items`, each once, in reading order: top to
/// bottom, then left to right.
pub(crate) fn focus_order(items: &[UIItem]) -> Vec<UIItem> {
    let mut targets: Vec<UIItem> = vec![];
    for item in items.iter().filter(|item| is_focus_target(&item.item_type)) {
        if !targets
            .iter()
            .any(|seen| same_target(&seen.item_type, &item.item_type))
        {
            targets.push(item.clone());
        }
    }
    targets.sort_by_key(|item| (item.y, item.x));
    targets
}

/// Where a step of `delta` from `current` lands among `order`, wrapping at
/// either end. From nowhere (or a control no longer shown) a step forward
/// lands on the first tab row, else the first control; a step back on the
/// last control.
pub(crate) fn step(
    order: &[UIItem],
    current: Option<&UIItemType>,
    delta: isize,
) -> Option<UIItemType> {
    if order.is_empty() {
        return None;
    }
    let len = order.len() as isize;
    let at = current.and_then(|current| {
        order
            .iter()
            .position(|item| same_target(&item.item_type, current))
    });
    let index = match at {
        Some(at) => (at as isize + delta).rem_euclid(len),
        None if delta < 0 => len - 1,
        None => order
            .iter()
            .position(|item| matches!(item.item_type, UIItemType::SidebarTab { .. }))
            .unwrap_or(0) as isize,
    };
    Some(order[index as usize].item_type.clone())
}

impl TermWindow {
    /// Put the keyboard on the sidebar: the first tab row, else the first
    /// control.
    pub(crate) fn focus_sidebar(&mut self) {
        if self.config.sidebar_auto_hide {
            self.sidebar_auto_hide_open = true;
        }
        self.sidebar_search = None;
        let order = focus_order(&self.ui_items);
        self.sidebar_kb_focus = step(&order, None, 1);
        self.sidebar_kb_focus_rect = self.focused_item(&order);
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
    }

    /// The focused control among `order`, where it was drawn.
    fn focused_item(&self, order: &[UIItem]) -> Option<UIItem> {
        let focus = self.sidebar_kb_focus.as_ref()?;
        order
            .iter()
            .find(|item| same_target(&item.item_type, focus))
            .cloned()
    }

    /// Keys while the sidebar has the keyboard. `false` when it does not,
    /// or for a key it gives back (focus is dropped and the key goes on).
    pub(crate) fn sidebar_focus_key(&mut self, keycode: &KeyCode, modifiers: Modifiers) -> bool {
        let Some(current) = self.sidebar_kb_focus.clone() else {
            return false;
        };
        let order = focus_order(&self.ui_items);
        let delta = match keycode {
            KeyCode::Char('\t') if modifiers.contains(Modifiers::SHIFT) => Some(-1),
            KeyCode::Char('\t') | KeyCode::DownArrow => Some(1),
            KeyCode::UpArrow => Some(-1),
            _ => None,
        };
        if let Some(delta) = delta {
            self.sidebar_kb_focus = step(&order, Some(&current), delta);
            self.sidebar_kb_focus_rect = self.focused_item(&order);
        } else {
            match keycode {
                KeyCode::Char('\r') | KeyCode::Char(' ') => {
                    if let Some(item) = order
                        .into_iter()
                        .find(|item| same_target(&item.item_type, &current))
                    {
                        // Into the tab or the search field: the keyboard
                        // goes with it.
                        if matches!(
                            current,
                            UIItemType::SidebarTab { .. } | UIItemType::SidebarSearch
                        ) {
                            self.sidebar_kb_focus = None;
                        }
                        self.click_ui_item(item);
                    }
                }
                KeyCode::Char('\u{1b}') => self.sidebar_kb_focus = None,
                // Anything else is typing: give the keyboard back with it.
                _ => {
                    self.sidebar_kb_focus = None;
                    if let Some(window) = self.window.as_ref() {
                        window.invalidate();
                    }
                    return false;
                }
            }
        }
        if let Some(window) = self.window.as_ref() {
            window.invalidate();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(item_type: UIItemType, x: usize, y: usize) -> UIItem {
        UIItem {
            x,
            y,
            width: 10,
            height: 10,
            item_type,
        }
    }

    fn tab(tab_idx: usize, y: usize) -> UIItem {
        item(
            UIItemType::SidebarTab {
                tab_idx,
                active: false,
            },
            0,
            y,
        )
    }

    #[test]
    fn controls_are_visited_top_to_bottom_then_left_to_right_once_each() {
        let items = vec![
            item(UIItemType::SidebarNewTabButton, 0, 300),
            item(UIItemType::SidebarSshLaunchButton, 50, 280),
            item(UIItemType::SidebarAgentLaunchButton, 0, 280),
            tab(0, 40),
            item(UIItemType::SidebarSearch, 30, 5),
            // Painted twice (a hover pass): still one stop.
            item(UIItemType::SidebarSearch, 30, 5),
            // Not a control the keyboard visits.
            item(UIItemType::SidebarScrollThumb, 0, 100),
        ];
        let order: Vec<UIItemType> = focus_order(&items)
            .into_iter()
            .map(|item| item.item_type)
            .collect();
        assert_eq!(
            order,
            vec![
                UIItemType::SidebarSearch,
                tab(0, 40).item_type,
                UIItemType::SidebarAgentLaunchButton,
                UIItemType::SidebarSshLaunchButton,
                UIItemType::SidebarNewTabButton,
            ]
        );
    }

    #[test]
    fn steps_wrap_and_start_on_the_tab_list() {
        let order = focus_order(&[
            item(UIItemType::SidebarSearch, 0, 0),
            tab(0, 20),
            tab(1, 40),
            item(UIItemType::SidebarNewTabButton, 0, 80),
        ]);
        let first_tab = tab(0, 20).item_type;
        assert_eq!(step(&order, None, 1), Some(first_tab.clone()));
        assert_eq!(
            step(&order, None, -1),
            Some(UIItemType::SidebarNewTabButton)
        );
        assert_eq!(
            step(&order, Some(&UIItemType::SidebarNewTabButton), 1),
            Some(UIItemType::SidebarSearch)
        );
        assert_eq!(
            step(&order, Some(&UIItemType::SidebarSearch), -1),
            Some(UIItemType::SidebarNewTabButton)
        );
        assert_eq!(
            step(&order, Some(&first_tab), 1),
            Some(tab(1, 40).item_type)
        );
        assert_eq!(step(&[], None, 1), None);
    }
}
