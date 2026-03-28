//! Shared types, identifiers, and error definitions for WinTermDriver.
//!
//! This crate contains the core domain model used across `wtd-host`, `wtd-ui`,
//! and `wtd-cli`. It has no platform-specific dependencies so it can be used
//! in tests and tooling without a Windows target.

pub mod error;
pub mod ids;
pub mod workspace;
pub mod workspace_loader;

pub use workspace_loader::{load_workspace_definition, LoadError, ValidationError};
