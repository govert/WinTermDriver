use std::path::Path;

use wtd_core::workspace::{
    PaneDriverDefinition, PaneDriverProfile, SessionLaunchDefinition, WorkspaceDefinition,
};

use crate::terminal_input::encode_key_specs;

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePaneDriver {
    pub profile: String,
    pub submit_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_break_key: Option<String>,
    #[serde(skip)]
    multiline_mode: PromptMultilineMode,
    #[serde(skip)]
    paste_mode: PromptPasteMode,
    #[serde(skip)]
    submit_delay_ms: u64,
    #[serde(skip)]
    prepare_keys: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PromptError {
    #[error("pane driver '{profile}' does not support multiline prompts")]
    MultilineUnsupported { profile: String },
    #[error("invalid pane driver key spec '{key_spec}': {message}")]
    InvalidKeySpec { key_spec: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInputPlan {
    pub body: Vec<u8>,
    pub submit: Vec<u8>,
    pub submit_delay_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptMultilineMode {
    Reject,
    SoftBreakKey,
    LiteralPaste,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptPasteMode {
    BracketedIfEnabled,
    Plain,
}

pub fn encode_send_input(text: &str, newline: bool, bracketed_paste_active: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 16);

    if bracketed_paste_active && text.len() > 1 {
        out.extend_from_slice(BRACKETED_PASTE_START);
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(BRACKETED_PASTE_END);
    } else {
        out.extend_from_slice(text.as_bytes());
    }

    if newline {
        out.push(b'\r');
    }

    out
}

fn encode_prompt_text(
    text: &str,
    newline: bool,
    paste_mode: PromptPasteMode,
    bracketed_paste_active: bool,
) -> Vec<u8> {
    match paste_mode {
        PromptPasteMode::BracketedIfEnabled => {
            encode_send_input(text, newline, bracketed_paste_active)
        }
        PromptPasteMode::Plain => {
            let mut out = Vec::with_capacity(text.len() + usize::from(newline));
            out.extend_from_slice(text.as_bytes());
            if newline {
                out.push(b'\r');
            }
            out
        }
    }
}

pub fn resolve_pane_driver(
    session_def: Option<&SessionLaunchDefinition>,
    workspace_def: Option<&WorkspaceDefinition>,
) -> EffectivePaneDriver {
    resolve_pane_driver_with_inference(session_def, workspace_def, None)
}

pub fn resolve_pane_driver_with_inference(
    session_def: Option<&SessionLaunchDefinition>,
    workspace_def: Option<&WorkspaceDefinition>,
    inferred_profile: Option<PaneDriverProfile>,
) -> EffectivePaneDriver {
    let merged = merge_driver_definition(
        workspace_def
            .and_then(|workspace| workspace.defaults.as_ref())
            .and_then(|defaults| defaults.driver.as_ref()),
        session_def.and_then(|session| session.driver.as_ref()),
    );

    let mut resolved = built_in_driver(
        merged
            .profile
            .clone()
            .or(inferred_profile)
            .unwrap_or(PaneDriverProfile::Plain),
    );

    if let Some(submit_key) = merged.submit_key {
        resolved.submit_key = submit_key;
    }
    if merged.disable_soft_break {
        resolved.soft_break_key = None;
    }
    if let Some(soft_break_key) = merged.soft_break_key {
        resolved.soft_break_key = Some(soft_break_key);
    }

    resolved
}

pub fn build_prompt_input_plan(
    text: &str,
    driver: &EffectivePaneDriver,
    bracketed_paste_active: bool,
) -> Result<PromptInputPlan, PromptError> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut body = if driver.prepare_keys.is_empty() {
        Vec::new()
    } else {
        encode_key_specs(&driver.prepare_keys).map_err(|message| PromptError::InvalidKeySpec {
            key_spec: driver.prepare_keys.join(", "),
            message: message.to_string(),
        })?
    };

    let prompt_body = match driver.multiline_mode {
        PromptMultilineMode::Reject if lines.len() > 1 => {
            return Err(PromptError::MultilineUnsupported {
                profile: driver.profile.clone(),
            });
        }
        PromptMultilineMode::Reject | PromptMultilineMode::LiteralPaste => encode_prompt_text(
            &normalized,
            false,
            driver.paste_mode,
            bracketed_paste_active,
        ),
        PromptMultilineMode::SoftBreakKey => {
            let mut out = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                out.extend_from_slice(&encode_prompt_text(
                    line,
                    false,
                    driver.paste_mode,
                    bracketed_paste_active,
                ));
                if index + 1 < lines.len() {
                    let soft_break_key = driver.soft_break_key.as_ref().expect("configured");
                    let encoded =
                        encode_key_specs(&[soft_break_key.clone()]).map_err(|message| {
                            PromptError::InvalidKeySpec {
                                key_spec: soft_break_key.clone(),
                                message: message.to_string(),
                            }
                        })?;
                    out.extend_from_slice(&encoded);
                }
            }
            out
        }
    };
    body.extend_from_slice(&prompt_body);

    let submit = encode_key_specs(&[driver.submit_key.clone()]).map_err(|message| {
        PromptError::InvalidKeySpec {
            key_spec: driver.submit_key.clone(),
            message: message.to_string(),
        }
    })?;
    Ok(PromptInputPlan {
        body,
        submit,
        submit_delay_ms: driver.submit_delay_ms,
    })
}

pub fn pane_driver_definition_from_effective(driver: &EffectivePaneDriver) -> PaneDriverDefinition {
    PaneDriverDefinition {
        profile: profile_name_to_builtin(&driver.profile),
        submit_key: Some(driver.submit_key.clone()),
        soft_break_key: driver.soft_break_key.clone(),
        disable_soft_break: driver.soft_break_key.is_none(),
    }
}

fn merge_driver_definition(
    defaults: Option<&PaneDriverDefinition>,
    session: Option<&PaneDriverDefinition>,
) -> PaneDriverDefinition {
    let mut merged = PaneDriverDefinition::default();

    if let Some(defaults) = defaults {
        merged.profile = defaults.profile.clone();
        merged.submit_key = defaults.submit_key.clone();
        merged.soft_break_key = defaults.soft_break_key.clone();
        merged.disable_soft_break = defaults.disable_soft_break;
    }

    if let Some(session) = session {
        if let Some(profile) = session.profile.clone() {
            merged.profile = Some(profile);
        }
        if let Some(submit_key) = session.submit_key.clone() {
            merged.submit_key = Some(submit_key);
        }
        if let Some(soft_break_key) = session.soft_break_key.clone() {
            merged.soft_break_key = Some(soft_break_key);
        }
        if session.disable_soft_break {
            merged.disable_soft_break = true;
        }
    }

    merged
}

fn built_in_driver(profile: PaneDriverProfile) -> EffectivePaneDriver {
    match profile {
        PaneDriverProfile::Plain => EffectivePaneDriver {
            profile: "plain".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: None,
            multiline_mode: PromptMultilineMode::Reject,
            paste_mode: PromptPasteMode::BracketedIfEnabled,
            submit_delay_ms: 0,
            prepare_keys: Vec::new(),
        },
        PaneDriverProfile::Codex => EffectivePaneDriver {
            profile: "codex".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: None,
            // Match the known-good Ctrl+Shift+V path in wtd-ui:
            // plain multiline paste, then a real Enter to submit.
            multiline_mode: PromptMultilineMode::LiteralPaste,
            paste_mode: PromptPasteMode::Plain,
            // Codex needs the submit to land noticeably after the paste bytes.
            // A larger gap than the low-level send+keys path is needed because
            // prompt writes both steps from within the host.
            submit_delay_ms: 200,
            // New Codex panes often start with a suggested draft. Select all
            // first so `wtd prompt` replaces that draft instead of appending.
            prepare_keys: vec!["Ctrl+A".to_string()],
        },
        PaneDriverProfile::Pi => EffectivePaneDriver {
            profile: "pi".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: Some("Shift+Enter".to_string()),
            multiline_mode: PromptMultilineMode::SoftBreakKey,
            paste_mode: PromptPasteMode::BracketedIfEnabled,
            submit_delay_ms: 0,
            prepare_keys: Vec::new(),
        },
        PaneDriverProfile::ClaudeCode => EffectivePaneDriver {
            profile: "claude-code".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: Some("Shift+Enter".to_string()),
            multiline_mode: PromptMultilineMode::SoftBreakKey,
            paste_mode: PromptPasteMode::BracketedIfEnabled,
            submit_delay_ms: 0,
            prepare_keys: Vec::new(),
        },
        PaneDriverProfile::GeminiCli => EffectivePaneDriver {
            profile: "gemini-cli".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: Some("Shift+Enter".to_string()),
            multiline_mode: PromptMultilineMode::SoftBreakKey,
            paste_mode: PromptPasteMode::BracketedIfEnabled,
            submit_delay_ms: 0,
            prepare_keys: Vec::new(),
        },
        PaneDriverProfile::CopilotCli => EffectivePaneDriver {
            profile: "copilot-cli".to_string(),
            submit_key: "Enter".to_string(),
            soft_break_key: Some("Shift+Enter".to_string()),
            multiline_mode: PromptMultilineMode::SoftBreakKey,
            paste_mode: PromptPasteMode::BracketedIfEnabled,
            submit_delay_ms: 0,
            prepare_keys: Vec::new(),
        },
    }
}

pub fn infer_pane_driver_profile(
    startup_command: Option<&str>,
    executable: Option<&str>,
) -> Option<PaneDriverProfile> {
    startup_command
        .and_then(first_command_token)
        .and_then(profile_from_program_name)
        .or_else(|| executable.and_then(profile_from_program_name))
}

fn profile_name_to_builtin(name: &str) -> Option<PaneDriverProfile> {
    match name {
        "plain" => Some(PaneDriverProfile::Plain),
        "codex" => Some(PaneDriverProfile::Codex),
        "pi" => Some(PaneDriverProfile::Pi),
        "claude-code" => Some(PaneDriverProfile::ClaudeCode),
        "gemini-cli" => Some(PaneDriverProfile::GeminiCli),
        "copilot-cli" => Some(PaneDriverProfile::CopilotCli),
        _ => None,
    }
}

fn first_command_token(command: &str) -> Option<&str> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    let bytes = trimmed.as_bytes();
    if bytes[0] == b'"' || bytes[0] == b'\'' {
        let quote = bytes[0];
        let rest = &trimmed[1..];
        let end = rest.find(quote as char).unwrap_or(rest.len());
        let token = &rest[..end];
        if token.is_empty() {
            None
        } else {
            Some(token)
        }
    } else {
        trimmed.split_whitespace().next()
    }
}

fn profile_from_program_name(program: &str) -> Option<PaneDriverProfile> {
    let file_name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    let normalized = file_name
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase();
    let normalized = normalized
        .strip_suffix(".exe")
        .or_else(|| normalized.strip_suffix(".cmd"))
        .or_else(|| normalized.strip_suffix(".bat"))
        .or_else(|| normalized.strip_suffix(".com"))
        .or_else(|| normalized.strip_suffix(".ps1"))
        .unwrap_or(&normalized);

    match normalized {
        "codex" => Some(PaneDriverProfile::Codex),
        "claude" | "claude-code" => Some(PaneDriverProfile::ClaudeCode),
        "gemini" | "gemini-cli" => Some(PaneDriverProfile::GeminiCli),
        "copilot" | "copilot-cli" | "github-copilot" | "github-copilot-cli" => {
            Some(PaneDriverProfile::CopilotCli)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtd_core::workspace::{DefaultsDefinition, WorkspaceDefinition};

    fn workspace_with_default_driver(driver: PaneDriverDefinition) -> WorkspaceDefinition {
        WorkspaceDefinition {
            version: 1,
            name: "test".to_string(),
            description: None,
            defaults: Some(DefaultsDefinition {
                profile: None,
                restart_policy: None,
                scrollback_lines: None,
                cwd: None,
                env: None,
                terminal_size: None,
                driver: Some(driver),
            }),
            profiles: None,
            bindings: None,
            windows: None,
            tabs: None,
        }
    }

    #[test]
    fn resolve_driver_defaults_to_plain() {
        let driver = resolve_pane_driver(None, None);
        assert_eq!(driver.profile, "plain");
        assert_eq!(driver.submit_key, "Enter");
        assert_eq!(driver.soft_break_key, None);
    }

    #[test]
    fn resolve_driver_applies_profile_and_overrides() {
        let workspace = workspace_with_default_driver(PaneDriverDefinition {
            profile: Some(PaneDriverProfile::ClaudeCode),
            submit_key: None,
            soft_break_key: None,
            disable_soft_break: false,
        });
        let session = SessionLaunchDefinition {
            driver: Some(PaneDriverDefinition {
                profile: Some(PaneDriverProfile::CopilotCli),
                submit_key: Some("Ctrl+Enter".to_string()),
                soft_break_key: None,
                disable_soft_break: false,
            }),
            ..Default::default()
        };

        let driver = resolve_pane_driver(Some(&session), Some(&workspace));
        assert_eq!(driver.profile, "copilot-cli");
        assert_eq!(driver.submit_key, "Ctrl+Enter");
        assert_eq!(driver.soft_break_key.as_deref(), Some("Shift+Enter"));
    }

    #[test]
    fn build_prompt_input_single_line_submits_with_enter() {
        let driver = resolve_pane_driver(None, None);
        let plan = build_prompt_input_plan("hello", &driver, false).unwrap();
        assert_eq!(plan.body, b"hello");
        assert_eq!(plan.submit, b"\r");
        assert_eq!(plan.submit_delay_ms, 0);
    }

    #[test]
    fn build_prompt_input_multiline_uses_soft_break() {
        let driver = resolve_pane_driver(
            Some(&SessionLaunchDefinition {
                driver: Some(PaneDriverDefinition {
                    profile: Some(PaneDriverProfile::ClaudeCode),
                    submit_key: None,
                    soft_break_key: None,
                    disable_soft_break: false,
                }),
                ..Default::default()
            }),
            None,
        );

        let plan = build_prompt_input_plan("first\nsecond", &driver, false).unwrap();
        assert_eq!(plan.body, b"first\x1b[13;2usecond");
        assert_eq!(plan.submit, b"\r");
        assert_eq!(plan.submit_delay_ms, 0);
    }

    #[test]
    fn build_prompt_input_pi_profile_uses_shift_enter_soft_breaks() {
        let driver = resolve_pane_driver(
            Some(&SessionLaunchDefinition {
                driver: Some(PaneDriverDefinition {
                    profile: Some(PaneDriverProfile::Pi),
                    submit_key: None,
                    soft_break_key: None,
                    disable_soft_break: false,
                }),
                ..Default::default()
            }),
            None,
        );

        let plan = build_prompt_input_plan("first\nsecond", &driver, true).unwrap();
        assert_eq!(driver.profile, "pi");
        assert_eq!(
            plan.body,
            b"\x1b[200~first\x1b[201~\x1b[13;2u\x1b[200~second\x1b[201~"
        );
        assert_eq!(plan.submit, b"\r");
        assert_eq!(plan.submit_delay_ms, 0);
    }

    #[test]
    fn build_prompt_input_rejects_multiline_for_plain_profile() {
        let driver = resolve_pane_driver(
            Some(&SessionLaunchDefinition {
                driver: Some(PaneDriverDefinition {
                    profile: Some(PaneDriverProfile::Plain),
                    submit_key: None,
                    soft_break_key: None,
                    disable_soft_break: false,
                }),
                ..Default::default()
            }),
            None,
        );

        let err = build_prompt_input_plan("first\nsecond", &driver, false).unwrap_err();
        assert_eq!(
            err,
            PromptError::MultilineUnsupported {
                profile: "plain".to_string(),
            }
        );
    }

    #[test]
    fn build_prompt_input_uses_bracketed_paste_per_line() {
        let driver = resolve_pane_driver(None, None);
        let plan = build_prompt_input_plan("hello", &driver, true).unwrap();
        assert_eq!(plan.body, b"\x1b[200~hello\x1b[201~");
        assert_eq!(plan.submit, b"\r");
        assert_eq!(plan.submit_delay_ms, 0);
    }

    #[test]
    fn build_prompt_input_codex_pastes_multiline_plain_text_then_submits() {
        let driver = resolve_pane_driver(
            Some(&SessionLaunchDefinition {
                driver: Some(PaneDriverDefinition {
                    profile: Some(PaneDriverProfile::Codex),
                    submit_key: None,
                    soft_break_key: None,
                    disable_soft_break: false,
                }),
                ..Default::default()
            }),
            None,
        );

        let plan = build_prompt_input_plan("first\nsecond", &driver, true).unwrap();
        assert_eq!(plan.body, b"\x01first\nsecond");
        assert_eq!(plan.submit, b"\r");
        assert_eq!(plan.submit_delay_ms, 200);
    }

    #[test]
    fn infer_driver_profile_prefers_startup_command() {
        assert_eq!(
            infer_pane_driver_profile(
                Some("codex --dangerously-skip-permissions"),
                Some("pwsh.exe")
            ),
            Some(PaneDriverProfile::Codex)
        );
    }

    #[test]
    fn infer_driver_profile_falls_back_to_executable_name() {
        assert_eq!(
            infer_pane_driver_profile(None, Some(r"C:\Tools\Claude-Code.exe")),
            Some(PaneDriverProfile::ClaudeCode)
        );
        assert_eq!(
            infer_pane_driver_profile(None, Some("gemini.cmd")),
            Some(PaneDriverProfile::GeminiCli)
        );
    }

    #[test]
    fn infer_driver_profile_returns_none_for_unknown_programs() {
        assert_eq!(
            infer_pane_driver_profile(Some("python runner.py"), Some("pwsh.exe")),
            None
        );
    }
}
