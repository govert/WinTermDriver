//! Named pipe IPC server (spec section 13).
//!
//! Accepts multiple concurrent client connections on a Windows named pipe,
//! performs the protocol handshake, routes requests to a [`RequestHandler`],
//! and supports pushing notifications/streaming output to connected clients.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch, Mutex};
use wtd_ipc::message::*;
use wtd_ipc::{Envelope, IpcError, MAX_MESSAGE_SIZE};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
#[cfg(windows)]
use windows::Win32::Foundation::HANDLE;

use crate::pipe_security::{PipeSecurity, PipeSecurityError};

/// Opaque identifier for a connected client.
pub type ClientId = u64;

/// IPC protocol version (spec section 13.6).
pub const PROTOCOL_VERSION: u32 = 1;

/// Host version reported in the handshake.
const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Errors ─────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IPC error: {0}")]
    Ipc(#[from] IpcError),

    #[error("pipe security error: {0}")]
    Security(#[from] PipeSecurityError),
}

// ── Request handler trait ──────────────────────────────────────────────

/// Trait for dispatching incoming client requests to business logic.
///
/// Return `Some(envelope)` with the response, or `None` for fire-and-forget
/// messages (e.g. `SessionInput`).
pub trait RequestHandler: std::marker::Send + std::marker::Sync + 'static {
    fn handle_request(
        &self,
        client_id: ClientId,
        envelope: &Envelope,
        msg: &TypedMessage,
    ) -> Option<Envelope>;
}

// ── Client registry ────────────────────────────────────────────────────

struct ClientEntry {
    client_type: Option<ClientType>,
    handshake_complete: bool,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// Registry of connected clients, protected by a [`Mutex`].
pub struct ClientRegistry {
    entries: HashMap<ClientId, ClientEntry>,
}

impl ClientRegistry {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn register(&mut self, id: ClientId, frame_tx: mpsc::UnboundedSender<Vec<u8>>) {
        self.entries.insert(
            id,
            ClientEntry {
                client_type: None,
                handshake_complete: false,
                frame_tx,
            },
        );
    }

    fn unregister(&mut self, id: ClientId) {
        self.entries.remove(&id);
    }

    fn complete_handshake(&mut self, id: ClientId, client_type: ClientType) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.client_type = Some(client_type);
            entry.handshake_complete = true;
        }
    }

    fn is_handshake_complete(&self, id: ClientId) -> bool {
        self.entries
            .get(&id)
            .map_or(false, |e| e.handshake_complete)
    }

    fn send_frame_to(&self, id: ClientId, frame: Vec<u8>) -> bool {
        self.entries
            .get(&id)
            .map_or(false, |e| e.frame_tx.send(frame).is_ok())
    }

    fn broadcast_to_ui(&self, frame: &[u8]) {
        for entry in self.entries.values() {
            if entry.handshake_complete && entry.client_type == Some(ClientType::Ui) {
                let _ = entry.frame_tx.send(frame.to_vec());
            }
        }
    }

    /// Number of currently connected clients.
    pub fn client_count(&self) -> usize {
        self.entries.len()
    }
}

// ── IPC Server ─────────────────────────────────────────────────────────

/// Named pipe IPC server.
///
/// Owns the pipe security context, client registry, and request handler.
/// Call [`run`](Self::run) to begin accepting connections.
pub struct IpcServer {
    pipe_name: String,
    clients: Arc<Mutex<ClientRegistry>>,
    handler: Arc<dyn RequestHandler>,
    next_client_id: AtomicU64,
    security: Arc<PipeSecurity>,
}

impl IpcServer {
    /// Create a new IPC server on the given pipe name.
    ///
    /// The pipe is created with a DACL granting access only to the current user.
    #[cfg(windows)]
    pub fn new(
        pipe_name: String,
        handler: impl RequestHandler,
    ) -> Result<Self, ServerError> {
        let security = PipeSecurity::new()?;
        Ok(Self {
            pipe_name,
            clients: Arc::new(Mutex::new(ClientRegistry::new())),
            handler: Arc::new(handler),
            next_client_id: AtomicU64::new(1),
            security: Arc::new(security),
        })
    }

    /// Handle to the shared client registry.
    pub fn clients(&self) -> &Arc<Mutex<ClientRegistry>> {
        &self.clients
    }

    /// Send a message to all connected UI clients (spec section 13.13).
    pub async fn broadcast_to_ui(&self, envelope: &Envelope) -> Result<(), IpcError> {
        let frame = wtd_ipc::encode(envelope)?;
        let reg = self.clients.lock().await;
        reg.broadcast_to_ui(&frame);
        Ok(())
    }

    /// Send a message to a specific client.
    pub async fn send_to_client(
        &self,
        client_id: ClientId,
        envelope: &Envelope,
    ) -> Result<(), IpcError> {
        let frame = wtd_ipc::encode(envelope)?;
        let reg = self.clients.lock().await;
        reg.send_frame_to(client_id, frame);
        Ok(())
    }

    /// Run the accept loop until shutdown is signalled.
    ///
    /// Spawns a tokio task for each accepted connection. Existing connections
    /// continue until the client disconnects or the runtime shuts down.
    #[cfg(windows)]
    pub async fn run(
        &self,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<(), ServerError> {
        let mut first_instance = true;
        loop {
            let server = self.create_pipe_instance(first_instance)?;
            first_instance = false;

            let connected = tokio::select! {
                biased;
                _ = shutdown_rx.changed() => false,
                result = server.connect() => {
                    match result {
                        Ok(()) => true,
                        Err(e) => {
                            eprintln!("wtd-host: pipe connect error: {}", e);
                            continue;
                        }
                    }
                }
            };

            if !connected {
                break;
            }

            self.accept_connection(server);
        }
        Ok(())
    }

    #[cfg(windows)]
    fn create_pipe_instance(
        &self,
        first: bool,
    ) -> Result<NamedPipeServer, ServerError> {
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .create_with_security_attributes_raw(
                    &self.pipe_name,
                    self.security.security_attributes_ptr(),
                )?
        };
        Ok(server)
    }

    #[cfg(windows)]
    fn accept_connection(&self, pipe: NamedPipeServer) {
        let client_id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        let clients = self.clients.clone();
        let handler = self.handler.clone();
        let security = self.security.clone();

        tokio::spawn(async move {
            // Verify client SID (spec section 28.3).
            {
                let raw = pipe.as_raw_handle();
                let handle = HANDLE(raw);
                match security.verify_client_sid(handle) {
                    Ok(true) => {}
                    Ok(false) => {
                        eprintln!(
                            "wtd-host: client {} SID mismatch, rejecting",
                            client_id
                        );
                        return;
                    }
                    Err(e) => {
                        eprintln!(
                            "wtd-host: SID verification error for client {}: {}",
                            client_id, e
                        );
                        return;
                    }
                }
            }

            let (frame_tx, frame_rx) = mpsc::unbounded_channel();
            {
                let mut reg = clients.lock().await;
                reg.register(client_id, frame_tx);
            }

            let _ =
                run_connection(client_id, pipe, frame_rx, clients.clone(), handler)
                    .await;

            {
                let mut reg = clients.lock().await;
                reg.unregister(client_id);
            }
        });
    }
}

// ── Connection handler ─────────────────────────────────────────────────

#[cfg(windows)]
async fn run_connection(
    client_id: ClientId,
    pipe: NamedPipeServer,
    mut frame_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    clients: Arc<Mutex<ClientRegistry>>,
    handler: Arc<dyn RequestHandler>,
) -> Result<(), IpcError> {
    let (mut read_half, mut write_half) = tokio::io::split(pipe);

    loop {
        tokio::select! {
            result = read_frame(&mut read_half) => {
                match result {
                    Ok(envelope) => {
                        let response = handle_message(
                            client_id, &envelope, &clients, &handler,
                        ).await;
                        if let Some(resp) = response {
                            let frame = wtd_ipc::encode(&resp)?;
                            write_half.write_all(&frame).await?;
                            write_half.flush().await?;
                        }
                    }
                    Err(_) => break,
                }
            }
            Some(frame_bytes) = frame_rx.recv() => {
                if write_half.write_all(&frame_bytes).await.is_err() {
                    break;
                }
                let _ = write_half.flush().await;
            }
        }
    }
    Ok(())
}

// ── Message dispatch ───────────────────────────────────────────────────

async fn handle_message(
    client_id: ClientId,
    envelope: &Envelope,
    clients: &Arc<Mutex<ClientRegistry>>,
    handler: &Arc<dyn RequestHandler>,
) -> Option<Envelope> {
    let msg = match parse_envelope(envelope) {
        Ok(m) => m,
        Err(_) => {
            return Some(make_error_envelope(
                &envelope.id,
                ErrorCode::ProtocolError,
                "failed to parse message payload",
            ));
        }
    };

    // Handshake is handled by the server itself, not the request handler.
    if let TypedMessage::Handshake(ref hs) = msg {
        return handle_handshake(client_id, &envelope.id, hs, clients).await;
    }

    // All other messages require a completed handshake.
    {
        let reg = clients.lock().await;
        if !reg.is_handshake_complete(client_id) {
            return Some(make_error_envelope(
                &envelope.id,
                ErrorCode::ProtocolError,
                "handshake required before sending requests",
            ));
        }
    }

    handler.handle_request(client_id, envelope, &msg)
}

async fn handle_handshake(
    client_id: ClientId,
    msg_id: &str,
    hs: &Handshake,
    clients: &Arc<Mutex<ClientRegistry>>,
) -> Option<Envelope> {
    let mut reg = clients.lock().await;

    if reg.is_handshake_complete(client_id) {
        return Some(make_error_envelope(
            msg_id,
            ErrorCode::ProtocolError,
            "handshake already completed",
        ));
    }

    if hs.protocol_version != PROTOCOL_VERSION {
        return Some(make_error_envelope(
            msg_id,
            ErrorCode::ProtocolError,
            &format!(
                "unsupported protocol version: {} (expected {})",
                hs.protocol_version, PROTOCOL_VERSION
            ),
        ));
    }

    reg.complete_handshake(client_id, hs.client_type.clone());

    Some(Envelope::new(
        msg_id,
        &HandshakeAck {
            host_version: HOST_VERSION.to_owned(),
            protocol_version: PROTOCOL_VERSION,
        },
    ))
}

fn make_error_envelope(id: &str, code: ErrorCode, message: &str) -> Envelope {
    Envelope::new(
        id,
        &ErrorResponse {
            code,
            message: message.to_owned(),
            candidates: None,
        },
    )
}

// ── Async frame I/O ────────────────────────────────────────────────────

/// Read one length-prefixed frame from an async reader (spec section 13.4).
pub async fn read_frame(
    reader: &mut (impl AsyncReadExt + Unpin),
) -> Result<Envelope, IpcError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_SIZE {
        return Err(IpcError::MessageTooLarge {
            size: len,
            max: MAX_MESSAGE_SIZE,
        });
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    let envelope: Envelope = serde_json::from_slice(&payload)?;
    Ok(envelope)
}

/// Write one length-prefixed frame to an async writer (spec section 13.4).
pub async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    envelope: &Envelope,
) -> Result<(), IpcError> {
    let frame = wtd_ipc::encode(envelope)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}
