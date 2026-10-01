use crate::spawn::SpawnWhere;
use config::keyassignment::{SpawnCommand, SpawnTabDomain};
use config::TermConfig;
use mux::pane::SpawnTitlePolicy;
use std::sync::Arc;

impl super::TermWindow {
    pub fn spawn_command(&self, spawn: &SpawnCommand, spawn_where: SpawnWhere) {
        self.spawn_command_with_title(spawn, spawn_where, SpawnTitlePolicy::ShimFallback)
    }

    /// [`Self::spawn_command`], recording the spawn's label under
    /// `title_policy` (see [`SpawnTitlePolicy`]).
    pub fn spawn_command_with_title(
        &self,
        spawn: &SpawnCommand,
        spawn_where: SpawnWhere,
        title_policy: SpawnTitlePolicy,
    ) {
        let size = if spawn_where == SpawnWhere::NewWindow {
            self.config.initial_size(
                self.dimensions.dpi as u32,
                crate::cell_pixel_dims(&self.config, self.dimensions.dpi as f64).ok(),
            )
        } else {
            self.terminal_size
        };
        let term_config = Arc::new(TermConfig::with_config(self.config.clone()));

        crate::spawn::spawn_command_impl_with_title(
            spawn,
            spawn_where,
            size,
            Some(self.mux_window_id),
            term_config,
            title_policy,
        )
    }

    pub fn spawn_tab(&mut self, domain: &SpawnTabDomain) {
        self.spawn_command(
            &SpawnCommand {
                domain: domain.clone(),
                ..Default::default()
            },
            SpawnWhere::NewTab,
        );
    }
}
