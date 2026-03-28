//! ConPTY pseudo-console management for WinTermDriver.
//!
//! This crate wraps the Windows ConPTY API (`CreatePseudoConsole`,
//! `ResizePseudoConsole`, `ClosePseudoConsole`) and child process lifecycle.
//! VT output from child processes is parsed by the `vte` crate.
//!
//! Full implementation lives in the ConPTY bead (wintermdriver-mtz.1).
//! This scaffold ensures the crate compiles and establishes the module layout.

pub mod error;
pub mod pty;
