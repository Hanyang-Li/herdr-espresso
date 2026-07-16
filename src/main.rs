mod cli;
mod consts;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let code = match Cli::parse().command {
        Command::Toggle => {
            eprintln!("toggle: not yet implemented");
            1
        }
        Command::Watch { pane_id } => {
            eprintln!("watch {pane_id}: not yet implemented");
            1
        }
        Command::Status => {
            eprintln!("status: not yet implemented");
            1
        }
    };
    std::process::exit(code);
}
