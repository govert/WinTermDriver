//! `wtd` — WinTermDriver controller CLI.
//!
//! Short-lived process: connects to `wtd-host`, issues one command, prints the
//! response, and exits. Long-running commands (e.g. `follow`) keep the
//! connection open until interrupted.
//!
//! CLI structure is defined in bead wintermdriver-rul.1. See spec §8.3 and §22.

fn main() {
    eprintln!("wtd: not yet implemented");
}
