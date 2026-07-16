mod cli;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let code = match Cli::parse().command {
        Command::Toggle => {
            eprintln!("toggle: not yet implemented");
            1
        }
        Command::Watch { pane_id } => herdr_espresso::watcher::watch(&pane_id),
        Command::Status => {
            eprintln!("status: not yet implemented");
            1
        }
    };
    std::process::exit(code);
}
