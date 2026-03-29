//! `wtd-host` library — session management and host process logic.

pub mod action;
pub mod backoff;
#[cfg(windows)]
pub mod ipc_server;
pub mod pipe_security;
pub mod session;
pub mod workspace_instance;
