//! `wtd-host` — WinTermDriver host process.
//!
//! Per-user singleton background process. Owns all ConPTY sessions, workspace
//! instance state, and the named pipe IPC server.
//!
//! See spec §8.1 and §16 for the full host lifecycle.

fn main() {
    eprintln!("wtd-host: not yet implemented");
}
