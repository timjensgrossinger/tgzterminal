//! `wezterm cli apply-layout` and `wezterm cli balance-panes` (TGZTerminal).

use anyhow::anyhow;
use clap::Parser;
use codec::ApplyPaneLayout;
use config::keyassignment::{PaneLayout, SpawnTabDomain};
use mux::pane::PaneId;
use mux::tab::{SplitDirection, SplitRequest};
use wezterm_client::client::Client;

fn parse_layout(name: &str) -> Result<PaneLayout, String> {
    PaneLayout::from_name(name).ok_or_else(|| {
        let known: Vec<String> = PaneLayout::ALL
            .iter()
            .map(|layout| format!("{layout:?}"))
            .collect();
        format!("unknown layout `{name}`; one of {}", known.join(", "))
    })
}

#[derive(Debug, Parser, Clone)]
pub struct ApplyLayout {
    /// Specify the target pane; it takes the main slot of the layout.
    /// The default is to use the current pane based on the
    /// environment variable WEZTERM_PANE.
    #[arg(long)]
    pane_id: Option<PaneId>,

    /// Rearrange only the panes the tab has, without adding shells when
    /// the layout wants more.
    #[arg(long)]
    no_fill: bool,

    /// EvenColumns, EvenRows, MainLeft, MainTop, Grid or AgentPlusShell
    /// (also accepted as even-columns, main-left, ...).
    #[arg(value_parser = parse_layout)]
    layout: PaneLayout,
}

impl ApplyLayout {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;

        if !self.no_fill {
            let count = panes_in_tab_of(&client, pane_id).await?;
            let mut split_from = pane_id;
            for i in 0..self.layout.min_panes().saturating_sub(count) {
                // Alternate so a narrow tab keeps room for the next split;
                // the layout does the real sizing afterwards.
                let direction = if i % 2 == 0 {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                };
                let spawned = client
                    .split_pane(codec::SplitPane {
                        pane_id: split_from,
                        split_request: SplitRequest {
                            direction,
                            ..Default::default()
                        },
                        domain: SpawnTabDomain::CurrentPaneDomain,
                        command: None,
                        command_dir: None,
                        move_pane_id: None,
                    })
                    .await?;
                split_from = spawned.pane_id;
            }
        }

        client
            .apply_pane_layout(ApplyPaneLayout {
                pane_id,
                layout: Some(self.layout),
            })
            .await?;
        Ok(())
    }
}

/// How many panes share a tab with `pane_id`.
async fn panes_in_tab_of(client: &Client, pane_id: PaneId) -> anyhow::Result<usize> {
    let panes = client.list_panes().await?;
    for tabroot in panes.tabs {
        let mut cursor = tabroot.into_tree().cursor();
        let mut count = 0;
        let mut found = false;
        loop {
            if let Some(entry) = cursor.leaf_mut() {
                count += 1;
                found |= entry.pane_id == pane_id;
            }
            match cursor.preorder_next() {
                Ok(c) => cursor = c,
                Err(_) => break,
            }
        }
        if found {
            return Ok(count);
        }
    }
    Err(anyhow!("pane {pane_id} is in no tab"))
}

#[derive(Debug, Parser, Clone)]
pub struct BalancePanes {
    /// Specify a pane of the tab to balance.
    /// The default is to use the current pane based on the
    /// environment variable WEZTERM_PANE.
    #[arg(long)]
    pane_id: Option<PaneId>,
}

impl BalancePanes {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;
        client
            .apply_pane_layout(ApplyPaneLayout {
                pane_id,
                layout: None,
            })
            .await?;
        Ok(())
    }
}
