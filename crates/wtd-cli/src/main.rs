//! `wtd` — WinTermDriver controller CLI.
//!
//! Short-lived process: connects to `wtd-host`, issues one command, prints the
//! response, and exits. Long-running commands (e.g. `follow`) keep the
//! connection open until interrupted.
//!
//! CLI structure defined per spec §22.

mod cli;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();

    if let Command::Completions { shell } = cli.command {
        cli::print_completions(shell);
        return;
    }

    // Command dispatch will be implemented in a future bead.
    eprintln!("wtd: command dispatch not yet implemented");
    std::process::exit(1);
}
