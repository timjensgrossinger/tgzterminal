//! The agent insight pane: the herd overview living in a real split pane
//! rather than an overlay.
//!
//! An overlay *replaces* the pane it covers and takes all of its input, so the
//! overview could never sit beside the agents it describes. This module puts it
//! in the tab's split tree instead, next to the worktree pane and the agents
//! themselves, where it survives focus changes and closes only when asked.
//!
//! The pane has no child process. `mux::termwiztermtab::allocate` hands back a
//! `TermWizTerminal` and a matching `Pane` — the same pair the overlays use —
//! and [`mux::tab::Tab::split_and_insert`] accepts that pane directly, which is
//! what `Mux::split_pane` cannot do (its `SplitSource` only knows how to spawn a
//! command or move an existing pane).
//!
//! Threading follows the overlay rules unchanged: the view runs on its own
//! thread and reaches the mux only inside a `TermWindowNotif::Apply` closure.

use crate::termwindow::TermWindowNotif;
use anyhow::anyhow;
use config::{AgentInsightSide, AgentInsightView, TermConfig};
use mux::pane::{Pane, PaneId};
use mux::tab::{SplitDirection, SplitRequest, SplitSize as MuxSplitSize};
use mux::termwiztermtab::allocate;
use mux::Mux;
use std::sync::Arc;
use std::time::Duration;
use wezterm_term::TerminalSize;
use window::WindowOps;

/// Pane title, also what the sidebar shows for the row.
pub(crate) const INSIGHT_PANE_TITLE: &str = "Agent Insight";

impl super::TermWindow {
    /// Open the insight pane, or close it if it is already open.
    pub(crate) fn toggle_agent_insight_pane(&mut self, pane: &Arc<dyn Pane>) {
        if let Some(pane_id) = self.find_agent_insight_pane() {
            self.close_agent_insight_pane(pane_id);
            return;
        }
        if let Err(err) = self.open_agent_insight_pane(pane) {
            log::error!("failed to open the agent insight pane: {err:#}");
        }
    }

    /// The insight pane in the active tab, if there is one.
    pub(crate) fn find_agent_insight_pane(&self) -> Option<PaneId> {
        let tab = Mux::get().get_active_tab_for_window(self.mux_window_id)?;
        tab.iter_panes_ignoring_zoom()
            .iter()
            .map(|pos| pos.pane.pane_id())
            .find(|pane_id| self.agent_insight_panes.borrow().contains(pane_id))
    }

    /// Is this pane the insight view?
    ///
    /// Identity is the tracked pane id, not the title: the title is cosmetic
    /// and any shell could print it, and this answer gates agent detection.
    pub(crate) fn is_agent_insight_pane(&self, pane: &Arc<dyn Pane>) -> bool {
        self.agent_insight_panes.borrow().contains(&pane.pane_id())
    }

    pub(crate) fn close_agent_insight_pane(&mut self, pane_id: PaneId) {
        Mux::get().remove_pane(pane_id);
        self.agent_insight_panes.borrow_mut().remove(&pane_id);
    }

    fn open_agent_insight_pane(&mut self, pane: &Arc<dyn Pane>) -> anyhow::Result<()> {
        let window = self
            .window
            .clone()
            .ok_or_else(|| anyhow!("window is gone"))?;
        let insight = &self.config.agent_ui.insight;

        let mux = Mux::get();
        let (_domain_id, _window_id, tab_id) = mux
            .resolve_pane_id(pane.pane_id())
            .ok_or_else(|| anyhow!("pane {} is not part of any tab", pane.pane_id()))?;
        let tab = mux
            .get_tab(tab_id)
            .ok_or_else(|| anyhow!("tab {tab_id} disappeared"))?;

        // `split_and_insert` bails outright while a tab is zoomed.
        tab.set_zoomed(false);

        let target = self.agent_insight_split_target(pane);
        let index = tab
            .iter_panes_ignoring_zoom()
            .iter()
            .find(|pos| pos.pane.pane_id() == target)
            .map(|pos| pos.index)
            .ok_or_else(|| anyhow!("pane {target} vanished before the split"))?;

        let (direction, target_is_second) = match insight.side {
            AgentInsightSide::Left => (SplitDirection::Horizontal, false),
            AgentInsightSide::Right => (SplitDirection::Horizontal, true),
            AgentInsightSide::Top => (SplitDirection::Vertical, false),
            AgentInsightSide::Bottom => (SplitDirection::Vertical, true),
        };
        let request = SplitRequest {
            direction,
            target_is_second,
            size: MuxSplitSize::Percent(insight.split_size_percent.clamp(5, 95)),
            top_level: false,
        };

        // `split_and_insert` resizes the inserted pane itself; this is only the
        // size the view is first laid out at.
        let size = tab
            .compute_split_size(index, request)
            .map(|split| split.second)
            .unwrap_or_else(|| {
                let dims = pane.get_dimensions();
                TerminalSize {
                    cols: dims.cols,
                    rows: dims.viewport_rows,
                    pixel_width: self.render_metrics.cell_size.width as usize * dims.cols,
                    pixel_height: self.render_metrics.cell_size.height as usize
                        * dims.viewport_rows,
                    dpi: dims.dpi,
                }
            });

        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));
        let (tw_term, insight_pane) = allocate(size, term_config.clone());
        let insight_pane_id = insight_pane.pane_id();

        // Registered before the split so a failed insert cannot leave a pane
        // that detection would then treat as an agent.
        self.agent_insight_panes
            .borrow_mut()
            .insert(insight_pane_id);
        if let Err(err) = tab.split_and_insert(index, request, Arc::clone(&insight_pane)) {
            self.agent_insight_panes
                .borrow_mut()
                .remove(&insight_pane_id);
            mux.remove_pane(insight_pane_id);
            return Err(err).map_err(|err| anyhow!("split_and_insert: {err:#}"));
        }
        insight_pane.set_config(term_config);

        let cwd = crate::termwindow::render::sidebar::pane_working_dir(pane);
        let current_project = cwd.as_deref().and_then(crate::agent_herd::project_root_for);
        let args = crate::overlay::agent_herd::HerdArgs {
            theme: self.agent_herd_theme(),
            view: match insight.default_view {
                AgentInsightView::CurrentProject => crate::agent_herd::HerdView::CurrentProject,
                AgentInsightView::AllProjects => crate::agent_herd::HerdView::AllGrouped,
            },
            // With no repo root we would filter against nothing and show an
            // empty list, so fall back to the plain cwd.
            current_project: current_project.or(cwd),
            refresh: Duration::from_millis(insight.refresh_ms.clamp(100, 10_000)),
            include_subagents: true,
            read_claude_sessions: true,
            initial_panes: self.agent_herd_pane_rows(),
            show_activity: insight.show_activity,
            activity_history: insight.activity_history as usize,
        };

        let view_window = window.clone();
        let future = promise::spawn::spawn_into_new_thread(move || {
            let result =
                crate::overlay::agent_herd::agent_herd_overview(view_window, tw_term, args);
            // The view exiting — `q`, or its pane being torn out from under it
            // — is what closes the pane. Going through the GUI thread keeps the
            // "never touch the mux from here" rule intact.
            window.notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                term_window.close_agent_insight_pane(insight_pane_id);
            })));
            result
        });
        promise::spawn::spawn(async move {
            if let Err(err) = future.await {
                log::error!("agent insight pane exited with an error: {err:#}");
            }
        })
        .detach();

        Ok(())
    }

    /// Which pane the insight view splits off.
    ///
    /// Never the worktree pane: it is already a narrow utility column, and
    /// halving it leaves two unusable slivers.
    fn agent_insight_split_target(&self, requested: &Arc<dyn Pane>) -> PaneId {
        if !self.is_worktree_pane_for_file_browser(requested) {
            return requested.pane_id();
        }
        Mux::get()
            .get_active_tab_for_window(self.mux_window_id)
            .and_then(|tab| {
                tab.iter_panes_ignoring_zoom()
                    .iter()
                    .find(|pos| !self.is_worktree_pane_for_file_browser(&pos.pane))
                    .map(|pos| pos.pane.pane_id())
            })
            .unwrap_or_else(|| requested.pane_id())
    }
}
