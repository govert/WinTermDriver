//! Global user configuration (§11).

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::workspace::ProfileDefinition;

/// Global user configuration loaded from `%APPDATA%\WinTermDriver\settings.yaml` (§11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalSettings {
    /// Built-in profile type or globally defined profile to use when no profile is
    /// specified in the session or workspace defaults. Default: `"powershell"`.
    #[serde(rename = "defaultProfile", default = "default_profile_name")]
    pub default_profile: String,

    /// Globally defined profiles available to all workspaces (§11.2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profiles: HashMap<String, ProfileDefinition>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            default_profile: default_profile_name(),
            profiles: HashMap::new(),
        }
    }
}

fn default_profile_name() -> String {
    "powershell".to_string()
}
