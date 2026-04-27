//! Project-local WTD recipe manifests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RecipeLoadError {
    #[error("failed to parse recipe manifest {file}: {source}")]
    Parse {
        file: String,
        source: serde_yaml::Error,
    },
    #[error("invalid recipe manifest {file}: {message}")]
    Validation { file: String, message: String },
    #[error("no WTD recipe manifest found from {0}")]
    NotFound(String),
    #[error("failed to read recipe manifest {file}: {source}")]
    Io {
        file: String,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecipeManifest {
    pub version: u32,
    #[serde(default)]
    pub commands: Vec<ProjectRecipe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRecipe {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<RecipeTarget>,
    #[serde(default = "default_palette")]
    pub palette: bool,
    #[serde(default)]
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RecipeStep {
    Prompt {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        text: String,
    },
    Capture {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<u32>,
    },
    Wait {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default = "default_wait_condition")]
        condition: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<f64>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            rename = "recentLines"
        )]
        recent_lines: Option<u32>,
    },
    Action {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<HashMap<String, String>>,
    },
}

fn default_palette() -> bool {
    true
}

fn default_wait_condition() -> String {
    "done".to_string()
}

pub fn load_recipe_manifest(
    file_name: impl Into<String>,
    content: &str,
) -> Result<RecipeManifest, RecipeLoadError> {
    let file = file_name.into();
    let manifest: RecipeManifest =
        serde_yaml::from_str(content).map_err(|source| RecipeLoadError::Parse {
            file: file.clone(),
            source,
        })?;
    validate_manifest(&file, &manifest)?;
    Ok(manifest)
}

pub fn load_recipe_manifest_file(path: &Path) -> Result<RecipeManifest, RecipeLoadError> {
    let file = path.to_string_lossy().to_string();
    let content = std::fs::read_to_string(path).map_err(|source| RecipeLoadError::Io {
        file: file.clone(),
        source,
    })?;
    load_recipe_manifest(file, &content)
}

pub fn find_recipe_manifest_from(start: &Path) -> Result<PathBuf, RecipeLoadError> {
    for dir in start.ancestors() {
        for candidate in recipe_manifest_candidates(dir) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(RecipeLoadError::NotFound(start.display().to_string()))
}

pub fn recipe_manifest_candidates(dir: &Path) -> [PathBuf; 2] {
    [
        dir.join(".wtd").join("recipes.yaml"),
        dir.join("wtd-recipes.yaml"),
    ]
}

pub fn find_recipe<'a>(manifest: &'a RecipeManifest, name: &str) -> Option<&'a ProjectRecipe> {
    manifest.commands.iter().find(|recipe| recipe.name == name)
}

pub fn resolve_step_target(recipe: &ProjectRecipe, step_target: Option<&str>) -> Option<String> {
    if let Some(target) = step_target {
        if !target.trim().is_empty() {
            return Some(target.to_string());
        }
    }
    recipe.target.as_ref().and_then(RecipeTarget::to_path)
}

impl RecipeTarget {
    pub fn to_path(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(workspace) = self.workspace.as_deref().filter(|s| !s.is_empty()) {
            parts.push(workspace);
        }
        if let Some(tab) = self.tab.as_deref().filter(|s| !s.is_empty()) {
            parts.push(tab);
        }
        if let Some(pane) = self.pane.as_deref().filter(|s| !s.is_empty()) {
            parts.push(pane);
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("/"))
        }
    }
}

fn validate_manifest(file: &str, manifest: &RecipeManifest) -> Result<(), RecipeLoadError> {
    if manifest.version != 1 {
        return validation_error(file, format!("unsupported version {}", manifest.version));
    }
    if manifest.commands.is_empty() {
        return validation_error(file, "commands must contain at least one recipe");
    }
    let mut names = std::collections::HashSet::new();
    for recipe in &manifest.commands {
        if recipe.name.trim().is_empty() {
            return validation_error(file, "recipe name must not be empty");
        }
        if !names.insert(recipe.name.as_str()) {
            return validation_error(file, format!("duplicate recipe '{}'", recipe.name));
        }
        if recipe.steps.is_empty() {
            return validation_error(file, format!("recipe '{}' must contain steps", recipe.name));
        }
        for step in &recipe.steps {
            validate_step(file, &recipe.name, step)?;
        }
    }
    Ok(())
}

fn validate_step(file: &str, recipe: &str, step: &RecipeStep) -> Result<(), RecipeLoadError> {
    match step {
        RecipeStep::Prompt { text, .. } if text.trim().is_empty() => {
            validation_error(file, format!("recipe '{recipe}' has empty prompt text"))
        }
        RecipeStep::Wait {
            condition, timeout, ..
        } => {
            match condition.as_str() {
                "idle" | "done" | "needs-attention" | "error" | "queue-empty" | "state-change" => {}
                other => {
                    return validation_error(
                        file,
                        format!("recipe '{recipe}' has unsupported wait condition '{other}'"),
                    )
                }
            }
            if timeout.is_some_and(|value| value < 0.0) {
                return validation_error(file, format!("recipe '{recipe}' has negative timeout"));
            }
            Ok(())
        }
        RecipeStep::Action { action, .. } if action.trim().is_empty() => {
            validation_error(file, format!("recipe '{recipe}' has empty action"))
        }
        _ => Ok(()),
    }
}

fn validation_error<T>(file: &str, message: impl Into<String>) -> Result<T, RecipeLoadError> {
    Err(RecipeLoadError::Validation {
        file: file.to_string(),
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
version: 1
commands:
  - name: test-and-review
    description: Run tests and ask reviewer
    cwd: .
    env:
      RUST_LOG: info
    target:
      workspace: dev
      tab: main
      pane: tests
    palette: true
    steps:
      - type: prompt
        text: cargo test
      - type: wait
        condition: done
        timeout: 60
      - type: capture
        lines: 80
      - type: action
        action: focus-pane
        target: dev/main/reviewer
"#;

    #[test]
    fn loads_valid_manifest_and_resolves_default_target() {
        let manifest = load_recipe_manifest("wtd-recipes.yaml", VALID).unwrap();
        let recipe = find_recipe(&manifest, "test-and-review").unwrap();
        assert_eq!(
            recipe.description.as_deref(),
            Some("Run tests and ask reviewer")
        );
        assert_eq!(
            resolve_step_target(recipe, None).as_deref(),
            Some("dev/main/tests")
        );
        assert_eq!(
            resolve_step_target(recipe, Some("dev/main/reviewer")).as_deref(),
            Some("dev/main/reviewer")
        );
    }

    #[test]
    fn rejects_schema_failures() {
        let err = load_recipe_manifest(
            "bad.yaml",
            r#"
version: 1
commands:
  - name: bad
    steps:
      - type: wait
        condition: unknown
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unsupported wait condition"));
    }
}
