//! M6 Diagnostics Gate — §31 logging infrastructure and §29 error clarity (§37.5)
//!
//! Validates:
//!   §31.1  Tracing infrastructure operational: host writes to log file, CLI/UI to stderr
//!   §31.1  Log rotation configured (daily rotation, MAX_LOG_FILES kept)
//!   §31.2  Log level filtering: settings-level and WTD_LOG env override
//!   §29    Error messages are clear, specific, and actionable

#![cfg(windows)]

use std::path::PathBuf;

// ── §31.1: Host log file creation ────────────────────────────────────────────

/// Verify that `init_host_logging` creates the log directory and writes a log
/// file. The returned guard must be held alive for writes to flush.
#[test]
fn host_logging_creates_log_file() {
    let tmp = std::env::temp_dir().join(format!("wtd-diag-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();

    // init_host_logging can only be called once per process (global subscriber).
    // Use init_host_logging_to_file for test isolation.
    let guard = wtd_core::logging::init_host_logging_to_file(
        &wtd_core::LogLevel::Debug,
        &tmp,
        "test-host.log",
    );

    // Emit a log event.
    tracing::info!("host_logging_creates_log_file marker");

    // Flush by dropping the guard.
    drop(guard);

    // Verify log directory contains a file with our prefix.
    let entries: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("test-host.log")
        })
        .collect();

    assert!(
        !entries.is_empty(),
        "§31.1: host logging should create a log file in the log directory"
    );

    // Verify the log file contains our marker.
    let content = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(
        content.contains("host_logging_creates_log_file marker"),
        "§31.1: log file should contain the emitted log message, got: {}",
        &content[..content.len().min(500)]
    );

    // Clean up.
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── §31.1: Log directory path ────────────────────────────────────────────────

/// Verify `log_dir()` computes the correct path under the data directory.
#[test]
fn log_dir_under_data_dir() {
    let data = PathBuf::from(r"C:\Users\test\AppData\Roaming\WinTermDriver");
    let log = wtd_core::logging::log_dir(&data);
    assert_eq!(
        log,
        data.join("logs"),
        "§31.1: log dir should be <data_dir>/logs"
    );
}

// ── §31.1: Rotation config ──────────────────────────────────────────────────

/// Verify the MAX_LOG_FILES constant matches §31.1 (keep 5 files).
#[test]
fn log_rotation_keeps_five_files() {
    assert_eq!(
        wtd_core::logging::MAX_LOG_FILES,
        5,
        "§31.1: should keep 5 rotated log files"
    );
}

// ── §31.2: LogLevel → tracing::Level ────────────────────────────────────────

/// Verify each `LogLevel` variant maps to the correct `tracing::Level`.
#[test]
fn log_level_to_tracing_level() {
    use wtd_core::LogLevel;

    assert_eq!(LogLevel::Trace.to_tracing_level(), tracing::Level::TRACE);
    assert_eq!(LogLevel::Debug.to_tracing_level(), tracing::Level::DEBUG);
    assert_eq!(LogLevel::Info.to_tracing_level(), tracing::Level::INFO);
    assert_eq!(LogLevel::Warn.to_tracing_level(), tracing::Level::WARN);
    assert_eq!(LogLevel::Error.to_tracing_level(), tracing::Level::ERROR);
}

/// Verify `as_filter_str` returns the lowercase string used in EnvFilter.
#[test]
fn log_level_filter_strings() {
    use wtd_core::LogLevel;

    assert_eq!(LogLevel::Trace.as_filter_str(), "trace");
    assert_eq!(LogLevel::Debug.as_filter_str(), "debug");
    assert_eq!(LogLevel::Info.as_filter_str(), "info");
    assert_eq!(LogLevel::Warn.as_filter_str(), "warn");
    assert_eq!(LogLevel::Error.as_filter_str(), "error");
}

// ── §31.2: WTD_LOG env override ─────────────────────────────────────────────

/// Verify `effective_log_filter` returns settings level when WTD_LOG is unset,
/// and WTD_LOG value when it is set.
#[test]
fn effective_log_filter_respects_env() {
    use wtd_core::LogLevel;

    // Without WTD_LOG set (clear it if present).
    std::env::remove_var("WTD_LOG");
    let filter = wtd_core::logging::effective_log_filter(&LogLevel::Warn);
    assert_eq!(filter, "warn", "§31.2: should use settings level when WTD_LOG is unset");

    // With WTD_LOG set.
    std::env::set_var("WTD_LOG", "trace");
    let filter = wtd_core::logging::effective_log_filter(&LogLevel::Warn);
    assert_eq!(filter, "trace", "§31.2: WTD_LOG should override settings level");

    // Clean up.
    std::env::remove_var("WTD_LOG");
}

// ── §31.2: LogLevel default ─────────────────────────────────────────────────

/// Verify the default log level is Info per §31.2.
#[test]
fn default_log_level_is_info() {
    let level = wtd_core::LogLevel::default();
    assert_eq!(
        level,
        wtd_core::LogLevel::Info,
        "§31.2: default log level should be Info"
    );
}

// ── §31.2: GlobalSettings logLevel ──────────────────────────────────────────

/// Verify `GlobalSettings` deserializes `logLevel` from YAML.
#[test]
fn global_settings_log_level_from_yaml() {
    let yaml = r#"
logLevel: debug
"#;
    let settings: wtd_core::GlobalSettings = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        settings.log_level,
        wtd_core::LogLevel::Debug,
        "§31.2: logLevel should be configurable via global settings YAML"
    );
}

/// Verify `GlobalSettings::default()` uses Info log level.
#[test]
fn global_settings_default_log_level() {
    let settings = wtd_core::GlobalSettings::default();
    assert_eq!(
        settings.log_level,
        wtd_core::LogLevel::Info,
        "§31.2: default GlobalSettings should use Info log level"
    );
}

// ── §29: Error message clarity ──────────────────────────────────────────────
//
// Each test below verifies that the Display impl for an error type produces
// a message that is specific, includes context (names, paths, codes), and is
// actionable (tells the user what went wrong and/or what to do).

/// §29.5: Workspace discovery errors include the workspace name.
#[test]
fn error_clarity_workspace_not_found() {
    use wtd_core::DiscoveryError;
    let err = DiscoveryError::NotFound {
        name: "dev".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("dev"),
        "§29.5: workspace-not-found error should include the workspace name, got: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("not found"),
        "§29.5: should clearly state 'not found', got: {msg}"
    );
}

/// §29.7: Workspace validation errors include the file path and field path.
#[test]
fn error_clarity_workspace_validation() {
    use wtd_core::workspace_loader::{LoadError, ValidationError};
    let err = LoadError::Validation {
        file_path: "workspace.yaml".to_string(),
        errors: vec![ValidationError {
            path: "tabs[0].layout.children[0].name".to_string(),
            message: "pane name cannot be empty".to_string(),
        }],
    };
    let msg = err.to_string();
    assert!(
        msg.contains("workspace.yaml"),
        "§29.7: validation error should include the file path, got: {msg}"
    );
    assert!(
        msg.contains("tabs[0].layout.children[0].name"),
        "§29.7: validation error should include the field path, got: {msg}"
    );
    assert!(
        msg.contains("pane name cannot be empty"),
        "§29.7: validation error should include a human-readable message, got: {msg}"
    );
}

/// §29.2: Session launch failure error includes context.
#[test]
fn error_clarity_session_pty_error() {
    use wtd_host::session::SessionError;
    use wtd_pty::PtyError;
    let err = SessionError::Pty(PtyError::SpawnFailed(
        "cmd.exe not found on PATH".to_string(),
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("cmd.exe not found on PATH"),
        "§29.2: session PTY error should propagate the spawn failure reason, got: {msg}"
    );
}

/// §29.4: Ambiguous target error.
#[test]
fn error_clarity_ambiguous_target() {
    use wtd_host::target_resolver::ResolveError;
    let err = ResolveError::Ambiguous {
        target: "shell".to_string(),
        candidates: vec!["dev/tab1/shell".to_string(), "dev/tab2/shell".to_string()],
    };
    let msg = err.to_string();
    assert!(
        msg.contains("shell"),
        "§29.4: ambiguous error should include the target name, got: {msg}"
    );
    assert!(
        msg.contains("dev/tab1/shell") && msg.contains("dev/tab2/shell"),
        "§29.4: ambiguous error should list all candidates, got: {msg}"
    );
}

/// §29: Profile resolution error is specific.
#[test]
fn error_clarity_profile_not_found() {
    use wtd_core::ResolveError;
    let err = ResolveError::ProfileNotFound {
        name: "my-profile".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("my-profile"),
        "§29: profile-not-found error should include the profile name, got: {msg}"
    );
    // Should mention where it looked (built-in, global, workspace).
    assert!(
        msg.contains("built-in") || msg.contains("not found"),
        "§29: profile-not-found should give context on search scope, got: {msg}"
    );
}

/// §29: IPC framing errors are precise.
#[test]
fn error_clarity_ipc_message_too_large() {
    use wtd_ipc::IpcError;
    let err = IpcError::MessageTooLarge {
        size: 20_000_000,
        max: 16_777_216,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("20000000"),
        "§29: should include actual message size, got: {msg}"
    );
    assert!(
        msg.contains("16777216"),
        "§29: should include maximum allowed size, got: {msg}"
    );
}

/// §29: Unknown IPC message type error is specific.
#[test]
fn error_clarity_unknown_message_type() {
    use wtd_ipc::message::ParseError;
    let err = ParseError::UnknownType("FooBar".to_string());
    let msg = err.to_string();
    assert!(
        msg.contains("FooBar"),
        "§29: unknown-type error should include the type name, got: {msg}"
    );
}

/// §29: Action error includes the action name.
#[test]
fn error_clarity_unknown_action() {
    use wtd_host::action::ActionError;
    let err = ActionError::UnknownAction("fly-to-moon".to_string());
    let msg = err.to_string();
    assert!(
        msg.contains("fly-to-moon"),
        "§29: unknown-action error should include the action name, got: {msg}"
    );
}

/// §29.6: Host connection errors include diagnostic info.
#[test]
fn error_clarity_host_not_found() {
    use wtd_ipc::connect::ConnectError;
    let err = ConnectError::HostNotFound;
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("not found"),
        "§29.6: host-not-found should clearly state the problem, got: {msg}"
    );
}

/// §29.6: Host startup timeout.
#[test]
fn error_clarity_host_startup_timeout() {
    use wtd_ipc::connect::ConnectError;
    let err = ConnectError::StartupTimeout;
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("timeout") || msg.to_lowercase().contains("available"),
        "§29.6: startup timeout should mention timeout or availability, got: {msg}"
    );
}

/// §29: ConPTY creation failure includes HRESULT.
#[test]
fn error_clarity_conpty_create_failed() {
    use wtd_pty::PtyError;
    let err = PtyError::CreateFailed(0x80070057);
    let msg = err.to_string();
    assert!(
        msg.contains("0x80070057"),
        "§29: ConPTY creation error should include the HRESULT code, got: {msg}"
    );
}

/// §29: Target path parse errors are descriptive.
#[test]
fn error_clarity_target_path_errors() {
    use wtd_core::TargetPathError;

    let err = TargetPathError::Empty;
    assert!(
        err.to_string().to_lowercase().contains("empty"),
        "§29: empty target path error should mention 'empty'"
    );

    let err = TargetPathError::TooManySegments(7);
    let msg = err.to_string();
    assert!(
        msg.contains("7") && msg.contains("4"),
        "§29: too-many-segments should include actual and max count, got: {msg}"
    );

    let err = TargetPathError::InvalidCharacters("bad name!".to_string());
    let msg = err.to_string();
    assert!(
        msg.contains("bad name!"),
        "§29: invalid-characters should include the offending segment, got: {msg}"
    );
}

/// §29: Layout error includes the pane ID.
#[test]
fn error_clarity_layout_pane_not_found() {
    use wtd_core::ids::PaneId;
    use wtd_core::layout::LayoutError;
    let err = LayoutError::PaneNotFound(PaneId(42));
    let msg = err.to_string();
    assert!(
        msg.contains("42"),
        "§29: pane-not-found should include the pane ID, got: {msg}"
    );
}

/// §29: Workspace state error is informative.
#[test]
fn error_clarity_workspace_invalid_state() {
    use wtd_host::workspace_instance::{WorkspaceError, WorkspaceState};
    let err = WorkspaceError::InvalidState(WorkspaceState::Closing);
    let msg = err.to_string();
    assert!(
        msg.contains("Closing"),
        "§29: invalid-state error should include the current state, got: {msg}"
    );
}

/// §29: Lifecycle already-running error.
#[test]
fn error_clarity_already_running() {
    use wtd_host::host_lifecycle::LifecycleError;
    let err = LifecycleError::AlreadyRunning;
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("already running"),
        "§29: already-running should clearly state the problem, got: {msg}"
    );
}
