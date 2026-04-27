//! Clap-based CLI parser for `wtd` — all commands, subcommands, and global flags.
//!
//! Spec references: §22.1–22.4

use std::io;
use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// WinTermDriver controller CLI.
///
/// Sends commands to the wtd-host background process.
#[derive(Debug, Parser)]
#[command(
    name = "wtd",
    version,
    about,
    after_long_help = "Coding-agent quick path:\n  1. `wtd prompt <target> \"<text>\"` to write\n  2. `wtd capture <target>` to read what is on screen now\n  3. `wtd configure-pane <target> ...` only when you need to override the inferred driver\n\nAgent panes launched as `codex`, `claude`, `gemini`, or `copilot` are auto-detected. Use `wtd send` only for low-level shell/text input and `wtd keys` when you need an explicit keypress."
)]
pub struct Cli {
    /// Output in JSON format instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Include internal IDs and additional metadata in output.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Address a target by internal ID instead of semantic path.
    #[arg(long, global = true)]
    pub id: Option<String>,

    /// Request timeout in seconds (default: 30).
    #[arg(long, global = true)]
    pub timeout: Option<f64>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    // ── Workspace commands ──────────────────────────────────────────
    /// Open workspace from definition, or launch a default shell.
    Open {
        /// Workspace name (omit to open default shell).
        name: Option<String>,
        /// Path to a workspace definition file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Tear down existing instance and recreate from definition.
        #[arg(long)]
        recreate: bool,
        /// Open an ad-hoc workspace using a named profile (no YAML file needed).
        #[arg(long, conflicts_with = "file")]
        profile: Option<String>,
    },

    /// Start or reuse a workspace and launch the graphical UI attached to it.
    Start {
        /// Workspace name.
        name: String,
        /// Path to a workspace definition file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Tear down existing instance and recreate from definition.
        #[arg(long)]
        recreate: bool,
        /// Open an ad-hoc workspace using a named profile (no YAML file needed).
        #[arg(long, conflicts_with = "file")]
        profile: Option<String>,
    },

    /// Attach to an existing workspace instance.
    Attach {
        /// Workspace name.
        name: String,
    },

    /// Tear down existing instance and recreate from definition.
    Recreate {
        /// Workspace name.
        name: String,
    },

    /// Close workspace UI.
    Close {
        /// Workspace name.
        name: String,
        /// Also destroy the instance.
        #[arg(long)]
        kill: bool,
    },

    /// Save workspace definition.
    Save {
        /// Workspace name.
        name: String,
        /// Output file path.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    // ── List commands ───────────────────────────────────────────────
    /// List workspaces, instances, panes, or sessions.
    List {
        #[command(subcommand)]
        what: ListCommand,
    },

    /// Discover, inspect, or run project-local WTD recipes.
    Recipe {
        #[command(subcommand)]
        action: RecipeCommand,
    },

    // ── Pane / session commands ─────────────────────────────────────
    /// Focus a pane in the UI.
    Focus {
        /// Target path (e.g. workspace/tab/pane).
        target: String,
    },

    /// Rename a pane.
    Rename {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// New name for the pane.
        new_name: String,
    },

    /// Configure pane-local prompt driving behavior.
    ///
    /// WTD auto-detects common agent panes. Use this when you want to
    /// override or pin the driver behavior for a pane.
    ConfigurePane {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Built-in driver profile to apply (`codex`, `claude-code`, `gemini-cli`, `copilot-cli`, `plain`).
        #[arg(long, value_enum)]
        driver_profile: Option<DriverProfileArg>,
        /// Key spec used to submit the prompt.
        #[arg(long)]
        submit_key: Option<String>,
        /// Key spec used for soft line breaks in multiline prompts.
        #[arg(long)]
        soft_break_key: Option<String>,
        /// Disable soft line breaks for this pane.
        #[arg(long)]
        clear_soft_break: bool,
        /// Remove all pane-local driver config and fall back to the plain profile.
        #[arg(long)]
        clear_driver: bool,
    },

    /// Invoke a named action on a target.
    Action {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Action name (kebab-case).
        action_name: String,
        /// Action arguments as key=value pairs.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    /// Publish pane attention/status for hosted agents.
    Notify {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Attention state to publish.
        #[arg(long, value_enum, default_value_t = AttentionStateArg::NeedsAttention)]
        state: AttentionStateArg,
        /// Source identifier for the publishing agent/tool.
        #[arg(long)]
        source: Option<String>,
        /// Optional status or completion message.
        message: Option<String>,
    },

    /// Clear pane attention/status.
    ClearAttention {
        /// Target path (e.g. workspace/pane).
        target: String,
    },

    /// Publish structured pane metadata/status for hosted agents.
    Status {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Waitable phase, such as idle, working, done, error, or needs-attention.
        #[arg(long)]
        phase: Option<String>,
        /// Source identifier for the publishing agent/tool.
        #[arg(long)]
        source: Option<String>,
        /// Pending queue item count.
        #[arg(long)]
        queue_pending: Option<u32>,
        /// Completion marker, such as success or failure.
        #[arg(long)]
        completion: Option<String>,
        /// Human-readable status text.
        status_text: Option<String>,
    },

    /// Wait until a pane reaches an attention/status condition.
    Wait {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Condition to wait for.
        #[arg(long = "for", value_enum, default_value_t = WaitConditionArg::Done)]
        condition: WaitConditionArg,
        /// Host-side wait timeout in seconds.
        #[arg(long, default_value_t = 30.0)]
        timeout: f64,
        /// Poll interval in milliseconds.
        #[arg(long, default_value_t = 250)]
        poll_ms: u64,
        /// Recent scrollback lines to include in success/timeout snapshots.
        #[arg(long, default_value_t = 40)]
        recent_lines: u32,
    },

    // ── Input commands ──────────────────────────────────────────────
    /// Send low-level text to a session.
    ///
    /// This is the shell/text primitive. For coding-agent panes, prefer
    /// `wtd prompt`.
    Send {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Text to send.
        text: String,
        /// Do not append newline.
        #[arg(long)]
        no_newline: bool,
    },

    /// Send a prompt using the pane's configured driver profile.
    ///
    /// Recommended agent flow:
    /// 1. `wtd prompt <target> "<text>"` to write
    /// 2. `wtd capture <target>` to read what the agent is showing now
    /// 3. `wtd configure-pane <target> ...` only if you need to override the inferred driver
    Prompt {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Prompt text to send. Embedded newlines are handled using the pane driver.
        text: String,
    },

    /// Send key sequences to a session.
    Keys {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Key specifications (e.g. Enter, Ctrl+C, F1).
        #[arg(required = true)]
        key_specs: Vec<String>,
    },

    /// Inject semantic mouse input into a session.
    Mouse {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Mouse event kind.
        #[arg(value_enum)]
        kind: MouseKindArg,
        /// 0-based cell column.
        #[arg(long)]
        col: u16,
        /// 0-based cell row.
        #[arg(long)]
        row: u16,
        /// Mouse button for press/release/click/move.
        #[arg(long, value_enum)]
        button: Option<MouseButtonArg>,
        /// Repeat count (useful for wheel).
        #[arg(long, default_value_t = 1)]
        repeat: u16,
        /// Include Shift modifier.
        #[arg(long)]
        shift: bool,
        /// Include Alt modifier.
        #[arg(long)]
        alt: bool,
        /// Include Ctrl modifier.
        #[arg(long)]
        ctrl: bool,
        /// Inject even when the pane is not advertising VT mouse mode.
        #[arg(long)]
        force: bool,
    },

    /// Send raw input bytes to a session.
    Input {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Input data. Use --escape for C-style escapes, --hex for hex bytes, or --base64.
        data: String,
        /// Interpret data using C-style escapes such as \e, \r, \n, and \x1b.
        #[arg(long, conflicts_with_all = ["hex", "base64"])]
        escape: bool,
        /// Interpret data as hexadecimal bytes.
        #[arg(long, conflicts_with_all = ["escape", "base64"])]
        hex: bool,
        /// Interpret data as base64-encoded bytes.
        #[arg(long, conflicts_with_all = ["escape", "hex"])]
        base64: bool,
    },

    // ── Inspection commands ─────────────────────────────────────────
    /// Capture the visible screen content as text.
    Capture {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Return a replayable VT snapshot of the visible screen state.
        #[arg(long, conflicts_with_all = ["lines", "all", "after", "after_regex", "max_lines", "count"])]
        vt: bool,
        /// Return last N lines (scrollback + visible, counted from bottom).
        #[arg(long)]
        lines: Option<u32>,
        /// Return entire buffer (all scrollback + visible).
        #[arg(long)]
        all: bool,
        /// Exact substring anchor — capture from match line to end.
        #[arg(long, value_name = "STRING")]
        after: Option<String>,
        /// Regex anchor — capture from first match line to end.
        #[arg(long, value_name = "PATTERN")]
        after_regex: Option<String>,
        /// Cap total lines returned.
        #[arg(long)]
        max_lines: Option<u32>,
        /// Return metadata only (line count), no text.
        #[arg(long)]
        count: bool,
    },

    /// Capture scrollback lines.
    Scrollback {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Number of lines from the end.
        #[arg(long)]
        tail: u32,
    },

    /// Stream output from a session until Ctrl+C or session exit.
    Follow {
        /// Target path (e.g. workspace/pane).
        target: String,
        /// Output raw bytes without processing.
        #[arg(long)]
        raw: bool,
    },

    /// Show full metadata for a pane/session.
    Inspect {
        /// Target path (e.g. workspace/pane).
        target: String,
    },

    /// Export a workspace attach snapshot to JSON.
    Snapshot {
        /// Workspace name.
        name: String,
        /// Output file path. Prints to stdout when omitted.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    // ── Host management commands ────────────────────────────────────
    /// Manage the host process.
    Host {
        #[command(subcommand)]
        action: HostCommand,
    },

    // ── Shell completions ───────────────────────────────────────────
    /// Generate shell completion scripts.
    #[command(hide = true)]
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

/// Subcommands for `wtd list`.
#[derive(Debug, Subcommand)]
pub enum ListCommand {
    /// List all available workspace definitions.
    Workspaces,

    /// List all running workspace instances.
    Instances,

    /// List all panes in a workspace instance.
    Panes {
        /// Workspace name.
        workspace: String,
    },

    /// List all sessions in a workspace instance.
    Sessions {
        /// Workspace name.
        workspace: String,
    },
}

/// Subcommands for `wtd recipe`.
#[derive(Debug, Subcommand)]
pub enum RecipeCommand {
    /// List commands from a recipe manifest.
    List {
        /// Path to a recipe manifest. Defaults to .wtd/recipes.yaml or wtd-recipes.yaml found from cwd.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    /// Show one command from a recipe manifest.
    Show {
        /// Recipe command name.
        name: String,
        /// Path to a recipe manifest.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    /// Run one command from a recipe manifest.
    Run {
        /// Recipe command name.
        name: String,
        /// Path to a recipe manifest.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Template variable override as key=value. May be repeated.
        #[arg(long = "var")]
        vars: Vec<String>,
        /// Print the WTD operations without sending them to the host.
        #[arg(long)]
        dry_run: bool,
        /// Confirm execution from a changed checked-in workflow file.
        #[arg(long)]
        allow_changed_workflow: bool,
    },
}

/// Subcommands for `wtd host`.
#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Show host process status (PID, uptime, instance count).
    Status,

    /// Shut down the host process.
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MouseKindArg {
    Press,
    Release,
    Click,
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MouseButtonArg {
    Left,
    Middle,
    Right,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DriverProfileArg {
    Plain,
    Codex,
    ClaudeCode,
    GeminiCli,
    CopilotCli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AttentionStateArg {
    Active,
    NeedsAttention,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WaitConditionArg {
    Idle,
    Done,
    NeedsAttention,
    Error,
    QueueEmpty,
    StateChange,
}

/// Generate shell completions and write them to stdout.
pub fn print_completions(shell: Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "wtd", &mut io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::Parser;

    /// Helper: parse a command line, returning the parsed Cli or the error string.
    fn parse(args: &[&str]) -> Result<Cli, String> {
        let mut full = vec!["wtd"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map_err(|e| e.to_string())
    }

    // ── Workspace commands ──────────────────────────────────────

    #[test]
    fn open_basic() {
        let cli = parse(&["open", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Open { ref name, ref file, recreate, ref profile })
            if name.as_deref() == Some("dev") && file.is_none() && !recreate && profile.is_none()
        ));
    }

    #[test]
    fn open_with_file_and_recreate() {
        let cli = parse(&["open", "dev", "--file", "dev.yaml", "--recreate"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Open { ref name, ref file, recreate, .. })
            if name.as_deref() == Some("dev") && file.as_deref() == Some(std::path::Path::new("dev.yaml")) && recreate
        ));
    }

    #[test]
    fn open_no_args_is_valid() {
        let cli = parse(&["open"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Open { ref name, ref file, recreate, ref profile })
            if name.is_none() && file.is_none() && !recreate && profile.is_none()
        ));
    }

    #[test]
    fn open_with_profile() {
        let cli = parse(&["open", "--profile", "ssh-prod"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Open { ref name, ref profile, .. })
            if name.is_none() && profile.as_deref() == Some("ssh-prod")
        ));
    }

    #[test]
    fn open_with_name_and_profile() {
        let cli = parse(&["open", "myws", "--profile", "ssh-prod"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Open { ref name, ref profile, .. })
            if name.as_deref() == Some("myws") && profile.as_deref() == Some("ssh-prod")
        ));
    }

    #[test]
    fn open_profile_conflicts_with_file() {
        assert!(parse(&["open", "--profile", "x", "--file", "y.yaml"]).is_err());
    }

    #[test]
    fn start_basic() {
        let cli = parse(&["start", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Start { ref name, ref file, recreate, ref profile })
            if name == "dev" && file.is_none() && !recreate && profile.is_none()
        ));
    }

    #[test]
    fn start_with_file_and_recreate() {
        let cli = parse(&["start", "dev", "--file", "dev.yaml", "--recreate"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Start { ref name, ref file, recreate, .. })
            if name == "dev" && file.as_deref() == Some(std::path::Path::new("dev.yaml")) && recreate
        ));
    }

    #[test]
    fn start_with_profile() {
        let cli = parse(&["start", "myws", "--profile", "ssh-prod"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Start { ref name, ref profile, .. })
            if name == "myws" && profile.as_deref() == Some("ssh-prod")
        ));
    }

    #[test]
    fn start_requires_name() {
        assert!(parse(&["start"]).is_err());
    }

    #[test]
    fn start_profile_conflicts_with_file() {
        assert!(parse(&["start", "dev", "--profile", "x", "--file", "y.yaml"]).is_err());
    }

    #[test]
    fn up_is_not_a_legacy_alias() {
        assert!(parse(&["up", "dev"]).is_err());
    }

    #[test]
    fn attach_basic() {
        let cli = parse(&["attach", "dev"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Attach { ref name }) if name == "dev"));
    }

    #[test]
    fn attach_missing_name() {
        assert!(parse(&["attach"]).is_err());
    }

    #[test]
    fn recreate_basic() {
        let cli = parse(&["recreate", "dev"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Recreate { ref name }) if name == "dev"));
    }

    #[test]
    fn close_basic() {
        let cli = parse(&["close", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Close { ref name, kill }) if name == "dev" && !kill
        ));
    }

    #[test]
    fn close_with_kill() {
        let cli = parse(&["close", "dev", "--kill"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Close { ref name, kill }) if name == "dev" && kill
        ));
    }

    #[test]
    fn save_basic() {
        let cli = parse(&["save", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Save { ref name, ref file }) if name == "dev" && file.is_none()
        ));
    }

    #[test]
    fn save_with_file() {
        let cli = parse(&["save", "dev", "--file", "out.yaml"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Save { ref name, ref file }
            ) if name == "dev" && file.as_deref() == Some(std::path::Path::new("out.yaml"))
        ));
    }

    #[test]
    fn snapshot_basic() {
        let cli = parse(&["snapshot", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Snapshot { ref name, ref file }) if name == "dev" && file.is_none()
        ));
    }

    #[test]
    fn snapshot_with_file() {
        let cli = parse(&["snapshot", "dev", "--file", "snap.json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Snapshot { ref name, ref file }
            ) if name == "dev" && file.as_deref() == Some(std::path::Path::new("snap.json"))
        ));
    }

    // ── List commands ───────────────────────────────────────────

    #[test]
    fn list_workspaces() {
        let cli = parse(&["list", "workspaces"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::List {
                what: ListCommand::Workspaces
            })
        ));
    }

    #[test]
    fn list_instances() {
        let cli = parse(&["list", "instances"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::List {
                what: ListCommand::Instances
            })
        ));
    }

    #[test]
    fn list_panes() {
        let cli = parse(&["list", "panes", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::List { what: ListCommand::Panes { ref workspace } }) if workspace == "dev"
        ));
    }

    #[test]
    fn list_panes_missing_workspace() {
        assert!(parse(&["list", "panes"]).is_err());
    }

    #[test]
    fn list_sessions() {
        let cli = parse(&["list", "sessions", "dev"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::List { what: ListCommand::Sessions { ref workspace } }) if workspace == "dev"
        ));
    }

    #[test]
    fn list_sessions_missing_workspace() {
        assert!(parse(&["list", "sessions"]).is_err());
    }

    #[test]
    fn list_missing_subcommand() {
        assert!(parse(&["list"]).is_err());
    }

    #[test]
    fn recipe_list_accepts_manifest_file() {
        let cli = parse(&["recipe", "list", "--file", "wtd-recipes.yaml"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Recipe {
                action: RecipeCommand::List { ref file }
            }) if file.as_deref() == Some(std::path::Path::new("wtd-recipes.yaml"))
        ));
    }

    #[test]
    fn recipe_run_accepts_dry_run() {
        let cli = parse(&["recipe", "run", "test-and-review", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Recipe {
                action: RecipeCommand::Run {
                    ref name,
                    dry_run: true,
                    ..
                }
            }) if name == "test-and-review"
        ));
    }

    #[test]
    fn recipe_run_accepts_changed_workflow_confirmation() {
        let cli = parse(&[
            "recipe",
            "run",
            "test-and-review",
            "--allow-changed-workflow",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Recipe {
                action: RecipeCommand::Run {
                    ref name,
                    allow_changed_workflow: true,
                    ..
                }
            }) if name == "test-and-review"
        ));
    }

    // ── Pane / session commands ─────────────────────────────────

    #[test]
    fn focus_basic() {
        let cli = parse(&["focus", "dev/server"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Focus { ref target }) if target == "dev/server")
        );
    }

    #[test]
    fn focus_missing_target() {
        assert!(parse(&["focus"]).is_err());
    }

    #[test]
    fn rename_basic() {
        let cli = parse(&["rename", "dev/server", "api-server"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Rename { ref target, ref new_name }
            ) if target == "dev/server" && new_name == "api-server"
        ));
    }

    #[test]
    fn rename_missing_new_name() {
        assert!(parse(&["rename", "dev/server"]).is_err());
    }

    #[test]
    fn action_basic() {
        let cli = parse(&["action", "dev/server", "split-right"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Action { ref target, ref action_name, ref args }
            ) if target == "dev/server" && action_name == "split-right" && args.is_empty()
        ));
    }

    #[test]
    fn action_with_args() {
        let cli = parse(&["action", "dev/server", "resize-pane-grow-right", "cells=5"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Action { ref target, ref action_name, ref args }
            ) if target == "dev/server"
                && action_name == "resize-pane-grow-right"
                && args == &["cells=5"]
        ));
    }

    #[test]
    fn action_missing_action_name() {
        assert!(parse(&["action", "dev/server"]).is_err());
    }

    // ── Input commands ──────────────────────────────────────────

    #[test]
    fn send_basic() {
        let cli = parse(&["send", "dev/server", "echo hello"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Send { ref target, ref text, no_newline }
            ) if target == "dev/server" && text == "echo hello" && !no_newline
        ));
    }

    #[test]
    fn send_no_newline() {
        let cli = parse(&["send", "dev/server", "data", "--no-newline"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Send { ref target, ref text, no_newline }
            ) if target == "dev/server" && text == "data" && no_newline
        ));
    }

    #[test]
    fn send_missing_text() {
        assert!(parse(&["send", "dev/server"]).is_err());
    }

    #[test]
    fn prompt_basic() {
        let cli = parse(&["prompt", "dev/server", "Explain this"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Prompt {
                ref target,
                ref text,
            }) if target == "dev/server" && text == "Explain this"
        ));
    }

    #[test]
    fn configure_pane_basic() {
        let cli = parse(&[
            "configure-pane",
            "dev/server",
            "--driver-profile",
            "claude-code",
            "--soft-break-key",
            "Shift+Enter",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ConfigurePane {
                ref target,
                driver_profile: Some(DriverProfileArg::ClaudeCode),
                submit_key: None,
                soft_break_key: Some(ref key),
                clear_soft_break: false,
                clear_driver: false,
            }) if target == "dev/server" && key == "Shift+Enter"
        ));
    }

    #[test]
    fn prompt_help_is_agent_oriented() {
        let mut cmd = Cli::command();
        let sub = cmd.find_subcommand_mut("prompt").unwrap();
        let mut help = Vec::new();
        sub.write_long_help(&mut help).unwrap();
        let text = String::from_utf8(help).unwrap();
        assert!(text.contains("Recommended agent flow"));
        assert!(text.contains("wtd prompt <target> \"<text>\""));
        assert!(text.contains("wtd capture <target>"));
        assert!(text.contains("override the inferred driver"));
    }

    #[test]
    fn configure_pane_help_mentions_prompt_workflow() {
        let mut cmd = Cli::command();
        let sub = cmd.find_subcommand_mut("configure-pane").unwrap();
        let mut help = Vec::new();
        sub.write_long_help(&mut help).unwrap();
        let text = String::from_utf8(help).unwrap();
        assert!(text.contains("auto-detects common agent panes"));
        assert!(text.contains("override or pin the driver behavior"));
    }

    #[test]
    fn root_help_has_agent_quick_path() {
        let mut cmd = Cli::command();
        let mut help = Vec::new();
        cmd.write_long_help(&mut help).unwrap();
        let text = String::from_utf8(help).unwrap();
        assert!(text.contains("Coding-agent quick path"));
        assert!(text.contains("wtd prompt <target> \"<text>\""));
        assert!(text.contains("wtd capture <target>"));
        assert!(text.contains("configure-pane <target> ..."));
        assert!(text.contains("auto-detected"));
    }

    #[test]
    fn send_help_steers_agents_to_prompt() {
        let mut cmd = Cli::command();
        let sub = cmd.find_subcommand_mut("send").unwrap();
        let mut help = Vec::new();
        sub.write_long_help(&mut help).unwrap();
        let text = String::from_utf8(help).unwrap();
        assert!(text.contains("low-level"));
        assert!(text.contains("prefer"));
        assert!(text.contains("wtd prompt"));
    }

    #[test]
    fn keys_basic() {
        let cli = parse(&["keys", "dev/server", "Enter"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Keys { ref target, ref key_specs }
            ) if target == "dev/server" && key_specs == &["Enter"]
        ));
    }

    #[test]
    fn keys_multiple() {
        let cli = parse(&["keys", "dev/server", "Ctrl+C", "Enter", "Up"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Keys { ref target, ref key_specs }
            ) if target == "dev/server" && key_specs == &["Ctrl+C", "Enter", "Up"]
        ));
    }

    #[test]
    fn keys_missing_spec() {
        assert!(parse(&["keys", "dev/server"]).is_err());
    }

    #[test]
    fn mouse_basic_click() {
        let cli = parse(&["mouse", "dev/server", "click", "--col", "12", "--row", "7"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Mouse {
                ref target,
                kind: MouseKindArg::Click,
                col: 12,
                row: 7,
                button: None,
                repeat: 1,
                shift: false,
                alt: false,
                ctrl: false,
                force: false,
            }) if target == "dev/server"
        ));
    }

    #[test]
    fn mouse_with_modifiers_and_button() {
        let cli = parse(&[
            "mouse",
            "dev/server",
            "move",
            "--col",
            "4",
            "--row",
            "9",
            "--button",
            "left",
            "--shift",
            "--ctrl",
            "--repeat",
            "3",
            "--force",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Mouse {
                kind: MouseKindArg::Move,
                col: 4,
                row: 9,
                button: Some(MouseButtonArg::Left),
                repeat: 3,
                shift: true,
                alt: false,
                ctrl: true,
                force: true,
                ..
            })
        ));
    }

    #[test]
    fn mouse_requires_coordinates() {
        assert!(parse(&["mouse", "dev/server", "click"]).is_err());
    }

    #[test]
    fn input_escape_mode() {
        let cli = parse(&["input", "dev/server", "\\e[<35;40;12M", "--escape"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Input { ref target, ref data, escape, hex, base64 }
            ) if target == "dev/server" && data == "\\e[<35;40;12M" && escape && !hex && !base64
        ));
    }

    #[test]
    fn input_hex_mode() {
        let cli = parse(&["input", "dev/server", "1b5b41", "--hex"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Input { ref target, ref data, escape, hex, base64 }
            ) if target == "dev/server" && data == "1b5b41" && !escape && hex && !base64
        ));
    }

    // ── Inspection commands ─────────────────────────────────────

    #[test]
    fn capture_basic() {
        let cli = parse(&["capture", "dev/server"]).unwrap();
        if let Some(Command::Capture {
            target,
            vt,
            lines,
            all,
            after,
            after_regex,
            max_lines,
            count,
        }) = &cli.command
        {
            assert_eq!(target, "dev/server");
            assert!(!vt);
            assert!(lines.is_none());
            assert!(!all);
            assert!(after.is_none());
            assert!(after_regex.is_none());
            assert!(max_lines.is_none());
            assert!(!count);
        } else {
            panic!("expected Capture command");
        }
    }

    #[test]
    fn capture_with_flags() {
        let cli = parse(&[
            "capture",
            "dev/server",
            "--lines",
            "50",
            "--all",
            "--after",
            "START",
            "--after-regex",
            "^\\$",
            "--max-lines",
            "100",
            "--count",
        ])
        .unwrap();
        if let Some(Command::Capture {
            target,
            vt,
            lines,
            all,
            after,
            after_regex,
            max_lines,
            count,
        }) = &cli.command
        {
            assert_eq!(target, "dev/server");
            assert!(!vt);
            assert_eq!(*lines, Some(50));
            assert!(*all);
            assert_eq!(after.as_deref(), Some("START"));
            assert_eq!(after_regex.as_deref(), Some("^\\$"));
            assert_eq!(*max_lines, Some(100));
            assert!(*count);
        } else {
            panic!("expected Capture command");
        }
    }

    #[test]
    fn capture_missing_target() {
        assert!(parse(&["capture"]).is_err());
    }

    #[test]
    fn capture_vt_mode() {
        let cli = parse(&["capture", "dev/server", "--vt"]).unwrap();
        if let Some(Command::Capture {
            target,
            vt,
            lines,
            all,
            after,
            after_regex,
            max_lines,
            count,
        }) = &cli.command
        {
            assert_eq!(target, "dev/server");
            assert!(*vt);
            assert!(lines.is_none());
            assert!(!all);
            assert!(after.is_none());
            assert!(after_regex.is_none());
            assert!(max_lines.is_none());
            assert!(!count);
        } else {
            panic!("expected Capture command");
        }
    }

    #[test]
    fn scrollback_basic() {
        let cli = parse(&["scrollback", "dev/server", "--tail", "100"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Scrollback { ref target, tail }
            ) if target == "dev/server" && tail == 100
        ));
    }

    #[test]
    fn scrollback_missing_tail() {
        assert!(parse(&["scrollback", "dev/server"]).is_err());
    }

    #[test]
    fn scrollback_invalid_tail() {
        assert!(parse(&["scrollback", "dev/server", "--tail", "abc"]).is_err());
    }

    #[test]
    fn follow_basic() {
        let cli = parse(&["follow", "dev/server"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Follow { ref target, raw }
            ) if target == "dev/server" && !raw
        ));
    }

    #[test]
    fn follow_raw() {
        let cli = parse(&["follow", "dev/server", "--raw"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Follow { ref target, raw }
            ) if target == "dev/server" && raw
        ));
    }

    #[test]
    fn inspect_basic() {
        let cli = parse(&["inspect", "dev/server"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Inspect { ref target }) if target == "dev/server")
        );
    }

    #[test]
    fn notify_defaults_to_needs_attention() {
        let cli = parse(&["notify", "dev/server", "input requested"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Notify {
                ref target,
                state: AttentionStateArg::NeedsAttention,
                ref source,
                ref message,
            }) if target == "dev/server"
                && source.is_none()
                && message.as_deref() == Some("input requested")
        ));
    }

    #[test]
    fn notify_accepts_done_and_source() {
        let cli = parse(&[
            "notify",
            "dev/server",
            "--state",
            "done",
            "--source",
            "codex",
            "tests passed",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Notify {
                ref target,
                state: AttentionStateArg::Done,
                ref source,
                ref message,
            }) if target == "dev/server"
                && source.as_deref() == Some("codex")
                && message.as_deref() == Some("tests passed")
        ));
    }

    #[test]
    fn clear_attention_basic() {
        let cli = parse(&["clear-attention", "dev/server"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::ClearAttention { ref target }) if target == "dev/server"
        ));
    }

    #[test]
    fn status_basic() {
        let cli = parse(&[
            "status",
            "dev/server",
            "--phase",
            "working",
            "--source",
            "codex",
            "--queue-pending",
            "2",
            "running tests",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Status {
                ref target,
                ref phase,
                ref source,
                queue_pending: Some(2),
                ref status_text,
                ..
            }) if target == "dev/server"
                && phase.as_deref() == Some("working")
                && source.as_deref() == Some("codex")
                && status_text.as_deref() == Some("running tests")
        ));
    }

    #[test]
    fn wait_accepts_condition_timeout_poll_and_recent_lines() {
        let cli = parse(&[
            "wait",
            "dev/server",
            "--for",
            "needs-attention",
            "--timeout",
            "1.5",
            "--poll-ms",
            "10",
            "--recent-lines",
            "5",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Wait {
                ref target,
                condition: WaitConditionArg::NeedsAttention,
                timeout,
                poll_ms: 10,
                recent_lines: 5,
            }) if target == "dev/server" && (timeout - 1.5).abs() < f64::EPSILON
        ));
    }

    // ── Host management commands ────────────────────────────────

    #[test]
    fn host_status() {
        let cli = parse(&["host", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Host {
                action: HostCommand::Status
            })
        ));
    }

    #[test]
    fn host_stop() {
        let cli = parse(&["host", "stop"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Host {
                action: HostCommand::Stop
            })
        ));
    }

    #[test]
    fn host_missing_subcommand() {
        assert!(parse(&["host"]).is_err());
    }

    // ── Global flags ────────────────────────────────────────────

    #[test]
    fn json_flag() {
        let cli = parse(&["--json", "list", "workspaces"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn json_flag_after_command() {
        let cli = parse(&["list", "workspaces", "--json"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn verbose_flag() {
        let cli = parse(&["--verbose", "list", "instances"]).unwrap();
        assert!(cli.verbose);
    }

    #[test]
    fn id_flag() {
        let cli = parse(&[
            "--id",
            "550e8400-e29b-41d4-a716-446655440000",
            "capture",
            "dev/server",
        ])
        .unwrap();
        assert_eq!(
            cli.id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn id_flag_with_inspect() {
        let cli = parse(&["inspect", "dev/server", "--id", "abc-123"]).unwrap();
        assert_eq!(cli.id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn timeout_flag() {
        let cli = parse(&["--timeout", "10", "capture", "dev/server"]).unwrap();
        assert_eq!(cli.timeout, Some(10.0));
    }

    #[test]
    fn timeout_flag_after_command() {
        let cli = parse(&["list", "workspaces", "--timeout", "5.5"]).unwrap();
        assert_eq!(cli.timeout, Some(5.5));
    }

    #[test]
    fn timeout_flag_absent() {
        let cli = parse(&["capture", "dev/server"]).unwrap();
        assert!(cli.timeout.is_none());
    }

    #[test]
    fn combined_global_flags() {
        let cli = parse(&["--json", "--verbose", "list", "panes", "dev"]).unwrap();
        assert!(cli.json);
        assert!(cli.verbose);
    }

    // ── Version and help ────────────────────────────────────────

    #[test]
    fn version_flag_produces_output() {
        let err = parse(&["--version"]).unwrap_err();
        assert!(err.contains("wtd"), "expected version output, got: {err}");
    }

    #[test]
    fn help_flag_produces_output() {
        let err = parse(&["--help"]).unwrap_err();
        assert!(err.contains("Usage"), "expected help output, got: {err}");
    }

    #[test]
    fn subcommand_help() {
        let err = parse(&["open", "--help"]).unwrap_err();
        assert!(err.contains("Usage"), "expected help output, got: {err}");
    }

    #[test]
    fn list_help() {
        let err = parse(&["list", "--help"]).unwrap_err();
        assert!(
            err.contains("workspaces"),
            "expected list subcommands in help, got: {err}"
        );
    }

    // ── Error messages ──────────────────────────────────────────

    #[test]
    fn unknown_command_produces_error() {
        let err = parse(&["frobnicate"]).unwrap_err();
        assert!(
            err.contains("unrecognized") || err.contains("invalid"),
            "expected helpful error, got: {err}"
        );
    }

    #[test]
    fn bare_wtd_parses_as_none_command() {
        let cli = parse(&[]).unwrap();
        assert!(cli.command.is_none());
    }

    // ── Shell completions ───────────────────────────────────────

    #[test]
    fn completions_parse() {
        let cli = parse(&["completions", "bash"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Completions { shell: Shell::Bash })
        ));
    }

    #[test]
    fn completions_powershell() {
        let cli = parse(&["completions", "powershell"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Completions {
                shell: Shell::PowerShell
            })
        ));
    }

    // ── Target path formats ─────────────────────────────────────

    #[test]
    fn single_segment_target() {
        let cli = parse(&["capture", "server"]).unwrap();
        if let Some(Command::Capture { target, .. }) = &cli.command {
            assert_eq!(target, "server");
        } else {
            panic!("expected Capture");
        }
    }

    #[test]
    fn two_segment_target() {
        let cli = parse(&["capture", "dev/server"]).unwrap();
        if let Some(Command::Capture { target, .. }) = &cli.command {
            assert_eq!(target, "dev/server");
        } else {
            panic!("expected Capture");
        }
    }

    #[test]
    fn three_segment_target() {
        let cli = parse(&["capture", "dev/backend/server"]).unwrap();
        if let Some(Command::Capture { target, .. }) = &cli.command {
            assert_eq!(target, "dev/backend/server");
        } else {
            panic!("expected Capture");
        }
    }

    #[test]
    fn four_segment_target() {
        let cli = parse(&["capture", "dev/main/backend/server"]).unwrap();
        if let Some(Command::Capture { target, .. }) = &cli.command {
            assert_eq!(target, "dev/main/backend/server");
        } else {
            panic!("expected Capture");
        }
    }
}
