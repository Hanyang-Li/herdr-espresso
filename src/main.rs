mod cli;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let code = match Cli::parse().command {
        Command::Toggle => herdr_espresso::toggle::toggle(),
        Command::Startup => herdr_espresso::startup::startup(),
        Command::Watch => herdr_espresso::watcher::watch(),
    };
    std::process::exit(code);
}
