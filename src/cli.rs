use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "herdr-espresso", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Cycle global keep-awake: off → auto → forced → off.
    Toggle,
    /// Restore the monitor after herdr starts (manifest `[[startup]]` hook).
    Startup,
    #[command(hide = true)]
    Watch {
        /// Keep awake regardless of agent status.
        #[arg(long)]
        forced: bool,
    },
}
