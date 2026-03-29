//! IPC message types and framing for WinTermDriver.
//!
//! The IPC transport uses Windows named pipes (`\\.\pipe\wtd-{user-SID}`).
//! Messages are framed with a 4-byte LE length prefix followed by a UTF-8 JSON
//! envelope. See spec §13 for the full IPC architecture.

pub mod connect;
pub mod error;
pub mod framing;
pub mod message;

/// IPC protocol version. Both client and host must agree on this.
pub const PROTOCOL_VERSION: u32 = 1;

pub use error::IpcError;
pub use framing::{decode, encode, read_frame_async, write_frame_async, MAX_MESSAGE_SIZE};
pub use message::{parse_envelope, Envelope, MessagePayload, TypedMessage};
