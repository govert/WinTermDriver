//! Async IPC client for connecting the UI to `wtd-host`.
//!
//! Handles pipe connection with retry, handshake (`clientType: "ui"`),
//! AttachWorkspace, and bidirectional message flow (push notifications
//! from host, fire-and-forget commands to host).

use thiserror::Error;
use wtd_ipc::connect::ConnectError;
use wtd_ipc::message::{ClientType, Handshake, HandshakeAck, MessagePayload};
use wtd_ipc::{connect, Envelope, IpcError, PROTOCOL_VERSION};

/// Errors from UI client operations.
#[derive(Debug, Error)]
pub enum UiClientError {
    #[error("{0}")]
    Connect(#[from] ConnectError),

    #[error("{0}")]
    Ipc(#[from] IpcError),

    #[error("handshake failed: {0}")]
    Handshake(String),
}

// ── Windows implementation ───────────────────────────────────────────

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::Duration;
    use tokio::io::{ReadHalf, WriteHalf};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use wtd_ipc::framing::{read_frame_async, write_frame_async};

    /// IPC client connected to `wtd-host` via named pipe with `clientType: "ui"`.
    pub struct UiIpcClient {
        pipe: NamedPipeClient,
    }

    /// Read half of a split UI IPC connection.
    pub struct UiIpcReader {
        reader: ReadHalf<NamedPipeClient>,
    }

    /// Write half of a split UI IPC connection.
    pub struct UiIpcWriter {
        writer: WriteHalf<NamedPipeClient>,
    }

    impl UiIpcClient {
        /// Connect to the host, auto-starting if necessary, and perform handshake.
        pub async fn connect_and_handshake() -> Result<Self, UiClientError> {
            let pipe_name = connect::pipe_name_for_current_user()?;
            connect::ensure_host_running(&pipe_name).await?;
            Self::connect_to(&pipe_name).await
        }

        /// Connect to a specific pipe name and perform handshake.
        pub async fn connect_to(pipe_name: &str) -> Result<Self, UiClientError> {
            let pipe = connect_to_pipe(pipe_name).await?;
            let mut client = Self { pipe };
            client.do_handshake().await?;
            Ok(client)
        }

        async fn do_handshake(&mut self) -> Result<(), UiClientError> {
            let hs = Envelope::new(
                "ui-hs-1",
                &Handshake {
                    client_type: ClientType::Ui,
                    client_version: env!("CARGO_PKG_VERSION").to_owned(),
                    protocol_version: PROTOCOL_VERSION,
                },
            );
            write_frame_async(&mut self.pipe, &hs).await?;
            let ack = read_frame_async(&mut self.pipe).await?;
            if ack.msg_type != HandshakeAck::TYPE_NAME {
                return Err(UiClientError::Handshake(format!(
                    "expected HandshakeAck, got {}",
                    ack.msg_type
                )));
            }
            Ok(())
        }

        /// Send a request and wait for the response.
        pub async fn request(&mut self, envelope: &Envelope) -> Result<Envelope, UiClientError> {
            write_frame_async(&mut self.pipe, envelope).await?;
            let response = read_frame_async(&mut self.pipe).await?;
            Ok(response)
        }

        /// Split the connection into independent read and write halves.
        ///
        /// Use this after the initial attach so the reader can run in a
        /// background task while the writer sends fire-and-forget messages.
        pub fn split(self) -> (UiIpcReader, UiIpcWriter) {
            let (reader, writer) = tokio::io::split(self.pipe);
            (UiIpcReader { reader }, UiIpcWriter { writer })
        }
    }

    impl UiIpcReader {
        /// Read one frame from the host.
        pub async fn read_frame(&mut self) -> Result<Envelope, UiClientError> {
            Ok(read_frame_async(&mut self.reader).await?)
        }
    }

    impl UiIpcWriter {
        /// Write one frame to the host.
        pub async fn write_frame(&mut self, envelope: &Envelope) -> Result<(), UiClientError> {
            Ok(write_frame_async(&mut self.writer, envelope).await?)
        }
    }

    /// Connect to a named pipe with retry loop.
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
pub use win::{UiIpcClient, UiIpcReader, UiIpcWriter};

// ── Non-Windows stub ─────────────────────────────────────────────────

#[cfg(not(windows))]
pub struct UiIpcClient;
#[cfg(not(windows))]
pub struct UiIpcReader;
#[cfg(not(windows))]
pub struct UiIpcWriter;

#[cfg(not(windows))]
impl UiIpcClient {
    pub async fn connect_and_handshake() -> Result<Self, UiClientError> {
        Err(UiClientError::Connect(ConnectError::PipeName(
            "named pipes not supported on this platform".into(),
        )))
    }
    pub async fn connect_to(_pipe_name: &str) -> Result<Self, UiClientError> {
        Err(UiClientError::Connect(ConnectError::PipeName(
            "named pipes not supported on this platform".into(),
        )))
    }
    pub async fn request(&mut self, _: &Envelope) -> Result<Envelope, UiClientError> {
        unreachable!()
    }
    pub fn split(self) -> (UiIpcReader, UiIpcWriter) {
        unreachable!()
    }
}

#[cfg(not(windows))]
impl UiIpcReader {
    pub async fn read_frame(&mut self) -> Result<Envelope, UiClientError> {
        unreachable!()
    }
}

#[cfg(not(windows))]
impl UiIpcWriter {
    pub async fn write_frame(&mut self, _: &Envelope) -> Result<(), UiClientError> {
        unreachable!()
    }
}
