//! IPC message types and framing for WinTermDriver.
//!
//! The IPC transport uses Windows named pipes (`\\.\pipe\wtd-{user-SID}`).
//! Messages are framed as newline-delimited JSON (NDJSON) for debuggability.
//! See spec §13 for the full IPC architecture.

pub mod error;
pub mod message;
