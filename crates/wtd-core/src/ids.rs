//! Opaque identifier types for workspace instances, sessions, panes, and tabs.
//!
//! Semantic names (e.g. "dev/server") are the primary addressing mechanism per
//! the spec (§3.3). These IDs are internal secondary identifiers.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! newtype_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

newtype_id!(
    WorkspaceInstanceId,
    "Opaque ID for a running workspace instance."
);
newtype_id!(
    SessionId,
    "Opaque ID for a PTY session within a workspace instance."
);
newtype_id!(PaneId, "Opaque ID for a UI pane viewport.");
newtype_id!(TabId, "Opaque ID for a tab.");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_roundtrip() {
        let id = SessionId(42);
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
