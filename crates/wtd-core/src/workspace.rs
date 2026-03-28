//! Core workspace domain types (definition layer stubs).
//!
//! Full parsing and validation lives in a dedicated bead. These stubs give
//! other crates a stable import path for the workspace types.

use serde::{Deserialize, Serialize};

/// A validated workspace name (non-empty, no path separators).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    pub fn new(name: impl Into<String>) -> Result<Self, crate::error::CoreError> {
        let s = name.into();
        if s.is_empty() || s.contains('/') || s.contains('\\') {
            return Err(crate::error::CoreError::InvalidName(s));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_name_accepted() {
        let name = WorkspaceName::new("my-workspace").unwrap();
        assert_eq!(name.as_str(), "my-workspace");
    }

    #[test]
    fn empty_name_rejected() {
        assert!(WorkspaceName::new("").is_err());
    }

    #[test]
    fn name_with_slash_rejected() {
        assert!(WorkspaceName::new("foo/bar").is_err());
    }
}
