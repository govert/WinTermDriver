//! `wtd-host` library — session management and host process logic.

pub mod action;
pub mod backoff;
pub mod host_lifecycle;
#[cfg(windows)]
pub mod ipc_server;
pub mod pipe_security;
#[cfg(windows)]
pub mod request_handler;
pub mod session;
pub mod target_resolver;
pub mod workspace_instance;
