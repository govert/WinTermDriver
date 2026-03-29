//! `wtd-host` — WinTermDriver host process.
//!
//! Per-user singleton background process. Owns all ConPTY sessions, workspace
//! instance state, and the named pipe IPC server.
//!
//! See spec §8.1 and §16 for the full host lifecycle.

#[cfg(windows)]
mod run {
    use wtd_host::host_lifecycle::*;
    use wtd_host::ipc_server::{ClientId, RequestHandler};
    use wtd_host::pipe_security::pipe_name_for_current_user;
    use wtd_ipc::message::TypedMessage;
    use wtd_ipc::Envelope;

    /// Stub request handler — returns no response for all requests.
    /// Real request dispatching will be wired up in a future bead.
    struct StubHandler;

    impl RequestHandler for StubHandler {
        fn handle_request(
            &self,
            _client_id: ClientId,
            _envelope: &Envelope,
            _msg: &TypedMessage,
        ) -> Option<Envelope> {
            None
        }
    }

    pub async fn run() -> anyhow::Result<()> {
        // 1. Determine pipe name from current user SID (§16.5).
        let pipe_name = pipe_name_for_current_user()?;
        let dir = data_dir();

        // 2. Single-instance check.
        match check_single_instance_in(&pipe_name, &dir) {
            SingleInstanceCheck::AlreadyRunning => {
                eprintln!("wtd-host: another instance is already running");
                std::process::exit(1);
            }
            SingleInstanceCheck::StalePidCleaned => {
                eprintln!(
                    "wtd-host: cleaned stale PID file from previous crash"
                );
            }
            SingleInstanceCheck::Available => {}
        }

        // 3. Shutdown channel.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // 4. Install console ctrl handler for graceful shutdown (§16.3).
        if let Err(e) = install_ctrl_handler(shutdown_tx) {
            eprintln!(
                "wtd-host: warning: could not install ctrl handler: {}",
                e
            );
        }

        eprintln!("wtd-host: started (PID {})", std::process::id());

        // 5. Run the IPC server until shutdown.
        run_host(&pipe_name, StubHandler, shutdown_rx, &dir).await?;

        eprintln!("wtd-host: shut down");
        Ok(())
    }
}

#[cfg(windows)]
#[tokio::main]
async fn main() {
    if let Err(e) = run::run().await {
        eprintln!("wtd-host: fatal: {}", e);
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("wtd-host: this binary requires Windows");
    std::process::exit(1);
}
