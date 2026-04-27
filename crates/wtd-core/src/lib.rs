//! Shared types, identifiers, and error definitions for WinTermDriver.
//!
//! This crate contains the core domain model used across `wtd-host`, `wtd-ui`,
//! and `wtd-cli`. It has no platform-specific dependencies so it can be used
//! in tests and tooling without a Windows target.

pub mod error;
pub mod global_settings;
pub mod ids;
pub mod layout;
pub mod logging;
pub mod profile_resolver;
pub mod recipe;
pub mod target;
pub mod workspace;
pub mod workspace_discovery;
pub mod workspace_loader;

pub use global_settings::{
    default_bindings, effective_bindings, load_global_settings, merge_bindings, tmux_bindings,
    windows_terminal_bindings, FontConfig, GlobalSettings, LogLevel, SettingsLoadError,
    ThemeConfig,
};
pub use profile_resolver::{resolve_launch_spec, ResolveError, ResolvedLaunchSpec};
pub use recipe::{
    expand_recipe_steps, find_recipe, find_recipe_manifest_from, load_recipe_manifest,
    load_recipe_manifest_file, recipe_manifest_candidates, render_recipe_template,
    resolve_step_target, ProjectRecipe, RecipeLoadError, RecipeManifest, RecipeStep, RecipeTarget,
};
pub use target::{TargetPath, TargetPathError};
pub use workspace::BindingPreset;
pub use workspace_discovery::{
    ensure_dir, ensure_user_workspaces_dir, find_workspace, find_workspace_in, list_workspaces,
    list_workspaces_in, user_workspaces_dir, DiscoveredWorkspace, DiscoveryError, WorkspaceSource,
};
pub use workspace_loader::{load_workspace_definition, LoadError, ValidationError};
