use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "herdr-espresso", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Turn the global keep-awake monitor on or off.
    Toggle,
    /// Restore the monitor after herdr starts (manifest `[[startup]]` hook).
    Startup,
    #[command(hide = true)]
    Watch,
}
