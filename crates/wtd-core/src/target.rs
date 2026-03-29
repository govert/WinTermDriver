//! Target path parsing (§19.3–19.4).
//!
//! Parses `/`-separated semantic paths like `dev/backend/server` into
//! structured [`TargetPath`] variants with 1–4 segments.

use std::fmt;

/// Maximum allowed length for a single name segment (§19.1).
const MAX_SEGMENT_LENGTH: usize = 64;

/// A parsed target path (§19.3).
///
/// Represents 1–4 `/`-separated segments used to address runtime objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetPath {
    /// 1 segment: pane name (requires implicit workspace per §19.5).
    Pane { pane: String },
    /// 2 segments: `workspace/pane`.
    WorkspacePane { workspace: String, pane: String },
    /// 3 segments: `workspace/tab/pane`.
    WorkspaceTabPane {
        workspace: String,
        tab: String,
        pane: String,
    },
    /// 4 segments: `workspace/window/tab/pane`.
    WorkspaceWindowTabPane {
        workspace: String,
        window: String,
        tab: String,
        pane: String,
    },
}

impl TargetPath {
    /// Parse a target path string into a [`TargetPath`].
    ///
    /// Validates each segment against §19.1 naming rules:
    /// - Non-empty
    /// - Characters in `[a-zA-Z0-9_-]`
    /// - Maximum 64 characters
    pub fn parse(path: &str) -> Result<Self, TargetPathError> {
        if path.is_empty() {
            return Err(TargetPathError::Empty);
        }

        let segments: Vec<&str> = path.split('/').collect();

        if segments.len() > 4 {
            return Err(TargetPathError::TooManySegments(segments.len()));
        }

        // Validate each segment.
        for (i, seg) in segments.iter().enumerate() {
            validate_segment(seg, i)?;
        }

        match segments.len() {
            1 => Ok(TargetPath::Pane {
                pane: segments[0].to_string(),
            }),
            2 => Ok(TargetPath::WorkspacePane {
                workspace: segments[0].to_string(),
                pane: segments[1].to_string(),
            }),
            3 => Ok(TargetPath::WorkspaceTabPane {
                workspace: segments[0].to_string(),
                tab: segments[1].to_string(),
                pane: segments[2].to_string(),
            }),
            4 => Ok(TargetPath::WorkspaceWindowTabPane {
                workspace: segments[0].to_string(),
                window: segments[1].to_string(),
                tab: segments[2].to_string(),
                pane: segments[3].to_string(),
            }),
            _ => unreachable!(), // Already checked > 4 above.
        }
    }
}

impl fmt::Display for TargetPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetPath::Pane { pane } => write!(f, "{}", pane),
            TargetPath::WorkspacePane { workspace, pane } => {
                write!(f, "{}/{}", workspace, pane)
            }
            TargetPath::WorkspaceTabPane {
                workspace,
                tab,
                pane,
            } => write!(f, "{}/{}/{}", workspace, tab, pane),
            TargetPath::WorkspaceWindowTabPane {
                workspace,
                window,
                tab,
                pane,
            } => write!(f, "{}/{}/{}/{}", workspace, window, tab, pane),
        }
    }
}

/// Error when parsing a target path string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetPathError {
    #[error("target path is empty")]
    Empty,
    #[error("target path has too many segments (max 4, got {0})")]
    TooManySegments(usize),
    #[error("empty segment at position {0}")]
    EmptySegment(usize),
    #[error("segment \"{0}\" contains invalid characters (allowed: a-zA-Z0-9_-)")]
    InvalidCharacters(String),
    #[error("segment \"{0}\" exceeds maximum length of 64 characters")]
    TooLong(String),
}

/// Validate a single name segment per §19.1.
fn validate_segment(seg: &str, index: usize) -> Result<(), TargetPathError> {
    if seg.is_empty() {
        return Err(TargetPathError::EmptySegment(index));
    }
    if seg.len() > MAX_SEGMENT_LENGTH {
        return Err(TargetPathError::TooLong(seg.to_string()));
    }
    if !seg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(TargetPathError::InvalidCharacters(seg.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Successful parsing ──────────────────────────────────────────

    #[test]
    fn parse_one_segment() {
        let tp = TargetPath::parse("server").unwrap();
        assert_eq!(
            tp,
            TargetPath::Pane {
                pane: "server".into()
            }
        );
        assert_eq!(tp.to_string(), "server");
    }

    #[test]
    fn parse_two_segments() {
        let tp = TargetPath::parse("dev/server").unwrap();
        assert_eq!(
            tp,
            TargetPath::WorkspacePane {
                workspace: "dev".into(),
                pane: "server".into(),
            }
        );
        assert_eq!(tp.to_string(), "dev/server");
    }

    #[test]
    fn parse_three_segments() {
        let tp = TargetPath::parse("dev/backend/server").unwrap();
        assert_eq!(
            tp,
            TargetPath::WorkspaceTabPane {
                workspace: "dev".into(),
                tab: "backend".into(),
                pane: "server".into(),
            }
        );
        assert_eq!(tp.to_string(), "dev/backend/server");
    }

    #[test]
    fn parse_four_segments() {
        let tp = TargetPath::parse("dev/main/backend/server").unwrap();
        assert_eq!(
            tp,
            TargetPath::WorkspaceWindowTabPane {
                workspace: "dev".into(),
                window: "main".into(),
                tab: "backend".into(),
                pane: "server".into(),
            }
        );
        assert_eq!(tp.to_string(), "dev/main/backend/server");
    }

    #[test]
    fn segment_with_underscores_and_hyphens() {
        let tp = TargetPath::parse("my_workspace/api-server").unwrap();
        assert_eq!(
            tp,
            TargetPath::WorkspacePane {
                workspace: "my_workspace".into(),
                pane: "api-server".into(),
            }
        );
    }

    #[test]
    fn segment_with_digits() {
        let tp = TargetPath::parse("ws1/pane2").unwrap();
        assert_eq!(
            tp,
            TargetPath::WorkspacePane {
                workspace: "ws1".into(),
                pane: "pane2".into(),
            }
        );
    }

    // ── Validation errors ───────────────────────────────────────────

    #[test]
    fn empty_path() {
        assert_eq!(TargetPath::parse(""), Err(TargetPathError::Empty));
    }

    #[test]
    fn too_many_segments() {
        let err = TargetPath::parse("a/b/c/d/e").unwrap_err();
        assert_eq!(err, TargetPathError::TooManySegments(5));
    }

    #[test]
    fn empty_segment_leading_slash() {
        let err = TargetPath::parse("/server").unwrap_err();
        assert_eq!(err, TargetPathError::EmptySegment(0));
    }

    #[test]
    fn empty_segment_trailing_slash() {
        let err = TargetPath::parse("dev/").unwrap_err();
        assert_eq!(err, TargetPathError::EmptySegment(1));
    }

    #[test]
    fn empty_segment_middle() {
        let err = TargetPath::parse("dev//server").unwrap_err();
        assert_eq!(err, TargetPathError::EmptySegment(1));
    }

    #[test]
    fn invalid_characters_space() {
        let err = TargetPath::parse("my workspace").unwrap_err();
        assert!(matches!(err, TargetPathError::InvalidCharacters(_)));
    }

    #[test]
    fn invalid_characters_dot() {
        let err = TargetPath::parse("dev/server.1").unwrap_err();
        assert!(matches!(err, TargetPathError::InvalidCharacters(_)));
    }

    #[test]
    fn segment_too_long() {
        let long = "a".repeat(65);
        let err = TargetPath::parse(&long).unwrap_err();
        assert!(matches!(err, TargetPathError::TooLong(_)));
    }

    #[test]
    fn max_length_segment_ok() {
        let seg = "a".repeat(64);
        assert!(TargetPath::parse(&seg).is_ok());
    }
}
