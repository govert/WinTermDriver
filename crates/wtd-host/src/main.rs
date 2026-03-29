//! `wtd-host` — WinTermDriver host process.
//!
//! Per-user singleton background process. Owns all ConPTY sessions, workspace
//! instance state, and the named pipe IPC server.
//!
//! See spec §8.1 and §16 for the full host lifecycle.

#[cfg(windows)]
mod run {
    use wtd_core::logging::init_host_logging;
    use wtd_core::GlobalSettings;
    use wtd_host::host_lifecycle::*;
    use wtd_host::pipe_security::pipe_name_for_current_user;
    use wtd_host::request_handler::HostRequestHandler;

    pub async fn run() -> anyhow::Result<()> {
        // 1. Determine pipe name from current user SID (§16.5).
        let pipe_name = pipe_name_for_current_user()?;
        let dir = data_dir();

        // 0. Initialise logging (§31.1): file + stderr.
        let settings = GlobalSettings::default();
        let _log_guard = init_host_logging(&settings.log_level, &dir);

        // 2. Single-instance check.
        match check_single_instance_in(&pipe_name, &dir) {
            SingleInstanceCheck::AlreadyRunning => {
                tracing::error!("another instance is already running");
                std::process::exit(1);
            }
            SingleInstanceCheck::StalePidCleaned => {
                tracing::warn!("cleaned stale PID file from previous crash");
            }
            SingleInstanceCheck::Available => {}
        }

        // 3. Shutdown channel.
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // 4. Install console ctrl handler for graceful shutdown (§16.3).
        if let Err(e) = install_ctrl_handler(shutdown_tx) {
            tracing::warn!("could not install ctrl handler: {}", e);
        }

        tracing::info!(pid = std::process::id(), "wtd-host started");

        // 5. Run the IPC server with real request handler (§8.1).
        let handler = HostRequestHandler::new(settings);
        run_host(&pipe_name, handler, shutdown_rx, &dir).await?;

        tracing::info!("wtd-host shut down");
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
