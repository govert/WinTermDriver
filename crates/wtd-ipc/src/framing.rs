//! Length-prefixed binary framing for IPC messages (§13.4).
//!
//! Wire format: `[4 bytes: payload length as u32 LE][UTF-8 JSON payload]`
//!
//! Maximum message size: 16 MiB.

use crate::error::IpcError;
use crate::message::Envelope;

/// Maximum message payload size in bytes: 16 MiB.
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Length prefix size in bytes.
pub const LENGTH_PREFIX_SIZE: usize = 4;

/// Encode an [`Envelope`] into a length-prefixed frame.
///
/// Returns a `Vec<u8>` containing the 4-byte LE length prefix followed by the
/// UTF-8 JSON payload.
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, IpcError> {
    let json = serde_json::to_vec(envelope)?;
    if json.len() > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge {
            size: json.len(),
            max: MAX_MESSAGE_SIZE,
        });
    }
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + json.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Decode a length-prefixed frame into an [`Envelope`].
///
/// `data` must contain exactly one complete frame (length prefix + payload).
pub fn decode(data: &[u8]) -> Result<Envelope, IpcError> {
    if data.len() < LENGTH_PREFIX_SIZE {
        return Err(IpcError::FrameTooShort {
            size: data.len(),
            expected: LENGTH_PREFIX_SIZE,
        });
    }
    let len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge {
            size: len,
            max: MAX_MESSAGE_SIZE,
        });
    }
    let payload_end = LENGTH_PREFIX_SIZE + len;
    if data.len() < payload_end {
        return Err(IpcError::FrameTooShort {
            size: data.len(),
            expected: payload_end,
        });
    }
    let payload = &data[LENGTH_PREFIX_SIZE..payload_end];
    let envelope: Envelope = serde_json::from_slice(payload)?;
    Ok(envelope)
}

/// Read the length prefix from the first 4 bytes.
///
/// Returns `None` if fewer than 4 bytes are available. Returns the expected
/// payload length on success. Useful for incremental reading from a pipe.
pub fn read_length_prefix(buf: &[u8]) -> Option<usize> {
    if buf.len() < LENGTH_PREFIX_SIZE {
        return None;
    }
    Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize)
}
