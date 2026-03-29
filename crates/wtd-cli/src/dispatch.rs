//! Command dispatch — maps CLI commands to IPC messages and handles responses.
//!
//! Each CLI command is translated to an IPC envelope, sent to the host,
//! and the response is formatted for output.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::cli::{Cli, Command, HostCommand, ListCommand};
use crate::client::{ClientError, IpcClient, DEFAULT_TIMEOUT};
use crate::exit_code;
use crate::output::{self, OutputResult};
use wtd_ipc::connect;
use wtd_ipc::message::{
    self, AttachWorkspace, CancelFollow, Capture, CloseWorkspace, ErrorResponse, FocusPane, Follow,
    FollowEnd, Inspect, InvokeAction, ListInstances, ListPanes, ListSessions, ListWorkspaces,
    MessagePayload, OpenWorkspace, RecreateWorkspace, RenamePane, SaveWorkspace, Scrollback,
};
use wtd_ipc::Envelope;

static MSG_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    format!("cli-{}", MSG_COUNTER.fetch_add(1, Ordering::SeqCst))
}

fn resolve_timeout(cli_timeout: Option<f64>) -> Duration {
    match cli_timeout {
        Some(secs) if secs > 0.0 => Duration::from_secs_f64(secs),
        _ => DEFAULT_TIMEOUT,
    }
}

/// Run the CLI command: connect to host, send request, format response.
pub async fn run(cli: Cli) -> i32 {
    let timeout = resolve_timeout(cli.timeout);

    match &cli.command {
        Command::Completions { .. } => unreachable!(),
        Command::Host { action } => return run_host_command(action, cli.json),
        Command::Follow { target, raw } => {
            return run_follow(target, *raw, cli.json, timeout).await;
        }
        _ => {}
    }

    let mut client = match IpcClient::connect_and_handshake().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wtd: {e}");
            return client_error_exit_code(&e);
        }
    };
    client.set_timeout(timeout);

    let envelope = match build_request(&cli.command) {
        Some(env) => env,
        None => {
            eprintln!("wtd: command not yet implemented");
            return exit_code::GENERAL_ERROR;
        }
    };

    let response = match client.request(&envelope).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wtd: {e}");
            return client_error_exit_code(&e);
        }
    };

    let result = output::format_response(&response, cli.json);
    print_result(&result);
    result.exit_code
}

// ── Request building ─────────────────────────────────────────────────

fn build_request(command: &Command) -> Option<Envelope> {
    let id = next_id();
    match command {
        Command::Open {
            name,
            file,
            recreate,
        } => Some(Envelope::new(
            &id,
            &OpenWorkspace {
                name: name.clone(),
                file: file.as_ref().map(|p| p.to_string_lossy().to_string()),
                recreate: *recreate,
            },
        )),
        Command::Attach { name } => Some(Envelope::new(
            &id,
            &AttachWorkspace {
                workspace: name.clone(),
            },
        )),
        Command::Recreate { name } => Some(Envelope::new(
            &id,
            &RecreateWorkspace {
                workspace: name.clone(),
            },
        )),
        Command::Close { name, kill } => Some(Envelope::new(
            &id,
            &CloseWorkspace {
                workspace: name.clone(),
                kill: *kill,
            },
        )),
        Command::Save { name, file } => Some(Envelope::new(
            &id,
            &SaveWorkspace {
                workspace: name.clone(),
                file: file.as_ref().map(|p| p.to_string_lossy().to_string()),
            },
        )),
        Command::List { what } => match what {
            ListCommand::Workspaces => Some(Envelope::new(&id, &ListWorkspaces {})),
            ListCommand::Instances => Some(Envelope::new(&id, &ListInstances {})),
            ListCommand::Panes { workspace } => Some(Envelope::new(
                &id,
                &ListPanes {
                    workspace: workspace.clone(),
                },
            )),
            ListCommand::Sessions { workspace } => Some(Envelope::new(
                &id,
                &ListSessions {
                    workspace: workspace.clone(),
                },
            )),
        },
        Command::Send {
            target,
            text,
            no_newline,
        } => Some(Envelope::new(
            &id,
            &message::Send {
                target: target.clone(),
                text: text.clone(),
                newline: !no_newline,
            },
        )),
        Command::Keys { target, key_specs } => Some(Envelope::new(
            &id,
            &message::Keys {
                target: target.clone(),
                keys: key_specs.clone(),
            },
        )),
        Command::Capture { target, lines, all, after, after_regex, max_lines, count } => {
            Some(Envelope::new(
                &id,
                &Capture {
                    target: target.clone(),
                    lines: *lines,
                    all: if *all { Some(true) } else { None },
                    after: after.clone(),
                    after_regex: after_regex.clone(),
                    max_lines: *max_lines,
                    count: if *count { Some(true) } else { None },
                },
            ))
        }
        Command::Scrollback { target, tail } => Some(Envelope::new(
            &id,
            &Scrollback {
                target: target.clone(),
                tail: *tail,
            },
        )),
        Command::Inspect { target } => Some(Envelope::new(
            &id,
            &Inspect {
                target: target.clone(),
            },
        )),
        Command::Focus { target } => Some(Envelope::new(
            &id,
            &FocusPane {
                pane_id: target.clone(),
            },
        )),
        Command::Rename { target, new_name } => Some(Envelope::new(
            &id,
            &RenamePane {
                pane_id: target.clone(),
                new_name: new_name.clone(),
            },
        )),
        Command::Action {
            target,
            action_name,
            args,
        } => {
            let args_value = if args.is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                let mut map = serde_json::Map::new();
                for arg in args {
                    if let Some((k, v)) = arg.split_once('=') {
                        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                    }
                }
                serde_json::Value::Object(map)
            };
            Some(Envelope::new(
                &id,
                &InvokeAction {
                    action: action_name.clone(),
                    target_pane_id: Some(target.clone()),
                    args: args_value,
                },
            ))
        }
        Command::Follow { .. } | Command::Host { .. } | Command::Completions { .. } => None,
    }
}

// ── Follow (streaming) ──────────────────────────────────────────────

async fn run_follow(target: &str, raw: bool, json_mode: bool, timeout: Duration) -> i32 {
    let mut client = match IpcClient::connect_and_handshake().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wtd: {e}");
            return client_error_exit_code(&e);
        }
    };
    client.set_timeout(timeout);

    let follow_id = next_id();
    let follow_req = Envelope::new(
        &follow_id,
        &Follow {
            target: target.to_string(),
            raw,
        },
    );
    if let Err(e) = client.write_frame(&follow_req).await {
        eprintln!("wtd: {e}");
        return exit_code::CONNECTION_ERROR;
    }

    // Read initial response (Ok or Error).
    let initial = match client.read_frame().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wtd: {e}");
            return exit_code::CONNECTION_ERROR;
        }
    };

    if initial.msg_type == ErrorResponse::TYPE_NAME {
        let result = output::format_response(&initial, json_mode);
        print_result(&result);
        return result.exit_code;
    }

    // Stream FollowData until FollowEnd, error, or Ctrl+C.
    loop {
        tokio::select! {
            frame = client.read_frame() => {
                match frame {
                    Ok(env) => {
                        let result = output::format_response(&env, json_mode);
                        if !result.stdout.is_empty() {
                            print!("{}", result.stdout);
                        }
                        if env.msg_type == FollowEnd::TYPE_NAME {
                            if !result.stderr.is_empty() {
                                eprint!("{}", result.stderr);
                            }
                            return exit_code::SUCCESS;
                        }
                    }
                    Err(e) => {
                        eprintln!("wtd: connection lost: {e}");
                        return exit_code::CONNECTION_ERROR;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                let cancel = Envelope::new(
                    &next_id(),
                    &CancelFollow { id: follow_id.clone() },
                );
                let _ = client.write_frame(&cancel).await;
                return exit_code::SUCCESS;
            }
        }
    }
}

// ── Host commands (local, no IPC) ────────────────────────────────────

fn run_host_command(action: &HostCommand, json_mode: bool) -> i32 {
    match action {
        HostCommand::Status => {
            #[cfg(windows)]
            {
                let pipe_name = match connect::pipe_name_for_current_user() {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("wtd: {e}");
                        return exit_code::GENERAL_ERROR;
                    }
                };
                let running = connect::is_host_pipe_available(&pipe_name);
                if json_mode {
                    println!("{}", serde_json::json!({ "running": running }));
                } else if running {
                    println!("Host is running");
                } else {
                    println!("Host is not running");
                }
                exit_code::SUCCESS
            }
            #[cfg(not(windows))]
            {
                let _ = json_mode;
                eprintln!("wtd: not supported on this platform");
                exit_code::GENERAL_ERROR
            }
        }
        HostCommand::Stop => {
            eprintln!("wtd: host stop not yet implemented");
            exit_code::GENERAL_ERROR
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn print_result(result: &OutputResult) {
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }
}

fn client_error_exit_code(e: &ClientError) -> i32 {
    match e {
        ClientError::Connect(ce) => match ce {
            connect::ConnectError::HostNotFound | connect::ConnectError::StartupTimeout => {
                exit_code::HOST_START_FAILED
            }
            _ => exit_code::CONNECTION_ERROR,
        },
        ClientError::Ipc(_) | ClientError::Handshake(_) => exit_code::CONNECTION_ERROR,
        ClientError::RequestTimeout(_) => exit_code::TIMEOUT,
    }
}
