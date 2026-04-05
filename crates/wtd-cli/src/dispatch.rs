//! Command dispatch — maps CLI commands to IPC messages and handles responses.
//!
//! Each CLI command is translated to an IPC envelope, sent to the host,
//! and the response is formatted for output.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::cli::{Cli, Command, HostCommand, ListCommand};
use crate::client::{ClientError, IpcClient, DEFAULT_TIMEOUT};
use crate::exit_code;
use crate::input_bytes::{encode_input_payload, InputEncoding};
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

fn request_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .to_string_lossy()
        .to_string()
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
        Ok(Some(env)) => env,
        Ok(None) => {
            eprintln!("wtd: command not yet implemented");
            return exit_code::GENERAL_ERROR;
        }
        Err(e) => {
            eprintln!("wtd: {e}");
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

fn build_request(command: &Command) -> Result<Option<Envelope>, String> {
    let id = next_id();
    Ok(match command {
        Command::Open {
            name,
            file,
            recreate,
        } => Some(Envelope {
            id,
            msg_type: OpenWorkspace::TYPE_NAME.to_string(),
            payload: serde_json::json!({
                "name": name,
                "file": file.as_ref().map(|p| p.to_string_lossy().to_string()),
                "recreate": recreate,
                "cwd": request_cwd(),
            }),
        }),
        Command::Attach { name } => Some(Envelope::new(
            &id,
            &AttachWorkspace {
                workspace: name.clone(),
            },
        )),
        Command::Recreate { name } => Some(Envelope {
            id,
            msg_type: RecreateWorkspace::TYPE_NAME.to_string(),
            payload: serde_json::json!({
                "workspace": name,
                "cwd": request_cwd(),
            }),
        }),
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
            ListCommand::Workspaces => Some(Envelope {
                id,
                msg_type: ListWorkspaces::TYPE_NAME.to_string(),
                payload: serde_json::json!({
                    "cwd": request_cwd(),
                }),
            }),
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
        Command::Input {
            target,
            data,
            escape,
            hex,
            base64,
        } => {
            let encoding = if *escape {
                InputEncoding::Escaped
            } else if *hex {
                InputEncoding::Hex
            } else if *base64 {
                InputEncoding::Base64
            } else {
                InputEncoding::Utf8
            };
            let encoded = encode_input_payload(data, encoding).map_err(|e| e.to_string())?;
            Some(Envelope::new(
                &id,
                &message::PaneInput {
                    target: target.clone(),
                    data: encoded,
                },
            ))
        }
        Command::Capture {
            target,
            vt,
            lines,
            all,
            after,
            after_regex,
            max_lines,
            count,
        } => Some(Envelope::new(
            &id,
            &Capture {
                target: target.clone(),
                vt: if *vt { Some(true) } else { None },
                lines: *lines,
                all: if *all { Some(true) } else { None },
                after: after.clone(),
                after_regex: after_regex.clone(),
                max_lines: *max_lines,
                count: if *count { Some(true) } else { None },
            },
        )),
        Command::Snapshot { name, file } => Some(Envelope::new(
            &id,
            &SaveWorkspace {
                workspace: name.clone(),
                file: file.as_ref().map(|path| path.to_string_lossy().to_string()),
            },
        )),
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
    })
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
            #[cfg(windows)]
            {
                match stop_host_process(json_mode) {
                    Ok(code) => code,
                    Err(e) => {
                        eprintln!("wtd: {e}");
                        exit_code::GENERAL_ERROR
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let _ = json_mode;
                eprintln!("wtd: not supported on this platform");
                exit_code::GENERAL_ERROR
            }
        }
    }
}

#[cfg(windows)]
fn stop_host_process(json_mode: bool) -> Result<i32, String> {
    let pipe_name = connect::pipe_name_for_current_user().map_err(|e| e.to_string())?;
    let data_dir = host_data_dir();
    let pid_path = data_dir.join("host.pid");
    let running = connect::is_host_pipe_available(&pipe_name);

    let Some(pid) = read_host_pid(&pid_path) else {
        if json_mode {
            println!(
                "{}",
                serde_json::json!({ "running": false, "stopped": false })
            );
        } else if running {
            println!("Host is running, but host.pid is missing");
        } else {
            println!("Host is not running");
        }
        return Ok(if running {
            exit_code::GENERAL_ERROR
        } else {
            exit_code::SUCCESS
        });
    };

    if !is_process_running(pid) {
        let _ = std::fs::remove_file(&pid_path);
        if json_mode {
            println!(
                "{}",
                serde_json::json!({ "running": false, "stopped": false, "stalePidRemoved": true })
            );
        } else {
            println!("Removed stale host.pid");
        }
        return Ok(exit_code::SUCCESS);
    }

    terminate_process(pid).map_err(|e| format!("failed to stop host pid {}: {}", pid, e))?;

    for _ in 0..100 {
        if !is_process_running(pid) && !connect::is_host_pipe_available(&pipe_name) {
            let _ = std::fs::remove_file(&pid_path);
            if json_mode {
                println!(
                    "{}",
                    serde_json::json!({ "running": false, "stopped": true, "pid": pid })
                );
            } else {
                println!("Stopped host (pid {})", pid);
            }
            return Ok(exit_code::SUCCESS);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Err(format!("host pid {} did not shut down within timeout", pid))
}

#[cfg(windows)]
fn host_data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("WTD_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }

    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        format!(r"{}\AppData\Roaming", home)
    });

    std::path::PathBuf::from(appdata).join("WinTermDriver")
}

#[cfg(windows)]
fn read_host_pid(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let mut exit_code = 0u32;
                let running = if GetExitCodeProcess(handle, &mut exit_code).is_ok() {
                    exit_code == 259
                } else {
                    false
                };
                let _ = CloseHandle(handle);
                running
            }
            Err(_) => false,
        }
    }
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<(), windows::core::Error> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid)?;
        TerminateProcess(handle, 0)?;
        let _ = CloseHandle(handle);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Command;
    use std::path::PathBuf;

    #[test]
    fn open_request_includes_client_cwd() {
        let env = build_request(&Command::Open {
            name: "dev".to_string(),
            file: Some(PathBuf::from("dev.yaml")),
            recreate: false,
        })
        .unwrap()
        .unwrap();

        assert_eq!(env.msg_type, OpenWorkspace::TYPE_NAME);
        assert_eq!(env.payload["name"], "dev");
        assert_eq!(env.payload["file"], "dev.yaml");
        assert_eq!(env.payload["recreate"], false);
        assert!(env.payload["cwd"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn recreate_request_includes_client_cwd() {
        let env = build_request(&Command::Recreate {
            name: "dev".to_string(),
        })
        .unwrap()
        .unwrap();

        assert_eq!(env.msg_type, RecreateWorkspace::TYPE_NAME);
        assert_eq!(env.payload["workspace"], "dev");
        assert!(env.payload["cwd"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn list_workspaces_request_includes_client_cwd() {
        let env = build_request(&Command::List {
            what: ListCommand::Workspaces,
        })
        .unwrap()
        .unwrap();

        assert_eq!(env.msg_type, ListWorkspaces::TYPE_NAME);
        assert!(env.payload["cwd"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[test]
    fn input_request_encodes_escape_data() {
        let env = build_request(&Command::Input {
            target: "dev/server".to_string(),
            data: r"\e[<35;40;12M".to_string(),
            escape: true,
            hex: false,
            base64: false,
        })
        .unwrap()
        .unwrap();

        assert_eq!(env.msg_type, message::PaneInput::TYPE_NAME);
        assert_eq!(env.payload["target"], "dev/server");
        assert_eq!(env.payload["data"], "G1s8MzU7NDA7MTJN");
    }
}
