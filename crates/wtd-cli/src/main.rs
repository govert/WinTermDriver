//! `wtd` — WinTermDriver controller CLI.
//!
//! Short-lived process: connects to `wtd-host`, issues one command, prints the
//! response, and exits. Long-running commands (e.g. `follow`) keep the
//! connection open until interrupted.
//!
//! CLI structure defined per spec §22.

use clap::Parser;
use wtd_cli::cli::{Cli, Command};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if let Command::Completions { shell } = cli.command {
        wtd_cli::cli::print_completions(shell);
        return;
    }

    let code = wtd_cli::dispatch::run(cli).await;
    std::process::exit(code);
}
