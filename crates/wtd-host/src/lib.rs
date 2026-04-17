//! `wtd-host` library — session management and host process logic.

pub mod action;
pub mod backoff;
pub mod host_lifecycle;
#[cfg(windows)]
pub mod ipc_server;
#[cfg(windows)]
pub mod output_broadcaster;
pub mod pipe_security;
pub mod prompt_driver;
#[cfg(windows)]
pub mod request_handler;
pub mod session;
pub mod target_resolver;
pub mod terminal_input;
pub mod workspace_instance;
