//! Which group each sidebar tab belongs to.
//!
//! A group is a project folder (found automatically), a group the user made,
//! or "Other". Rules the rest of the sidebar relies on:
//!
//! - A tab's group is set **once**: `cd` into another project does not move
//!   it. The exception is a tab that had no project yet when it was first
//!   seen (a shell that has not reported its folder); it is placed for real
//!   the first time a project shows up.
//! - A tab the user moved stays where it was put.
//! - Groups are contiguous in the order the sidebar shows, and the mux tab
//!   order is made to match (`reorder_moves`), so "tab 3" means the third row.
//!
//! Pure: no GUI, mux or terminal types beyond `TabId`.

use mux::tab::TabId;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum GroupId {
    /// Tabs working in one project folder.
    Project(PathBuf),
    /// A group the user made, by name.
    Custom(String),
    /// Tabs with no project and no group of their own.
    Other,
}

impl GroupId {
    /// What the header shows.
    pub(crate) fn name(&self) -> String {
        match self {
            Self::Project(root) => root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned()),
            Self::Custom(name) => name.clone(),
            Self::Other => "Other".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Group {
    pub id: GroupId,
    pub collapsed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct TabGroups {
    groups: Vec<Group>,
    member: HashMap<TabId, GroupId>,
    /// Tabs parked in `Other` only because no project was known yet.
    provisional: HashSet<TabId>,
}

impl TabGroups {
    pub(crate) fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub(crate) fn group_of(&self, tab: TabId) -> Option<&GroupId> {
        self.member.get(&tab)
    }

    /// Whether `assign` would still look at this tab's project folder.
    pub(crate) fn needs_root(&self, tab: TabId) -> bool {
        !self.member.contains_key(&tab) || self.provisional.contains(&tab)
    }

    fn ensure_group(&mut self, id: &GroupId) {
        if self.groups.iter().any(|group| &group.id == id) {
            return;
        }
        let group = Group {
            id: id.clone(),
            collapsed: false,
        };
        // "Other" stays last; everything else lines up in order of arrival.
        match self.groups.iter().position(|g| g.id == GroupId::Other) {
            Some(other) if *id != GroupId::Other => self.groups.insert(other, group),
            _ => self.groups.push(group),
        }
    }

    /// Place a tab that has no settled group. `auto` is the auto-group
    /// switch: off, a tab only ever lands in `Other` until the user moves it.
    pub(crate) fn assign(&mut self, tab: TabId, project_root: Option<&Path>, auto: bool) {
        if !self.needs_root(tab) {
            return;
        }
        match project_root.filter(|_| auto) {
            Some(root) => {
                let id = GroupId::Project(root.to_path_buf());
                self.ensure_group(&id);
                self.member.insert(tab, id);
                self.provisional.remove(&tab);
            }
            None => {
                if !self.member.contains_key(&tab) {
                    self.ensure_group(&GroupId::Other);
                    self.member.insert(tab, GroupId::Other);
                    self.provisional.insert(tab);
                }
            }
        }
    }

    /// The user put `tab` into `group`; it stays there.
    pub(crate) fn move_tab(&mut self, tab: TabId, group: &GroupId) {
        self.ensure_group(group);
        self.member.insert(tab, group.clone());
        self.provisional.remove(&tab);
    }

    /// Adds an empty group the user named. False when the name is empty or
    /// taken.
    pub(crate) fn create_custom(&mut self, name: &str) -> bool {
        let name = name.trim();
        let id = GroupId::Custom(name.to_string());
        if name.is_empty() || self.groups.iter().any(|group| group.id == id) {
            return false;
        }
        self.ensure_group(&id);
        true
    }

    pub(crate) fn rename_custom(&mut self, old: &str, new: &str) -> bool {
        let new = new.trim();
        let old_id = GroupId::Custom(old.to_string());
        let new_id = GroupId::Custom(new.to_string());
        if new.is_empty() || old_id == new_id || self.groups.iter().any(|g| g.id == new_id) {
            return false;
        }
        let Some(group) = self.groups.iter_mut().find(|group| group.id == old_id) else {
            return false;
        };
        group.id = new_id.clone();
        for id in self.member.values_mut().filter(|id| **id == old_id) {
            *id = new_id.clone();
        }
        true
    }

    /// Removes a group the user made. Its tabs are placed again as if new.
    pub(crate) fn delete_custom(&mut self, name: &str) {
        let id = GroupId::Custom(name.to_string());
        self.groups.retain(|group| group.id != id);
        self.member.retain(|_, group| *group != id);
    }

    /// Names of the groups the user made, in order.
    pub(crate) fn custom_names(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter_map(|group| match &group.id {
                GroupId::Custom(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Tabs of one group, in `mux_order`.
    pub(crate) fn tabs_of(&self, id: &GroupId, mux_order: &[TabId]) -> Vec<TabId> {
        mux_order
            .iter()
            .copied()
            .filter(|tab| self.member.get(tab) == Some(id))
            .collect()
    }

    pub(crate) fn toggle_collapsed(&mut self, id: &GroupId) {
        if let Some(group) = self.groups.iter_mut().find(|group| &group.id == id) {
            group.collapsed = !group.collapsed;
        }
    }

    /// Forget where tabs were placed automatically, so the next `assign`
    /// sorts them by the folder they are in now. Tabs in the user's own
    /// groups stay.
    pub(crate) fn regroup_by_folder(&mut self) {
        self.member
            .retain(|_, group| matches!(group, GroupId::Custom(_)));
        self.provisional.clear();
    }

    /// Drop tabs that no longer exist and the automatic groups they emptied.
    /// A group the user made stays, empty or not.
    pub(crate) fn prune(&mut self, live: &[TabId]) {
        let live: HashSet<TabId> = live.iter().copied().collect();
        self.member.retain(|tab, _| live.contains(tab));
        self.provisional.retain(|tab| live.contains(tab));
        let used: HashSet<&GroupId> = self.member.values().collect();
        self.groups
            .retain(|group| matches!(group.id, GroupId::Custom(_)) || used.contains(&group.id));
    }

    /// The groups to show, each with its tabs in `mux_order`. Tabs without a
    /// group yet are left out until `assign` has seen them.
    pub(crate) fn layout(&self, mux_order: &[TabId]) -> Vec<(usize, Vec<TabId>)> {
        self.groups
            .iter()
            .enumerate()
            .map(|(idx, group)| {
                let tabs = mux_order
                    .iter()
                    .copied()
                    .filter(|tab| self.member.get(tab) == Some(&group.id))
                    .collect();
                (idx, tabs)
            })
            .collect()
    }

    /// The tab order in which every group is one contiguous run.
    pub(crate) fn desired_order(&self, mux_order: &[TabId]) -> Vec<TabId> {
        let mut order: Vec<TabId> = self
            .layout(mux_order)
            .into_iter()
            .flat_map(|(_, tabs)| tabs)
            .collect();
        order.extend(
            mux_order
                .iter()
                .copied()
                .filter(|tab| !self.member.contains_key(tab)),
        );
        order
    }
}

/// The `(from, to)` index moves that turn `current` into `desired`, applied
/// in order as "remove at `from`, insert at `to`". Both must hold the same
/// tabs; anything else yields no moves.
pub(crate) fn reorder_moves(current: &[TabId], desired: &[TabId]) -> Vec<(usize, usize)> {
    if current.len() != desired.len() {
        return vec![];
    }
    let mut work = current.to_vec();
    let mut moves = vec![];
    for (to, tab) in desired.iter().enumerate() {
        let Some(from) = work.iter().position(|candidate| candidate == tab) else {
            return vec![];
        };
        if from != to {
            let moved = work.remove(from);
            work.insert(to, moved);
            moves.push((from, to));
        }
    }
    moves
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> GroupId {
        GroupId::Project(PathBuf::from(format!("/src/{name}")))
    }

    fn root(name: &str) -> PathBuf {
        PathBuf::from(format!("/src/{name}"))
    }

    #[test]
    fn a_tab_keeps_its_first_project() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), true);
        groups.assign(1, Some(&root("b")), true);
        assert_eq!(groups.group_of(1), Some(&project("a")));
        assert_eq!(groups.groups().len(), 1);
    }

    #[test]
    fn a_tab_without_a_project_waits_in_other_until_one_shows_up() {
        let mut groups = TabGroups::default();
        groups.assign(1, None, true);
        assert_eq!(groups.group_of(1), Some(&GroupId::Other));
        assert!(groups.needs_root(1));
        groups.assign(1, Some(&root("a")), true);
        assert_eq!(groups.group_of(1), Some(&project("a")));
        assert!(!groups.needs_root(1));
        // And from then on it is settled.
        groups.assign(1, Some(&root("b")), true);
        assert_eq!(groups.group_of(1), Some(&project("a")));
    }

    #[test]
    fn with_auto_off_tabs_only_move_by_hand() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), false);
        assert_eq!(groups.group_of(1), Some(&GroupId::Other));
        assert!(groups.create_custom("Infra"));
        groups.move_tab(1, &GroupId::Custom("Infra".into()));
        groups.assign(1, Some(&root("a")), true);
        assert_eq!(groups.group_of(1), Some(&GroupId::Custom("Infra".into())));
    }

    #[test]
    fn other_stays_last_and_groups_keep_arrival_order() {
        let mut groups = TabGroups::default();
        groups.assign(1, None, true);
        groups.assign(2, Some(&root("b")), true);
        groups.assign(3, Some(&root("a")), true);
        let ids: Vec<GroupId> = groups.groups().iter().map(|g| g.id.clone()).collect();
        assert_eq!(ids, vec![project("b"), project("a"), GroupId::Other]);
    }

    #[test]
    fn desired_order_makes_groups_contiguous_and_keeps_order_inside() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), true);
        groups.assign(2, Some(&root("b")), true);
        groups.assign(3, Some(&root("a")), true);
        groups.assign(4, None, true);
        // Tab 9 has not been seen by `assign` yet: it goes last.
        assert_eq!(groups.desired_order(&[4, 3, 2, 9, 1]), vec![3, 1, 2, 4, 9]);
    }

    #[test]
    fn reorder_moves_reach_the_desired_order() {
        let current = [4, 3, 2, 1];
        let desired = [3, 1, 2, 4];
        let mut work = current.to_vec();
        for (from, to) in reorder_moves(&current, &desired) {
            let moved = work.remove(from);
            work.insert(to, moved);
        }
        assert_eq!(work, desired);
        assert!(reorder_moves(&desired, &desired).is_empty());
        assert!(reorder_moves(&[1, 2], &[1]).is_empty());
        assert!(reorder_moves(&[1, 2], &[1, 3]).is_empty());
    }

    #[test]
    fn pruning_drops_empty_projects_but_keeps_the_users_groups() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), true);
        assert!(groups.create_custom("Infra"));
        groups.prune(&[]);
        let ids: Vec<GroupId> = groups.groups().iter().map(|g| g.id.clone()).collect();
        assert_eq!(ids, vec![GroupId::Custom("Infra".into())]);
    }

    #[test]
    fn custom_groups_rename_and_delete() {
        let mut groups = TabGroups::default();
        assert!(groups.create_custom("Infra"));
        assert!(!groups.create_custom("Infra"));
        assert!(!groups.create_custom("  "));
        groups.move_tab(1, &GroupId::Custom("Infra".into()));
        assert!(groups.rename_custom("Infra", "Ops"));
        assert_eq!(groups.group_of(1), Some(&GroupId::Custom("Ops".into())));
        assert!(!groups.rename_custom("Ops", "Ops"));
        groups.delete_custom("Ops");
        assert_eq!(groups.group_of(1), None);
        assert!(groups.needs_root(1));
    }

    #[test]
    fn regrouping_resorts_automatic_tabs_only() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), true);
        groups.move_tab(2, &GroupId::Custom("Infra".into()));
        groups.regroup_by_folder();
        groups.assign(1, Some(&root("b")), true);
        groups.assign(2, Some(&root("b")), true);
        assert_eq!(groups.group_of(1), Some(&project("b")));
        assert_eq!(groups.group_of(2), Some(&GroupId::Custom("Infra".into())));
    }

    #[test]
    fn collapsing_is_per_group() {
        let mut groups = TabGroups::default();
        groups.assign(1, Some(&root("a")), true);
        groups.toggle_collapsed(&project("a"));
        assert!(groups.groups()[0].collapsed);
        assert_eq!(GroupId::Other.name(), "Other");
        assert_eq!(project("a").name(), "a");
    }
}
