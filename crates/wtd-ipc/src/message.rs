//! IPC message envelope stubs.
//!
//! Full message type definitions live in a dedicated bead (wintermdriver-8w8.1).
//! This module provides the framing infrastructure and placeholder types so
//! other crates can compile against a stable import path.

use serde::{Deserialize, Serialize};

/// Message direction: sent from a client (UI or CLI) to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientMessage {
    /// Correlation ID used to match responses to requests.
    pub id: u64,
    /// The command payload — full enum defined in wintermdriver-8w8.1.
    pub payload: serde_json::Value,
}

/// Message direction: sent from the host to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostMessage {
    /// Correlation ID (matches the originating [`ClientMessage::id`], or 0 for
    /// unsolicited push messages such as VT output).
    pub id: u64,
    /// The response or push payload.
    pub payload: serde_json::Value,
}

/// Named pipe path for the current user.
///
/// The SID segment is filled in at runtime; this function returns the path
/// prefix. Full path construction requires calling `GetTokenInformation` /
/// `ConvertSidToStringSid` (see wtd-host bead).
pub fn pipe_name_prefix() -> &'static str {
    r"\\.\pipe\wtd-"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_roundtrip() {
        let msg = ClientMessage {
            id: 1,
            payload: serde_json::json!({ "action": "ping" }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
    }

    #[test]
    fn host_message_roundtrip() {
        let msg = HostMessage {
            id: 1,
            payload: serde_json::json!({ "ok": true }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: HostMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, 1);
    }

    #[test]
    fn pipe_name_prefix_correct() {
        assert!(pipe_name_prefix().starts_with(r"\\.\pipe\wtd-"));
    }
}
