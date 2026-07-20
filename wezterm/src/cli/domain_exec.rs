use anyhow::Context;
use clap::Parser;
use std::io::{Read, Write};
use wezterm_client::client::Client;

/// Run a command over a wezterm SSH domain's already-authenticated session and
/// print its captured output. The command is read from stdin so that arbitrary
/// shell programs (with quotes, newlines, etc.) can be passed safely without
/// argv escaping. Exits with the remote command's coarse exit code.
#[derive(Debug, Parser, Clone)]
pub struct DomainExec {
    /// The id of the ssh domain whose live connection should run the command.
    #[arg(long)]
    domain_id: usize,
}

impl DomainExec {
    pub async fn run(self, client: Client) -> anyhow::Result<()> {
        let mut command = String::new();
        std::io::stdin()
            .read_to_string(&mut command)
            .context("reading command from stdin")?;

        let resp = client
            .domain_exec(codec::DomainExec {
                domain_id: self.domain_id,
                command,
                env: None,
            })
            .await?;

        std::io::stdout().write_all(&resp.stdout).ok();
        std::io::stderr().write_all(&resp.stderr).ok();

        if resp.exit_code != 0 {
            std::process::exit(resp.exit_code);
        }
        Ok(())
    }
}
