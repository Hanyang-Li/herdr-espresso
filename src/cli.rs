use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "herdr-espresso", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Toggle monitoring for the focused pane ($HERDR_PANE_ID).
    Toggle,
    /// Internal: per-pane watcher (spawned detached by `toggle`).
    #[command(hide = true)]
    Watch { pane_id: String },
    /// List currently monitored panes.
    Status,
}
