//! Background output broadcaster (§13.9, §13.13).
//!
//! Periodically drains ConPTY output from each session's reader thread,
//! feeds it to the session's ScreenBuffer, and broadcasts `SessionOutput`,
//! `SessionStateChanged`, `TitleChanged`, and `WorkspaceStateChanged`
//! messages to all connected UI clients via [`IpcServer::broadcast_to_ui`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use wtd_ipc::message::{
    ProgressChanged, ProgressInfo, ProgressState, SessionOutput, SessionStateChanged,
    TitleChanged, WorkspaceStateChanged,
};
use wtd_ipc::Envelope;

use crate::ipc_server::IpcServer;
use crate::request_handler::HostRequestHandler;

/// Polling interval for draining session output (ms).
const POLL_INTERVAL_MS: u64 = 50;

/// An event to broadcast to UI clients.
pub enum BroadcastEvent {
    /// Raw VT bytes produced by a session.
    Output { session_id: String, data: Vec<u8> },
    /// Session state transition (e.g. running → exited).
    StateChanged {
        session_id: String,
        new_state: String,
        exit_code: Option<i32>,
    },
    /// Session window title changed via VT escape sequence.
    TitleChange { session_id: String, title: String },
    /// Session progress changed via OSC 9;4.
    ProgressChange {
        session_id: String,
        progress: Option<ProgressInfo>,
    },
    /// Workspace instance state transition.
    WorkspaceState {
        workspace: String,
        new_state: String,
    },
}

/// Run the output broadcaster loop.
///
/// Polls all sessions every ~50 ms, drains ConPTY output into each
/// session's ScreenBuffer, and broadcasts events to connected UI clients.
/// Exits when `shutdown_rx` signals.
#[cfg(windows)]
pub async fn run(
    handler: Arc<HostRequestHandler>,
    server: Arc<IpcServer>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut prev_titles: HashMap<String, String> = HashMap::new();
    let mut prev_progress: HashMap<String, Option<ProgressInfo>> = HashMap::new();
    let event_counter = AtomicU64::new(1);

    loop {
        tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {
                let events = handler.drain_session_events(&mut prev_titles, &mut prev_progress);
                for event in events {
                    let id = format!(
                        "evt-{}",
                        event_counter.fetch_add(1, Ordering::Relaxed)
                    );
                    let envelope = match &event {
                        BroadcastEvent::Output { session_id, data } => {
                            Envelope::new(
                                &id,
                                &SessionOutput {
                                    session_id: session_id.clone(),
                                    data: encode_base64(data),
                                },
                            )
                        }
                        BroadcastEvent::StateChanged {
                            session_id,
                            new_state,
                            exit_code,
                        } => Envelope::new(
                            &id,
                            &SessionStateChanged {
                                session_id: session_id.clone(),
                                new_state: new_state.clone(),
                                exit_code: *exit_code,
                            },
                        ),
                        BroadcastEvent::TitleChange {
                            session_id,
                            title,
                        } => Envelope::new(
                            &id,
                            &TitleChanged {
                                session_id: session_id.clone(),
                                title: title.clone(),
                            },
                        ),
                        BroadcastEvent::ProgressChange {
                            session_id,
                            progress,
                        } => Envelope::new(
                            &id,
                            &ProgressChanged {
                                session_id: session_id.clone(),
                                progress: progress.clone(),
                            },
                        ),
                        BroadcastEvent::WorkspaceState {
                            workspace,
                            new_state,
                        } => Envelope::new(
                            &id,
                            &WorkspaceStateChanged {
                                workspace: workspace.clone(),
                                new_state: new_state.clone(),
                            },
                        ),
                    };
                    let _ = server.broadcast_to_ui(&envelope).await;
                }
            }
        }
    }
}

pub(crate) fn progress_info_from_screen(
    progress: Option<wtd_pty::TerminalProgress>,
) -> Option<ProgressInfo> {
    progress.map(|progress| match progress {
        wtd_pty::TerminalProgress::Normal(value) => ProgressInfo {
            state: ProgressState::Normal,
            value: Some(value),
        },
        wtd_pty::TerminalProgress::Error(value) => ProgressInfo {
            state: ProgressState::Error,
            value: Some(value),
        },
        wtd_pty::TerminalProgress::Indeterminate => ProgressInfo {
            state: ProgressState::Indeterminate,
            value: None,
        },
        wtd_pty::TerminalProgress::Warning(value) => ProgressInfo {
            state: ProgressState::Warning,
            value: Some(value),
        },
    })
}

// ── Base64 encode ────────────────────────────────────────────────────────

const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode_base64(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        let data = b"Hello, World!";
        let encoded = encode_base64(data);
        assert_eq!(encoded, "SGVsbG8sIFdvcmxkIQ==");
    }

    #[test]
    fn base64_empty() {
        assert_eq!(encode_base64(b""), "");
    }

    #[test]
    fn base64_one_byte() {
        assert_eq!(encode_base64(b"A"), "QQ==");
    }

    #[test]
    fn base64_two_bytes() {
        assert_eq!(encode_base64(b"AB"), "QUI=");
    }

    #[test]
    fn base64_three_bytes() {
        assert_eq!(encode_base64(b"ABC"), "QUJD");
    }
}
