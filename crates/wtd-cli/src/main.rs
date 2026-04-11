//! `wtd` — WinTermDriver controller CLI.
//!
//! Short-lived process: connects to `wtd-host`, issues one command, prints the
//! response, and exits. Long-running commands (e.g. `follow`) keep the
//! connection open until interrupted.
//!
//! CLI structure defined per spec §22.

use clap::Parser;
use wtd_cli::cli::{Cli, Command};
use wtd_core::logging::init_stderr_logging;
use wtd_core::LogLevel;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // §31.1: CLI logs to stderr only. Verbose flag bumps to debug.
    let level = if cli.verbose {
        LogLevel::Debug
    } else {
        LogLevel::Info
    };
    init_stderr_logging(&level);

    if let Some(Command::Completions { shell }) = cli.command {
        wtd_cli::cli::print_completions(shell);
        return;
    }

    let code = wtd_cli::dispatch::run(cli).await;
    std::process::exit(code);
}
