//! Output formatting for CLI responses (§22.5).
//!
//! Human-readable text by default, JSON with `--json`.

use crate::exit_code;
use wtd_ipc::message::*;
use wtd_ipc::Envelope;

/// Formatted output ready to print.
pub struct OutputResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Format a response envelope for display.
pub fn format_response(response: &Envelope, json_mode: bool) -> OutputResult {
    if json_mode {
        return format_json(response);
    }
    format_text(response)
}

// ── JSON formatting ──────────────────────────────────────────────────

fn format_json(response: &Envelope) -> OutputResult {
    let exit_code = if response.msg_type == ErrorResponse::TYPE_NAME {
        error_exit_code(response)
    } else {
        exit_code::SUCCESS
    };
    let stdout = serde_json::to_string_pretty(&response.payload).unwrap_or_default();
    OutputResult {
        stdout,
        stderr: String::new(),
        exit_code,
    }
}

// ── Text formatting ──────────────────────────────────────────────────

fn format_text(response: &Envelope) -> OutputResult {
    match response.msg_type.as_str() {
        "Ok" => OutputResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: exit_code::SUCCESS,
        },
        "Error" => format_error(response),
        "OpenWorkspaceResult" => format_open_workspace(response),
        "AttachWorkspaceResult" => format_ok_msg("Attached to workspace"),
        "RecreateWorkspaceResult" => format_ok_msg("Workspace recreated"),
        "ListWorkspacesResult" => format_list_workspaces(response),
        "ListInstancesResult" => format_list_instances(response),
        "ListPanesResult" => format_list_panes(response),
        "ListSessionsResult" => format_list_sessions(response),
        "CaptureResult" => format_capture(response),
        "ScrollbackResult" => format_scrollback(response),
        "WaitPaneResult" => format_wait_pane(response),
        "InspectResult" => format_inspect(response),
        "InvokeActionResult" => format_invoke_action(response),
        "FollowData" => format_follow_data(response),
        "FollowEnd" => format_follow_end(response),
        other => OutputResult {
            stdout: String::new(),
            stderr: format!("wtd: unexpected response type: {other}"),
            exit_code: exit_code::GENERAL_ERROR,
        },
    }
}

fn format_error(response: &Envelope) -> OutputResult {
    let err: ErrorResponse = match response.extract_payload() {
        Ok(e) => e,
        Err(_) => {
            return OutputResult {
                stdout: String::new(),
                stderr: "wtd: unknown error".to_string(),
                exit_code: exit_code::GENERAL_ERROR,
            }
        }
    };

    let mut stderr = format!("wtd: {}", err.message);
    if let Some(candidates) = &err.candidates {
        stderr.push_str("\nCandidates:");
        for c in candidates {
            stderr.push_str(&format!("\n  {c}"));
        }
    }
    stderr.push('\n');

    OutputResult {
        stdout: String::new(),
        stderr,
        exit_code: error_code_to_exit_code(&err.code),
    }
}

fn format_ok_msg(msg: &str) -> OutputResult {
    OutputResult {
        stdout: format!("{msg}\n"),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_open_workspace(response: &Envelope) -> OutputResult {
    let result: OpenWorkspaceResult = match response.extract_payload() {
        Ok(r) => r,
        Err(_) => return format_ok_msg("Workspace opened"),
    };
    OutputResult {
        stdout: format!("Opened workspace (instance {})\n", result.instance_id),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_list_workspaces(response: &Envelope) -> OutputResult {
    let result: ListWorkspacesResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let rows: Vec<Vec<String>> = result
        .workspaces
        .iter()
        .map(|w| vec![w.name.clone(), w.source.clone()])
        .collect();
    OutputResult {
        stdout: format_table(&["NAME", "SOURCE"], &rows),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_list_instances(response: &Envelope) -> OutputResult {
    let result: ListInstancesResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let rows: Vec<Vec<String>> = result
        .instances
        .iter()
        .map(|i| vec![i.name.clone(), i.instance_id.clone()])
        .collect();
    OutputResult {
        stdout: format_table(&["NAME", "INSTANCE ID"], &rows),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_list_panes(response: &Envelope) -> OutputResult {
    let result: ListPanesResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let rows: Vec<Vec<String>> = result
        .panes
        .iter()
        .map(|p| vec![p.tab.clone(), p.name.clone(), p.session_state.clone()])
        .collect();
    OutputResult {
        stdout: format_table(&["TAB", "PANE", "STATE"], &rows),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_list_sessions(response: &Envelope) -> OutputResult {
    let result: ListSessionsResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let rows: Vec<Vec<String>> = result
        .sessions
        .iter()
        .map(|s| vec![s.session_id.clone(), s.pane.clone(), s.state.clone()])
        .collect();
    OutputResult {
        stdout: format_table(&["SESSION ID", "PANE", "STATE"], &rows),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_capture(response: &Envelope) -> OutputResult {
    let result: CaptureResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    // Count mode: print the line count as a number
    let stdout = if result.text.is_empty() && result.lines > 0 {
        format!("{}\n", result.lines)
    } else {
        result.text
    };
    OutputResult {
        stdout,
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_scrollback(response: &Envelope) -> OutputResult {
    let result: ScrollbackResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let mut stdout = result.lines.join("\n");
    if !stdout.is_empty() {
        stdout.push('\n');
    }
    OutputResult {
        stdout,
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_wait_pane(response: &Envelope) -> OutputResult {
    let result: WaitPaneResult = match response.extract_payload() {
        Ok(r) => r,
        Err(e) => return parse_error(e),
    };
    let condition = wait_condition_label(result.condition);
    let metadata = &result.data["metadata"];
    let attention = result.data["attention"]["state"]
        .as_str()
        .unwrap_or("active");
    let phase = metadata["phase"].as_str().unwrap_or("");
    let status = metadata["statusText"].as_str().unwrap_or("");
    let mut stdout = if result.matched {
        format!("matched {condition}\n")
    } else {
        format!("timeout waiting for {condition}\n")
    };
    stdout.push_str(&format!("attention: {attention}\n"));
    if !phase.is_empty() {
        stdout.push_str(&format!("phase: {phase}\n"));
    }
    if !status.is_empty() {
        stdout.push_str(&format!("status: {status}\n"));
    }
    if let Some(lines) = result.data["recentOutput"].as_array() {
        if !lines.is_empty() {
            stdout.push_str("recent output:\n");
            for line in lines {
                if let Some(line) = line.as_str() {
                    stdout.push_str(line);
                    stdout.push('\n');
                }
            }
        }
    }
    OutputResult {
        stdout,
        stderr: String::new(),
        exit_code: if result.matched {
            exit_code::SUCCESS
        } else {
            exit_code::TIMEOUT
        },
    }
}

fn wait_condition_label(condition: WaitCondition) -> &'static str {
    match condition {
        WaitCondition::Idle => "idle",
        WaitCondition::Done => "done",
        WaitCondition::NeedsAttention => "needs-attention",
        WaitCondition::Error => "error",
        WaitCondition::QueueEmpty => "queue-empty",
        WaitCondition::StateChange => "state-change",
    }
}

fn format_inspect(response: &Envelope) -> OutputResult {
    let stdout = serde_json::to_string_pretty(&response.payload).unwrap_or_default();
    OutputResult {
        stdout: format!("{stdout}\n"),
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_invoke_action(response: &Envelope) -> OutputResult {
    let result: InvokeActionResult = match response.extract_payload() {
        Ok(r) => r,
        Err(_) => return format_ok_msg("Action completed"),
    };
    let stdout = match result.pane_id {
        Some(pane_id) => format!("{} (pane {})\n", result.result, pane_id),
        None => String::new(),
    };
    OutputResult {
        stdout,
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_follow_data(response: &Envelope) -> OutputResult {
    let data: FollowData = match response.extract_payload() {
        Ok(d) => d,
        Err(_) => {
            return OutputResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: exit_code::SUCCESS,
            }
        }
    };
    OutputResult {
        stdout: data.text,
        stderr: String::new(),
        exit_code: exit_code::SUCCESS,
    }
}

fn format_follow_end(response: &Envelope) -> OutputResult {
    let end: FollowEnd = match response.extract_payload() {
        Ok(e) => e,
        Err(_) => {
            return OutputResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: exit_code::SUCCESS,
            }
        }
    };
    OutputResult {
        stdout: String::new(),
        stderr: format!("Follow ended: {}\n", end.reason),
        exit_code: exit_code::SUCCESS,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn error_exit_code(response: &Envelope) -> i32 {
    let err: ErrorResponse = match response.extract_payload() {
        Ok(e) => e,
        Err(_) => return exit_code::GENERAL_ERROR,
    };
    error_code_to_exit_code(&err.code)
}

/// Map an IPC error code to a CLI exit code.
pub fn error_code_to_exit_code(code: &ErrorCode) -> i32 {
    match code {
        ErrorCode::TargetNotFound | ErrorCode::WorkspaceNotFound => exit_code::TARGET_NOT_FOUND,
        ErrorCode::TargetAmbiguous => exit_code::AMBIGUOUS_TARGET,
        ErrorCode::DefinitionError => exit_code::DEFINITION_ERROR,
        _ => exit_code::GENERAL_ERROR,
    }
}

fn parse_error(e: serde_json::Error) -> OutputResult {
    OutputResult {
        stdout: String::new(),
        stderr: format!("wtd: failed to parse response: {e}\n"),
        exit_code: exit_code::GENERAL_ERROR,
    }
}

fn format_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let ncols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let mut out = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            out.push_str("  ");
        }
        out.push_str(&format!("{:width$}", h, width = widths[i]));
    }
    out.push('\n');
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(&format!("{:width$}", cell, width = widths[i]));
        }
        out.push('\n');
    }
    out
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_envelope(msg_type: &str, payload: serde_json::Value) -> Envelope {
        Envelope {
            id: "test-1".to_string(),
            msg_type: msg_type.to_string(),
            payload,
        }
    }

    #[test]
    fn ok_response_success() {
        let env = make_envelope("Ok", json!({}));
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn error_target_not_found_exit_code() {
        let env = make_envelope(
            "Error",
            json!({
                "code": "target-not-found",
                "message": "pane 'foo' not found"
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::TARGET_NOT_FOUND);
        assert!(result.stderr.contains("pane 'foo' not found"));
    }

    #[test]
    fn error_ambiguous_exit_code() {
        let env = make_envelope(
            "Error",
            json!({
                "code": "target-ambiguous",
                "message": "ambiguous target",
                "candidates": ["dev/tab1/server", "dev/tab2/server"]
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::AMBIGUOUS_TARGET);
        assert!(result.stderr.contains("Candidates:"));
        assert!(result.stderr.contains("dev/tab1/server"));
    }

    #[test]
    fn error_workspace_not_found_exit_code() {
        let env = make_envelope(
            "Error",
            json!({
                "code": "workspace-not-found",
                "message": "workspace 'foo' not found"
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::TARGET_NOT_FOUND);
    }

    #[test]
    fn list_workspaces_text_format() {
        let env = make_envelope(
            "ListWorkspacesResult",
            json!({
                "workspaces": [
                    { "name": "dev", "source": "user" },
                    { "name": "ops", "source": "local" }
                ]
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("NAME"));
        assert!(result.stdout.contains("SOURCE"));
        assert!(result.stdout.contains("dev"));
        assert!(result.stdout.contains("user"));
        assert!(result.stdout.contains("ops"));
        assert!(result.stdout.contains("local"));
    }

    #[test]
    fn list_workspaces_json_format() {
        let env = make_envelope(
            "ListWorkspacesResult",
            json!({
                "workspaces": [
                    { "name": "dev", "source": "user" }
                ]
            }),
        );
        let result = format_response(&env, true);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(parsed["workspaces"][0]["name"], "dev");
    }

    #[test]
    fn list_panes_text_format() {
        let env = make_envelope(
            "ListPanesResult",
            json!({
                "panes": [
                    { "tab": "backend", "name": "editor", "sessionState": "running" },
                    { "tab": "backend", "name": "server", "sessionState": "running" }
                ]
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("TAB"));
        assert!(result.stdout.contains("PANE"));
        assert!(result.stdout.contains("STATE"));
        assert!(result.stdout.contains("editor"));
        assert!(result.stdout.contains("server"));
    }

    #[test]
    fn capture_text_format() {
        let env = make_envelope("CaptureResult", json!({ "text": "hello world\n" }));
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert_eq!(result.stdout, "hello world\n");
    }

    #[test]
    fn open_workspace_text_format() {
        let env = make_envelope(
            "OpenWorkspaceResult",
            json!({
                "instanceId": "42",
                "state": {}
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("42"));
    }

    #[test]
    fn error_json_returns_error_exit_code() {
        let env = make_envelope(
            "Error",
            json!({
                "code": "target-not-found",
                "message": "not found"
            }),
        );
        let result = format_response(&env, true);
        assert_eq!(result.exit_code, exit_code::TARGET_NOT_FOUND);
        let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(parsed["code"], "target-not-found");
    }

    #[test]
    fn table_formatting() {
        let headers = &["A", "BB", "CCC"];
        let rows = vec![
            vec!["x".into(), "yy".into(), "zzz".into()],
            vec!["longer".into(), "y".into(), "z".into()],
        ];
        let table = format_table(headers, &rows);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        // All columns should be aligned
        assert!(lines[0].contains("A"));
        assert!(lines[0].contains("BB"));
        assert!(lines[0].contains("CCC"));
    }

    #[test]
    fn list_instances_text_format() {
        let env = make_envelope(
            "ListInstancesResult",
            json!({
                "instances": [
                    { "name": "dev", "instanceId": "100" }
                ]
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("INSTANCE ID"));
        assert!(result.stdout.contains("100"));
    }

    #[test]
    fn list_sessions_text_format() {
        let env = make_envelope(
            "ListSessionsResult",
            json!({
                "sessions": [
                    { "sessionId": "1", "pane": "editor", "state": "running" }
                ]
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("SESSION ID"));
        assert!(result.stdout.contains("editor"));
    }

    #[test]
    fn scrollback_text_format() {
        let env = make_envelope(
            "ScrollbackResult",
            json!({ "lines": ["line1", "line2", "line3"] }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("line1\nline2\nline3"));
    }

    #[test]
    fn wait_pane_success_text_format() {
        let env = make_envelope(
            "WaitPaneResult",
            json!({
                "matched": true,
                "condition": "done",
                "target": "dev/tests",
                "data": {
                    "attention": { "state": "done" },
                    "metadata": {
                        "phase": "done",
                        "statusText": "tests passed",
                        "queuePending": 0
                    },
                    "recentOutput": ["ok"]
                }
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::SUCCESS);
        assert!(result.stdout.contains("matched done"));
        assert!(result.stdout.contains("attention: done"));
        assert!(result.stdout.contains("phase: done"));
        assert!(result.stdout.contains("status: tests passed"));
        assert!(result.stdout.contains("recent output:\nok\n"));
    }

    #[test]
    fn wait_pane_timeout_text_format() {
        let env = make_envelope(
            "WaitPaneResult",
            json!({
                "matched": false,
                "condition": "needs-attention",
                "target": "dev/tests",
                "data": {
                    "attention": { "state": "active" },
                    "metadata": { "phase": "working" },
                    "recentOutput": ["still running"]
                }
            }),
        );
        let result = format_response(&env, false);
        assert_eq!(result.exit_code, exit_code::TIMEOUT);
        assert!(result
            .stdout
            .contains("timeout waiting for needs-attention"));
        assert!(result.stdout.contains("recent output:\nstill running\n"));
    }
}
