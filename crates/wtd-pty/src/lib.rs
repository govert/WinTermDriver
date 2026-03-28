//! ConPTY pseudo-console management for WinTermDriver.
//!
//! This crate wraps the Windows ConPTY API (`CreatePseudoConsole`,
//! `ResizePseudoConsole`, `ClosePseudoConsole`) and child process lifecycle.
//! Process tree management uses Windows Job Objects (§14.6).

mod handle;

pub mod error;
pub mod job;
pub mod pty;

pub use error::PtyError;
pub use job::JobObject;
pub use pty::{PtySession, PtySize};
