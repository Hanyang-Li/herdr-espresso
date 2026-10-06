mod cli;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let code = match Cli::parse().command {
        Command::Toggle => herdr_espresso::toggle::toggle(),
        Command::Watch { .. } => herdr_espresso::watcher::watch(),
        Command::Status => herdr_espresso::status::status(),
    };
    std::process::exit(code);
}
