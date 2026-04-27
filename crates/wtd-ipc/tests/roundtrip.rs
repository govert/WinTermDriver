//! Round-trip serialization tests for every IPC message type.
//!
//! Each test: construct payload → wrap in Envelope → serialize JSON →
//! deserialize JSON → assert equality + correct type discriminator.

use wtd_ipc::message::*;

/// Helper: wrap a payload in an envelope, serialize, deserialize, and verify.
fn roundtrip<P: MessagePayload + std::fmt::Debug + Clone + PartialEq>(payload: P) {
    let envelope = Envelope::new("test-uuid-001", &payload);

    // Verify type name is set correctly.
    assert_eq!(envelope.msg_type, P::TYPE_NAME);

    // Serialize to JSON.
    let json = serde_json::to_string(&envelope).unwrap();

    // Verify the JSON contains the expected type field.
    let raw: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(raw["type"].as_str().unwrap(), P::TYPE_NAME);
    assert_eq!(raw["id"].as_str().unwrap(), "test-uuid-001");
    assert!(
        raw.get("payload").is_some(),
        "payload field must be present"
    );

    // Deserialize back.
    let back: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(envelope, back);

    // Extract typed payload.
    let extracted: P = back.extract_payload().unwrap();
    assert_eq!(extracted, payload);
}

// ===================================================================
// Client → Host message round-trips
// ===================================================================

#[test]
fn handshake_roundtrip() {
    roundtrip(Handshake {
        client_type: ClientType::Ui,
        client_version: "1.0.0".into(),
        protocol_version: 1,
    });
}

#[test]
fn handshake_cli_roundtrip() {
    roundtrip(Handshake {
        client_type: ClientType::Cli,
        client_version: "0.5.0".into(),
        protocol_version: 2,
    });
}

#[test]
fn open_workspace_roundtrip() {
    roundtrip(OpenWorkspace {
        name: Some("dev".into()),
        file: Some("/path/to/dev.yaml".into()),
        recreate: false,

        profile: None,
    });
}

#[test]
fn open_workspace_minimal_roundtrip() {
    roundtrip(OpenWorkspace {
        name: Some("dev".into()),
        file: None,
        recreate: true,

        profile: None,
    });
}

#[test]
fn attach_workspace_roundtrip() {
    roundtrip(AttachWorkspace {
        workspace: "dev".into(),
    });
}

#[test]
fn close_workspace_roundtrip() {
    roundtrip(CloseWorkspace {
        workspace: "dev".into(),
        kill: true,
    });
}

#[test]
fn recreate_workspace_roundtrip() {
    roundtrip(RecreateWorkspace {
        workspace: "dev".into(),
    });
}

#[test]
fn save_workspace_roundtrip() {
    roundtrip(SaveWorkspace {
        workspace: "dev".into(),
        file: Some("saved.yaml".into()),
    });
}

#[test]
fn list_workspaces_roundtrip() {
    roundtrip(ListWorkspaces {});
}

#[test]
fn list_instances_roundtrip() {
    roundtrip(ListInstances {});
}

#[test]
fn list_panes_roundtrip() {
    roundtrip(ListPanes {
        workspace: "dev".into(),
    });
}

#[test]
fn list_sessions_roundtrip() {
    roundtrip(ListSessions {
        workspace: "dev".into(),
    });
}

#[test]
fn send_roundtrip() {
    roundtrip(Send {
        target: "dev/backend/editor".into(),
        text: "ls -la\n".into(),
        newline: true,
    });
}

#[test]
fn send_no_newline_roundtrip() {
    roundtrip(Send {
        target: "dev/server".into(),
        text: "partial".into(),
        newline: false,
    });
}

#[test]
fn prompt_roundtrip() {
    roundtrip(Prompt {
        target: "dev/server".into(),
        text: "Reply with exactly test".into(),
    });
}

#[test]
fn keys_roundtrip() {
    roundtrip(Keys {
        target: "dev/editor".into(),
        keys: vec!["C-c".into(), "Enter".into()],
    });
}

#[test]
fn capture_roundtrip() {
    roundtrip(Capture {
        target: "dev/editor".into(),
        vt: None,
        lines: None,
        all: None,
        after: None,
        after_regex: None,
        max_lines: None,
        count: None,
    });
}

#[test]
fn capture_with_lines_roundtrip() {
    roundtrip(Capture {
        target: "dev/logs".into(),
        vt: None,
        lines: Some(50),
        all: None,
        after: None,
        after_regex: None,
        max_lines: Some(100),
        count: None,
    });
}

#[test]
fn capture_with_anchor_roundtrip() {
    roundtrip(Capture {
        target: "dev/server".into(),
        vt: None,
        lines: None,
        all: None,
        after: Some("=== BUILD START ===".into()),
        after_regex: None,
        max_lines: Some(200),
        count: None,
    });
}

#[test]
fn capture_with_regex_anchor_roundtrip() {
    roundtrip(Capture {
        target: "dev/ci".into(),
        vt: None,
        lines: None,
        all: None,
        after: None,
        after_regex: Some(r"test\d+ PASS".into()),
        max_lines: None,
        count: None,
    });
}

#[test]
fn capture_all_roundtrip() {
    roundtrip(Capture {
        target: "dev/editor".into(),
        vt: None,
        lines: None,
        all: Some(true),
        after: None,
        after_regex: None,
        max_lines: None,
        count: None,
    });
}

#[test]
fn capture_count_mode_roundtrip() {
    roundtrip(Capture {
        target: "dev/editor".into(),
        vt: None,
        lines: None,
        all: None,
        after: None,
        after_regex: None,
        max_lines: None,
        count: Some(true),
    });
}

#[test]
fn capture_vt_roundtrip() {
    roundtrip(Capture {
        target: "dev/editor".into(),
        vt: Some(true),
        lines: None,
        all: None,
        after: None,
        after_regex: None,
        max_lines: None,
        count: None,
    });
}

#[test]
fn capture_minimal_json_deserializes() {
    // {"target":"x"} must still deserialize with all optional fields → None.
    let json = r#"{"target":"x"}"#;
    let cap: Capture = serde_json::from_str(json).unwrap();
    assert_eq!(cap.target, "x");
    assert!(cap.vt.is_none());
    assert!(cap.lines.is_none());
    assert!(cap.all.is_none());
    assert!(cap.after.is_none());
    assert!(cap.after_regex.is_none());
    assert!(cap.max_lines.is_none());
    assert!(cap.count.is_none());
}

#[test]
fn scrollback_roundtrip() {
    roundtrip(Scrollback {
        target: "dev/logs".into(),
        tail: 100,
    });
}

#[test]
fn follow_roundtrip() {
    roundtrip(Follow {
        target: "dev/logs".into(),
        raw: false,
    });
}

#[test]
fn follow_raw_roundtrip() {
    roundtrip(Follow {
        target: "dev/server".into(),
        raw: true,
    });
}

#[test]
fn cancel_follow_roundtrip() {
    roundtrip(CancelFollow {
        id: "req-42".into(),
    });
}

#[test]
fn inspect_roundtrip() {
    roundtrip(Inspect {
        target: "dev/editor".into(),
    });
}

#[test]
fn invoke_action_roundtrip() {
    roundtrip(InvokeAction {
        action: "split-right".into(),
        target_pane_id: Some("pane-3".into()),
        args: serde_json::json!({}),
    });
}

#[test]
fn invoke_action_no_target_roundtrip() {
    roundtrip(InvokeAction {
        action: "close-pane".into(),
        target_pane_id: None,
        args: serde_json::json!({ "force": true }),
    });
}

#[test]
fn session_input_roundtrip() {
    roundtrip(SessionInput {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        data: "aGVsbG8=".into(), // "hello" base64
    });
}

#[test]
fn configure_pane_roundtrip() {
    roundtrip(ConfigurePane {
        target: "dev/server".into(),
        driver_profile: Some("claude-code".into()),
        submit_key: Some("Enter".into()),
        soft_break_key: Some("Shift+Enter".into()),
        clear_soft_break: false,
        clear_driver: false,
    });
}

#[test]
fn pane_input_roundtrip() {
    roundtrip(PaneInput {
        target: "dev/editor".into(),
        data: "G1s8MzU7NDA7MTJN".into(),
    });
}

#[test]
fn pane_resize_roundtrip() {
    roundtrip(PaneResize {
        pane_id: "pane-1".into(),
        cols: 120,
        rows: 30,
    });
}

#[test]
fn focus_pane_roundtrip() {
    roundtrip(FocusPane {
        pane_id: "pane-2".into(),
    });
}

#[test]
fn rename_pane_roundtrip() {
    roundtrip(RenamePane {
        pane_id: "pane-1".into(),
        new_name: "my-editor".into(),
    });
}

// ===================================================================
// Host → Client message round-trips
// ===================================================================

#[test]
fn handshake_ack_roundtrip() {
    roundtrip(HandshakeAck {
        host_version: "1.0.0".into(),
        protocol_version: 1,
    });
}

#[test]
fn ok_response_roundtrip() {
    roundtrip(OkResponse {});
}

#[test]
fn error_response_roundtrip() {
    roundtrip(ErrorResponse {
        code: ErrorCode::TargetNotFound,
        message: "No pane named 'editor' in workspace 'dev'".into(),
        candidates: Some(vec![
            "dev/backend/editor".into(),
            "dev/ops/prod-shell".into(),
        ]),
    });
}

#[test]
fn error_response_no_candidates_roundtrip() {
    roundtrip(ErrorResponse {
        code: ErrorCode::InternalError,
        message: "unexpected host error".into(),
        candidates: None,
    });
}

#[test]
fn error_codes_roundtrip() {
    // Verify all error codes serialize to the expected kebab-case strings.
    let codes = [
        (ErrorCode::TargetNotFound, "target-not-found"),
        (ErrorCode::TargetAmbiguous, "target-ambiguous"),
        (ErrorCode::WorkspaceNotFound, "workspace-not-found"),
        (
            ErrorCode::WorkspaceAlreadyExists,
            "workspace-already-exists",
        ),
        (ErrorCode::InvalidAction, "invalid-action"),
        (ErrorCode::InvalidArgument, "invalid-argument"),
        (ErrorCode::SessionFailed, "session-failed"),
        (ErrorCode::ProtocolError, "protocol-error"),
        (ErrorCode::InternalError, "internal-error"),
    ];
    for (code, expected_str) in &codes {
        let json = serde_json::to_string(code).unwrap();
        assert_eq!(json, format!("\"{}\"", expected_str));
        let back: ErrorCode = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, code);
    }
}

#[test]
fn open_workspace_result_roundtrip() {
    roundtrip(OpenWorkspaceResult {
        instance_id: "inst-42".into(),
        state: serde_json::json!({ "windows": [] }),
    });
}

#[test]
fn attach_workspace_result_roundtrip() {
    roundtrip(AttachWorkspaceResult {
        state: serde_json::json!({
            "windows": [{ "name": "main", "tabs": [] }]
        }),
    });
}

#[test]
fn recreate_workspace_result_roundtrip() {
    roundtrip(RecreateWorkspaceResult {
        instance_id: "inst-99".into(),
        state: serde_json::json!({}),
    });
}

#[test]
fn list_workspaces_result_roundtrip() {
    roundtrip(ListWorkspacesResult {
        workspaces: vec![
            WorkspaceInfo {
                name: "dev".into(),
                source: "~/.config/wtd/workspaces/dev.yaml".into(),
            },
            WorkspaceInfo {
                name: "ops".into(),
                source: "~/.config/wtd/workspaces/ops.yaml".into(),
            },
        ],
    });
}

#[test]
fn list_instances_result_roundtrip() {
    roundtrip(ListInstancesResult {
        instances: vec![InstanceInfo {
            name: "dev".into(),
            instance_id: "inst-1".into(),
        }],
    });
}

#[test]
fn list_panes_result_roundtrip() {
    roundtrip(ListPanesResult {
        panes: vec![
            PaneInfo {
                name: "editor".into(),
                tab: "backend".into(),
                session_state: "running".into(),
                attention: AttentionState::Active,
                attention_message: None,
            },
            PaneInfo {
                name: "server".into(),
                tab: "backend".into(),
                session_state: "running".into(),
                attention: AttentionState::NeedsAttention,
                attention_message: Some("review needed".into()),
            },
        ],
    });
}

#[test]
fn list_sessions_result_roundtrip() {
    roundtrip(ListSessionsResult {
        sessions: vec![SessionInfo {
            session_id: "sess-1".into(),
            pane: "editor".into(),
            state: "running".into(),
        }],
    });
}

#[test]
fn capture_result_roundtrip() {
    roundtrip(CaptureResult {
        text: "$ ls\nfile1.txt\nfile2.txt\n".into(),
        lines: 3,
        total_lines: 150,
        anchor_found: None,
        cursor: Some(147),
        cols: 80,
        rows: 24,
        on_alternate: false,
        title: Some("shell".into()),
        progress: Some(ProgressInfo {
            state: ProgressState::Normal,
            value: Some(35),
        }),
        mouse_mode: Some("none".into()),
        sgr_mouse: false,
        bracketed_paste: false,
        cursor_row: Some(12),
        cursor_col: Some(7),
        cursor_visible: Some(true),
        cursor_shape: Some("block".into()),
    });
}

#[test]
fn capture_result_with_anchor_roundtrip() {
    roundtrip(CaptureResult {
        text: "=== BUILD START ===\nBuilding...\nDone.\n".into(),
        lines: 3,
        total_lines: 500,
        anchor_found: Some(true),
        cursor: Some(497),
        cols: 80,
        rows: 24,
        on_alternate: true,
        title: None,
        progress: Some(ProgressInfo {
            state: ProgressState::Indeterminate,
            value: None,
        }),
        mouse_mode: Some("any-event".into()),
        sgr_mouse: true,
        bracketed_paste: true,
        cursor_row: Some(23),
        cursor_col: Some(42),
        cursor_visible: Some(false),
        cursor_shape: Some("bar".into()),
    });
}

#[test]
fn capture_result_anchor_not_found_roundtrip() {
    roundtrip(CaptureResult {
        text: "last line\n".into(),
        lines: 1,
        total_lines: 42,
        anchor_found: Some(false),
        cursor: Some(41),
        cols: 80,
        rows: 24,
        on_alternate: false,
        title: None,
        progress: Some(ProgressInfo {
            state: ProgressState::Warning,
            value: Some(64),
        }),
        mouse_mode: Some("normal".into()),
        sgr_mouse: false,
        bracketed_paste: false,
        cursor_row: Some(23),
        cursor_col: Some(0),
        cursor_visible: Some(true),
        cursor_shape: Some("underline".into()),
    });
}

#[test]
fn capture_result_count_mode_roundtrip() {
    roundtrip(CaptureResult {
        text: String::new(),
        lines: 200,
        total_lines: 200,
        anchor_found: None,
        cursor: None,
        cols: 0,
        rows: 0,
        on_alternate: false,
        title: None,
        progress: None,
        mouse_mode: None,
        sgr_mouse: false,
        bracketed_paste: false,
        cursor_row: None,
        cursor_col: None,
        cursor_visible: None,
        cursor_shape: None,
    });
}

#[test]
fn capture_result_legacy_json_deserializes() {
    // Old responses with only "text" must still deserialize.
    let json = r#"{"text":"hello\n"}"#;
    let result: CaptureResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.text, "hello\n");
    assert_eq!(result.lines, 0);
    assert_eq!(result.total_lines, 0);
    assert!(result.anchor_found.is_none());
    assert!(result.cursor.is_none());
    assert_eq!(result.cols, 0);
    assert_eq!(result.rows, 0);
    assert!(!result.on_alternate);
    assert!(result.title.is_none());
}

#[test]
fn scrollback_result_roundtrip() {
    roundtrip(ScrollbackResult {
        lines: vec!["line 1".into(), "line 2".into(), "line 3".into()],
    });
}

#[test]
fn inspect_result_roundtrip() {
    roundtrip(InspectResult {
        data: serde_json::json!({
            "paneId": "pane-1",
            "sessionId": "sess-1",
            "state": "running",
            "title": "bash",
            "cols": 120,
            "rows": 30
        }),
    });
}

#[test]
fn follow_data_roundtrip() {
    roundtrip(FollowData {
        text: "2024-01-15 New connection from...".into(),
    });
}

#[test]
fn follow_end_roundtrip() {
    roundtrip(FollowEnd {
        reason: "session-exited".into(),
        exit_code: Some(0),
    });
}

#[test]
fn follow_end_no_exit_code_roundtrip() {
    roundtrip(FollowEnd {
        reason: "cancelled".into(),
        exit_code: None,
    });
}

#[test]
fn session_output_roundtrip() {
    roundtrip(SessionOutput {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        data: "SGVsbG8gV29ybGQ=".into(), // "Hello World" base64
    });
}

#[test]
fn session_state_changed_roundtrip() {
    roundtrip(SessionStateChanged {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        new_state: "exited".into(),
        exit_code: Some(0),
    });
}

#[test]
fn session_state_changed_no_exit_roundtrip() {
    roundtrip(SessionStateChanged {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        new_state: "running".into(),
        exit_code: None,
    });
}

#[test]
fn title_changed_roundtrip() {
    roundtrip(TitleChanged {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        title: "vim - main.rs".into(),
    });
}

#[test]
fn progress_changed_roundtrip() {
    roundtrip(ProgressChanged {
        workspace: "dev".into(),
        session_id: "sess-1".into(),
        progress: Some(ProgressInfo {
            state: ProgressState::Warning,
            value: Some(64),
        }),
    });
}

#[test]
fn notify_roundtrip() {
    roundtrip(Notify {
        target: "dev/main/server".into(),
        state: AttentionState::NeedsAttention,
        message: Some("input requested".into()),
        source: Some("pi".into()),
    });
}

#[test]
fn clear_attention_roundtrip() {
    roundtrip(ClearAttention {
        target: "dev/main/server".into(),
    });
}

#[test]
fn attention_changed_roundtrip() {
    roundtrip(AttentionChanged {
        workspace: "dev".into(),
        pane_id: Some("11".into()),
        state: AttentionState::Done,
        message: Some("tests passed".into()),
        source: Some("codex".into()),
    });
}

#[test]
fn layout_changed_roundtrip() {
    roundtrip(LayoutChanged {
        workspace: "dev".into(),
        window: "main".into(),
        tab: "backend".into(),
        layout: serde_json::json!({
            "type": "split",
            "orientation": "horizontal",
            "children": [
                { "type": "pane", "name": "editor" },
                { "type": "pane", "name": "server" }
            ]
        }),
    });
}

#[test]
fn workspace_state_changed_roundtrip() {
    roundtrip(WorkspaceStateChanged {
        workspace: "dev".into(),
        new_state: "closing".into(),
    });
}

// ===================================================================
// parse_envelope dispatch tests
// ===================================================================

#[test]
fn parse_envelope_dispatches_all_types() {
    use wtd_ipc::parse_envelope;
    use wtd_ipc::TypedMessage;

    // Handshake
    let env = Envelope::new(
        "id-1",
        &Handshake {
            client_type: ClientType::Cli,
            client_version: "1.0.0".into(),
            protocol_version: 1,
        },
    );
    let parsed = parse_envelope(&env).unwrap();
    assert!(matches!(parsed, TypedMessage::Handshake(_)));

    // Error
    let env = Envelope::new(
        "id-2",
        &ErrorResponse {
            code: ErrorCode::TargetAmbiguous,
            message: "ambiguous".into(),
            candidates: Some(vec!["a".into(), "b".into()]),
        },
    );
    let parsed = parse_envelope(&env).unwrap();
    assert!(matches!(parsed, TypedMessage::ErrorResponse(_)));

    // SessionOutput
    let env = Envelope::new(
        "id-3",
        &SessionOutput {
            workspace: "dev".into(),
            session_id: "s1".into(),
            data: "AA==".into(),
        },
    );
    let parsed = parse_envelope(&env).unwrap();
    assert!(matches!(parsed, TypedMessage::SessionOutput(_)));

    let env = Envelope::new(
        "id-4",
        &Notify {
            target: "dev/main/server".into(),
            state: AttentionState::Done,
            message: None,
            source: None,
        },
    );
    let parsed = parse_envelope(&env).unwrap();
    assert!(matches!(parsed, TypedMessage::Notify(_)));

    let env = Envelope::new(
        "id-5",
        &AttentionChanged {
            workspace: "dev".into(),
            pane_id: Some("1".into()),
            state: AttentionState::Error,
            message: Some("failed".into()),
            source: None,
        },
    );
    let parsed = parse_envelope(&env).unwrap();
    assert!(matches!(parsed, TypedMessage::AttentionChanged(_)));
}

#[test]
fn parse_envelope_unknown_type() {
    use wtd_ipc::parse_envelope;

    let env = Envelope {
        id: "id-1".into(),
        msg_type: "NonExistentMessage".into(),
        payload: serde_json::json!({}),
    };
    let err = parse_envelope(&env).unwrap_err();
    assert!(err.to_string().contains("NonExistentMessage"));
}

// ===================================================================
// Wire format verification
// ===================================================================

#[test]
fn wire_format_matches_spec() {
    // Verify the JSON structure exactly matches §13.5 envelope.
    let env = Envelope::new(
        "req-1",
        &ListPanes {
            workspace: "dev".into(),
        },
    );
    let json: serde_json::Value = serde_json::to_value(&env).unwrap();

    assert_eq!(json["id"], "req-1");
    assert_eq!(json["type"], "ListPanes");
    assert_eq!(json["payload"]["workspace"], "dev");

    // No extra fields.
    let obj = json.as_object().unwrap();
    assert_eq!(obj.len(), 3);
    assert!(obj.contains_key("id"));
    assert!(obj.contains_key("type"));
    assert!(obj.contains_key("payload"));
}

#[test]
fn error_response_wire_format() {
    // Verify error response matches §13.8 example.
    let env = Envelope::new(
        "req-1",
        &ErrorResponse {
            code: ErrorCode::TargetNotFound,
            message: "No pane named 'editor' in workspace 'dev'".into(),
            candidates: Some(vec![
                "dev/backend/editor".into(),
                "dev/ops/prod-shell".into(),
            ]),
        },
    );
    let json: serde_json::Value = serde_json::to_value(&env).unwrap();

    assert_eq!(json["type"], "Error");
    assert_eq!(json["payload"]["code"], "target-not-found");
    assert_eq!(
        json["payload"]["message"],
        "No pane named 'editor' in workspace 'dev'"
    );
    assert_eq!(json["payload"]["candidates"][0], "dev/backend/editor");
    assert_eq!(json["payload"]["candidates"][1], "dev/ops/prod-shell");
}

#[test]
fn send_newline_default_is_true() {
    // When deserializing a Send payload without "newline", it should default to true.
    let json = r#"{"target":"dev/server","text":"hello"}"#;
    let send: Send = serde_json::from_str(json).unwrap();
    assert!(send.newline);
}

#[test]
fn handshake_camel_case_fields() {
    // Verify camelCase field names on the wire.
    let payload = Handshake {
        client_type: ClientType::Ui,
        client_version: "1.0.0".into(),
        protocol_version: 1,
    };
    let json: serde_json::Value = serde_json::to_value(&payload).unwrap();
    assert!(json.get("clientType").is_some());
    assert!(json.get("clientVersion").is_some());
    assert!(json.get("protocolVersion").is_some());
    // snake_case fields should NOT be present.
    assert!(json.get("client_type").is_none());
    assert!(json.get("client_version").is_none());
    assert!(json.get("protocol_version").is_none());
}
