//! IPC client for connecting to `wtd-host`.
//!
//! Handles pipe connection with retry, handshake, and frame I/O.

use std::time::Duration;

use thiserror::Error;
use wtd_ipc::connect::ConnectError;
use wtd_ipc::message::{ClientType, Handshake, HandshakeAck, MessagePayload};
use wtd_ipc::{connect, Envelope, IpcError, PROTOCOL_VERSION};

/// Default request timeout (30 seconds).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors from client operations.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{0}")]
    Connect(#[from] ConnectError),

    #[error("{0}")]
    Ipc(#[from] IpcError),

    #[error("handshake failed: {0}")]
    Handshake(String),

    #[error("request timed out after {0:.1}s — host may be unresponsive")]
    RequestTimeout(f64),
}

// ── Windows implementation ───────────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use wtd_ipc::framing::{read_frame_async, write_frame_async};

    /// IPC client connected to `wtd-host` via named pipe.
    pub struct IpcClient {
        pipe: NamedPipeClient,
        timeout: Duration,
    }

    impl IpcClient {
        /// Connect to the host, auto-starting if necessary, and perform handshake.
        pub async fn connect_and_handshake() -> Result<Self, ClientError> {
            let pipe_name = connect::pipe_name_for_current_user()?;
            connect::ensure_host_running(&pipe_name).await?;
            Self::connect_to(&pipe_name).await
        }

        /// Connect to a specific pipe name and perform handshake.
        ///
        /// Useful for tests that run their own pipe server.
        pub async fn connect_to(pipe_name: &str) -> Result<Self, ClientError> {
            let pipe = connect_to_pipe(pipe_name).await?;
            let mut client = Self {
                pipe,
                timeout: DEFAULT_TIMEOUT,
            };
            client.do_handshake().await?;
            Ok(client)
        }

        /// Set the request timeout duration.
        pub fn set_timeout(&mut self, timeout: Duration) {
            self.timeout = timeout;
        }

        async fn do_handshake(&mut self) -> Result<(), ClientError> {
            let hs = Envelope::new(
                "hs-1",
                &Handshake {
                    client_type: ClientType::Cli,
                    client_version: env!("CARGO_PKG_VERSION").to_owned(),
                    protocol_version: PROTOCOL_VERSION,
                },
            );
            write_frame_async(&mut self.pipe, &hs).await?;
            let ack = self.read_frame_with_timeout().await?;
            if ack.msg_type != HandshakeAck::TYPE_NAME {
                return Err(ClientError::Handshake(format!(
                    "expected HandshakeAck, got {}",
                    ack.msg_type
                )));
            }
            Ok(())
        }

        /// Send a request and wait for the response, subject to the configured timeout.
        pub async fn request(&mut self, envelope: &Envelope) -> Result<Envelope, ClientError> {
            write_frame_async(&mut self.pipe, envelope).await?;
            self.read_frame_with_timeout().await
        }

        /// Read one frame from the server (for streaming responses like Follow).
        pub async fn read_frame(&mut self) -> Result<Envelope, ClientError> {
            Ok(read_frame_async(&mut self.pipe).await?)
        }

        /// Write one frame to the server.
        pub async fn write_frame(&mut self, envelope: &Envelope) -> Result<(), ClientError> {
            Ok(write_frame_async(&mut self.pipe, envelope).await?)
        }

        async fn read_frame_with_timeout(&mut self) -> Result<Envelope, ClientError> {
            match tokio::time::timeout(self.timeout, read_frame_async(&mut self.pipe)).await {
                Ok(result) => Ok(result?),
                Err(_) => Err(ClientError::RequestTimeout(self.timeout.as_secs_f64())),
            }
        }
    }

    /// Connect to a named pipe with retry loop (handles pipe-busy and not-yet-ready).
    async fn connect_to_pipe(pipe_name: &str) -> Result<NamedPipeClient, ConnectError> {
        for _ in 0..100 {
            match ClientOptions::new().open(pipe_name) {
                Ok(client) => return Ok(client),
                Err(e) if e.raw_os_error() == Some(2) => {
                    // ERROR_FILE_NOT_FOUND — server not ready yet
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) if e.raw_os_error() == Some(231) => {
                    // ERROR_PIPE_BUSY — all instances in use, retry
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => return Err(ConnectError::Io(e)),
            }
        }
        Err(ConnectError::StartupTimeout)
    }
}

#[cfg(windows)]
pub use win::IpcClient;

// ── Non-Windows stub ─────────────────────────────────────────────────

#[cfg(not(windows))]
pub struct IpcClient;

#[cfg(not(windows))]
impl IpcClient {
    pub async fn connect_and_handshake() -> Result<Self, ClientError> {
        Err(ClientError::Connect(ConnectError::PipeName(
            "named pipes not supported on this platform".into(),
        )))
    }

    pub async fn connect_to(_pipe_name: &str) -> Result<Self, ClientError> {
        Err(ClientError::Connect(ConnectError::PipeName(
            "named pipes not supported on this platform".into(),
        )))
    }

    pub fn set_timeout(&mut self, _timeout: Duration) {}

    pub async fn request(&mut self, _: &Envelope) -> Result<Envelope, ClientError> {
        unreachable!()
    }

    pub async fn read_frame(&mut self) -> Result<Envelope, ClientError> {
        unreachable!()
    }

    pub async fn write_frame(&mut self, _: &Envelope) -> Result<(), ClientError> {
        unreachable!()
    }
}
