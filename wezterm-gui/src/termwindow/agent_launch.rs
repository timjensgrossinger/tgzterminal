//! Placement logic for the sidebar agent launcher: deciding *where* a newly
//! launched agent lands (a new tab, a split, or a split that gets zoomed)
//! and driving the mux split directly so repeat clicks tile into an
//! even-ish grid instead of nondeterministically halving whatever pane
//! happens to be active when each detached spawn future resolves.
//!
//! `TermWindow::spawn_command` (see `termwindow/spawn.rs`) is unsuitable for
//! this: it is fire-and-forget, returns no `PaneId`, and its split branch
//! re-resolves the target as the tab's *active* pane inside the detached
//! future (`spawn.rs`), which is exactly the race this module exists to
//! avoid.

use crate::spawn::{command_builder_for, SpawnWhere};
use anyhow::{anyhow, Context};
use config::keyassignment::SpawnCommand;
use config::{AgentLaunchTarget, AgentTilePolicy, TermConfig};
use mux::domain::SplitSource;
use mux::pane::PaneId;
use mux::tab::{SplitDirection, SplitRequest, SplitSize as MuxSplitSize};
use mux::Mux;
use std::sync::Arc;

/// Where a launched agent ends up, fully resolved: either a plain new tab,
/// or a split against a specific pane, optionally zoomed afterward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentPlacement {
    NewTab,
    Split {
        pane_id: PaneId,
        request: SplitRequest,
        /// Zoom the new agent pane once it exists, so it fills the tab.
        /// Un-zooming restores every pane that was already there.
        zoom: bool,
    },
}

/// The subset of `mux::tab::PositionedPane` that tiling geometry needs, so
/// the decision can be unit tested without a live mux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneGeom {
    pub pane_id: PaneId,
    /// Topological pane index; used only to break area ties in favor of the
    /// most recently split-in pane.
    pub index: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileTarget {
    pub pane_id: PaneId,
    pub direction: SplitDirection,
}

/// Pick which pane to split for the second (and later) agent landing in a
/// tab, and along which axis, so repeated launches tile into an even-ish
/// grid rather than each one halving whatever the previous launch made
/// active.
///
/// Splits the largest eligible pane along its longer axis. `cap` (0 =
/// unlimited) and a minimum usable half-size both fall back to `None`, which
/// callers should treat as "open a new tab instead" — a sliver pane is worse
/// than a new tab.
pub(crate) fn pick_tile_target(
    panes: &[PaneGeom],
    cap: u8,
    min_width_px: usize,
    min_height_px: usize,
) -> Option<TileTarget> {
    if cap != 0 && panes.len() >= cap as usize {
        return None;
    }
    let largest = panes
        .iter()
        .max_by_key(|p| (p.pixel_width * p.pixel_height, p.index))?;

    let can_split_horizontal = largest.pixel_width / 2 >= min_width_px;
    let can_split_vertical = largest.pixel_height / 2 >= min_height_px;
    let prefer_horizontal = largest.pixel_width >= largest.pixel_height;

    let direction = if prefer_horizontal && can_split_horizontal {
        SplitDirection::Horizontal
    } else if !prefer_horizontal && can_split_vertical {
        SplitDirection::Vertical
    } else if can_split_horizontal {
        SplitDirection::Horizontal
    } else if can_split_vertical {
        SplitDirection::Vertical
    } else {
        return None;
    };

    Some(TileTarget {
        pane_id: largest.pane_id,
        direction,
    })
}

/// Apply the Alt-click inversion (or an explicit per-launch override from
/// the launcher's submenu) to the configured `open_in` target.
///
/// `Zoomed` has no independent "other target": inverting it falls through
/// to `NewTab`, the same as inverting `SplitPane`.
pub(crate) fn resolve_launch_target(
    open_in: AgentLaunchTarget,
    invert_target: bool,
    override_target: Option<AgentLaunchTarget>,
) -> AgentLaunchTarget {
    if let Some(target) = override_target {
        return target;
    }
    if !invert_target {
        return open_in;
    }
    match open_in {
        AgentLaunchTarget::NewTab => AgentLaunchTarget::SplitPane,
        AgentLaunchTarget::SplitPane | AgentLaunchTarget::Zoomed => AgentLaunchTarget::NewTab,
    }
}

/// Decide the full placement for one agent launch: a plain new tab, or a
/// split (with the target pane and axis resolved) that is optionally
/// zoomed afterward.
///
/// `eligible_panes` excludes panes that must never be split into (the
/// worktree/file-browser pane). When there is at most one eligible pane,
/// this reproduces the pre-tiling behavior exactly: split the active pane
/// at the configured direction and size. Two or more eligible panes hand
/// off to `pick_tile_target`, which always uses a 50/50 split.
#[allow(clippy::too_many_arguments)]
pub(crate) fn agent_placement(
    target: AgentLaunchTarget,
    tile_policy: AgentTilePolicy,
    configured_direction: SplitDirection,
    configured_size_percent: u8,
    max_panes_per_tab: u8,
    active_pane_id: PaneId,
    eligible_panes: &[PaneGeom],
    min_width_px: usize,
    min_height_px: usize,
) -> AgentPlacement {
    if matches!(target, AgentLaunchTarget::NewTab) {
        return AgentPlacement::NewTab;
    }
    let zoom = matches!(target, AgentLaunchTarget::Zoomed);

    let configured_split = |pane_id: PaneId| AgentPlacement::Split {
        pane_id,
        request: SplitRequest {
            direction: configured_direction,
            target_is_second: true,
            size: MuxSplitSize::Percent(configured_size_percent.clamp(5, 95)),
            top_level: false,
        },
        zoom,
    };

    let at_cap = max_panes_per_tab != 0 && eligible_panes.len() as u8 >= max_panes_per_tab;

    if eligible_panes.len() <= 1 {
        return if at_cap {
            AgentPlacement::NewTab
        } else {
            configured_split(active_pane_id)
        };
    }

    match tile_policy {
        AgentTilePolicy::ActivePane => {
            if at_cap {
                AgentPlacement::NewTab
            } else {
                configured_split(active_pane_id)
            }
        }
        AgentTilePolicy::SplitLargest => {
            match pick_tile_target(
                eligible_panes,
                max_panes_per_tab,
                min_width_px,
                min_height_px,
            ) {
                Some(tile) => AgentPlacement::Split {
                    pane_id: tile.pane_id,
                    request: SplitRequest {
                        direction: tile.direction,
                        target_is_second: true,
                        size: MuxSplitSize::Percent(50),
                        top_level: false,
                    },
                    zoom,
                },
                None => AgentPlacement::NewTab,
            }
        }
    }
}

/// Split `pane_id`, run `spawn` in the new pane, and optionally zoom it.
///
/// Drives `Mux::split_pane` directly with an explicit target `PaneId`
/// instead of going through `spawn_command`/`SpawnWhere::SplitPane`, which
/// resolves its target as the tab's active pane inside the future — fine
/// for a single launch, but nondeterministic once several splits are
/// in flight.
async fn split_and_maybe_zoom(
    spawn: SpawnCommand,
    pane_id: PaneId,
    request: SplitRequest,
    zoom: bool,
    term_config: Arc<TermConfig>,
) -> anyhow::Result<()> {
    let mux = Mux::get();
    let _activity = mux::activity::Activity::new();

    let (command, command_dir) = command_builder_for(&spawn)?;

    let (_domain_id, _window_id, tab_id) = mux
        .resolve_pane_id(pane_id)
        .ok_or_else(|| anyhow!("pane {} is no longer part of any tab", pane_id))?;
    let tab = mux
        .get_tab(tab_id)
        .ok_or_else(|| anyhow!("tab {} disappeared before the agent could launch", tab_id))?;

    // A zoomed tab must be flattened before splitting: `split_and_insert`
    // bails outright while zoomed, and `compute_split_size` force-unzooms
    // anyway (see mux/src/tab.rs) — so do it explicitly and predictably
    // rather than relying on that side effect.
    tab.set_zoomed(false);

    let (pane, _size) = mux
        .split_pane(
            pane_id,
            request,
            SplitSource::Spawn {
                command,
                command_dir,
            },
            spawn.domain.clone(),
        )
        .await
        .context("split_pane")?;
    pane.set_config(term_config);

    if zoom {
        // `Tab::toggle_zoom` zooms whichever pane is active; it cannot be
        // told a pane directly, so the new pane must be made active first.
        tab.set_active_pane(&pane);
        tab.set_zoomed(true);
    }

    Ok(())
}

impl super::TermWindow {
    /// Execute a fully-resolved `AgentPlacement`: spawn straight into a new
    /// tab, or split a specific pane (and maybe zoom it) in one detached
    /// async task.
    pub(crate) fn spawn_agent(&self, spawn: SpawnCommand, placement: AgentPlacement) {
        match placement {
            AgentPlacement::NewTab => {
                self.spawn_command(&spawn, SpawnWhere::NewTab);
            }
            AgentPlacement::Split {
                pane_id,
                request,
                zoom,
            } => {
                let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
                promise::spawn::spawn(async move {
                    if let Err(err) =
                        split_and_maybe_zoom(spawn, pane_id, request, zoom, term_config).await
                    {
                        log::error!("Failed to launch agent: {:#}", err);
                    }
                })
                .detach();
            }
        }
    }

    /// Spawn several agents into new tabs of this window, one after another.
    ///
    /// Both halves matter. A batch must not go through `agent_launch_placement`
    /// per item — every in-flight `NewTab` spawn would resolve its target against
    /// the pre-restore layout — and awaiting each spawn is what keeps the
    /// restored tabs in the order they were captured. Goes straight to
    /// `spawn_command_internal` rather than `spawn_command`, which is
    /// fire-and-forget and would give up both properties.
    ///
    /// `skipped` is reported in the summary so a partial restore is visible
    /// rather than looking like everything worked.
    pub(crate) fn spawn_agents_in_new_tabs(&self, spawns: Vec<SpawnCommand>, skipped: usize) {
        if spawns.is_empty() {
            notify_restore_outcome(0, skipped);
            return;
        }
        let size = self.terminal_size;
        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
        let window_id = self.mux_window_id;
        promise::spawn::spawn(async move {
            let mut restored = 0usize;
            let mut failed = 0usize;
            for spawn in spawns {
                match crate::spawn::spawn_command_internal(
                    spawn,
                    SpawnWhere::NewTab,
                    size,
                    Some(window_id),
                    term_config.clone(),
                )
                .await
                {
                    Ok(()) => restored += 1,
                    Err(err) => {
                        failed += 1;
                        log::error!("agent session restore failed: {err:#}");
                    }
                }
            }
            notify_restore_outcome(restored, skipped + failed);
        })
        .detach();
    }
}

/// One toast for the whole batch — never one per session.
fn notify_restore_outcome(restored: usize, unavailable: usize) {
    let message = match (restored, unavailable) {
        (0, 0) => return,
        (0, _) => "No agent sessions could be reopened".to_string(),
        (n, 0) => format!("Reopened {n} agent session{}", plural(n)),
        (n, u) => format!("Reopened {n} agent session{}, {u} unavailable", plural(n)),
    };
    wezterm_toast_notification::show(wezterm_toast_notification::ToastNotification {
        title: "Agent restore".to_string(),
        message,
        url: None,
        timeout: Some(std::time::Duration::from_millis(2600)),
    });
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(pane_id: PaneId, index: usize, w: usize, h: usize) -> PaneGeom {
        PaneGeom {
            pane_id,
            index,
            pixel_width: w,
            pixel_height: h,
        }
    }

    #[test]
    fn tile_target_picks_the_largest_pane() {
        let panes = [geom(1, 0, 400, 400), geom(2, 1, 900, 400)];
        let target = pick_tile_target(&panes, 0, 10, 10).unwrap();
        assert_eq!(target.pane_id, 2);
    }

    #[test]
    fn tile_target_splits_the_longer_axis() {
        // Wider than tall -> horizontal; taller than wide -> vertical.
        let wide = [geom(1, 0, 900, 400)];
        assert_eq!(
            pick_tile_target(&wide, 0, 10, 10).unwrap().direction,
            SplitDirection::Horizontal
        );

        let tall = [geom(1, 0, 400, 900)];
        assert_eq!(
            pick_tile_target(&tall, 0, 10, 10).unwrap().direction,
            SplitDirection::Vertical
        );
    }

    #[test]
    fn tile_target_tie_breaks_on_the_newest_pane() {
        let panes = [geom(1, 0, 400, 400), geom(2, 1, 400, 400)];
        let target = pick_tile_target(&panes, 0, 10, 10).unwrap();
        assert_eq!(target.pane_id, 2);
    }

    #[test]
    fn tile_target_respects_the_cap() {
        let panes = [geom(1, 0, 900, 400), geom(2, 1, 900, 400)];
        assert!(pick_tile_target(&panes, 2, 10, 10).is_none());
        assert!(pick_tile_target(&panes, 3, 10, 10).is_some());
        // 0 means unlimited.
        assert!(pick_tile_target(&panes, 0, 10, 10).is_some());
    }

    #[test]
    fn tile_target_falls_back_to_new_tab_when_both_axes_are_too_small() {
        let panes = [geom(1, 0, 60, 60)];
        assert!(pick_tile_target(&panes, 0, 1000, 1000).is_none());
    }

    #[test]
    fn tile_target_tries_the_other_axis_when_the_preferred_one_is_too_small() {
        // Wide pane, but splitting horizontally would leave too little
        // width; vertical still has plenty of room.
        let panes = [geom(1, 0, 200, 900)];
        let target = pick_tile_target(&panes, 0, 1000, 10).unwrap();
        assert_eq!(target.direction, SplitDirection::Vertical);
    }

    #[test]
    fn resolve_target_uses_the_explicit_override_first() {
        assert_eq!(
            resolve_launch_target(
                AgentLaunchTarget::SplitPane,
                true,
                Some(AgentLaunchTarget::Zoomed)
            ),
            AgentLaunchTarget::Zoomed
        );
    }

    #[test]
    fn resolve_target_inverts_split_and_new_tab() {
        assert_eq!(
            resolve_launch_target(AgentLaunchTarget::SplitPane, true, None),
            AgentLaunchTarget::NewTab
        );
        assert_eq!(
            resolve_launch_target(AgentLaunchTarget::NewTab, true, None),
            AgentLaunchTarget::SplitPane
        );
    }

    #[test]
    fn resolve_target_inverting_zoomed_falls_back_to_new_tab() {
        assert_eq!(
            resolve_launch_target(AgentLaunchTarget::Zoomed, true, None),
            AgentLaunchTarget::NewTab
        );
    }

    #[test]
    fn placement_new_tab_ignores_tiling_entirely() {
        let panes = [geom(1, 0, 900, 400), geom(2, 1, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::NewTab,
            AgentTilePolicy::SplitLargest,
            SplitDirection::Horizontal,
            50,
            4,
            1,
            &panes,
            10,
            10,
        );
        assert_eq!(placement, AgentPlacement::NewTab);
    }

    #[test]
    fn placement_first_launch_uses_configured_direction_and_size() {
        let panes = [geom(1, 0, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::SplitPane,
            AgentTilePolicy::SplitLargest,
            SplitDirection::Vertical,
            30,
            4,
            1,
            &panes,
            10,
            10,
        );
        assert_eq!(
            placement,
            AgentPlacement::Split {
                pane_id: 1,
                request: SplitRequest {
                    direction: SplitDirection::Vertical,
                    target_is_second: true,
                    size: MuxSplitSize::Percent(30),
                    top_level: false,
                },
                zoom: false,
            }
        );
    }

    #[test]
    fn placement_second_launch_tiles_into_the_largest_pane() {
        let panes = [geom(1, 0, 400, 400), geom(2, 1, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::SplitPane,
            AgentTilePolicy::SplitLargest,
            SplitDirection::Vertical,
            30,
            4,
            1,
            &panes,
            10,
            10,
        );
        assert_eq!(
            placement,
            AgentPlacement::Split {
                pane_id: 2,
                request: SplitRequest {
                    direction: SplitDirection::Horizontal,
                    target_is_second: true,
                    size: MuxSplitSize::Percent(50),
                    top_level: false,
                },
                zoom: false,
            }
        );
    }

    #[test]
    fn placement_zoomed_target_carries_the_zoom_flag() {
        let panes = [geom(1, 0, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::Zoomed,
            AgentTilePolicy::SplitLargest,
            SplitDirection::Horizontal,
            50,
            4,
            1,
            &panes,
            10,
            10,
        );
        assert!(matches!(
            placement,
            AgentPlacement::Split { zoom: true, .. }
        ));
    }

    #[test]
    fn placement_active_pane_policy_ignores_tile_target() {
        let panes = [geom(1, 0, 400, 400), geom(2, 1, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::SplitPane,
            AgentTilePolicy::ActivePane,
            SplitDirection::Horizontal,
            50,
            4,
            1,
            &panes,
            10,
            10,
        );
        assert!(matches!(
            placement,
            AgentPlacement::Split { pane_id: 1, .. }
        ));
    }

    #[test]
    fn placement_falls_back_to_new_tab_at_the_cap() {
        let panes = [geom(1, 0, 900, 400), geom(2, 1, 900, 400)];
        let placement = agent_placement(
            AgentLaunchTarget::SplitPane,
            AgentTilePolicy::SplitLargest,
            SplitDirection::Horizontal,
            50,
            2,
            1,
            &panes,
            10,
            10,
        );
        assert_eq!(placement, AgentPlacement::NewTab);
    }
}
