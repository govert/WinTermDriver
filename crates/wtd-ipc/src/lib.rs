//! IPC message types and framing for WinTermDriver.
//!
//! The IPC transport uses Windows named pipes (`\\.\pipe\wtd-{user-SID}`).
//! Messages are framed with a 4-byte LE length prefix followed by a UTF-8 JSON
//! envelope. See spec §13 for the full IPC architecture.

pub mod connect;
pub mod error;
pub mod framing;
pub mod message;

pub use error::IpcError;
pub use framing::{decode, encode, MAX_MESSAGE_SIZE};
pub use message::{parse_envelope, Envelope, MessagePayload, TypedMessage};
