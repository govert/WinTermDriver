//! Structured logging infrastructure for all WinTermDriver processes (§31).
//!
//! Three initialization modes map to the three process types:
//! - [`init_host_logging`] — file appender with rotation + stderr
//! - [`init_stderr_logging`] — stderr only (for CLI and UI)
//!
//! The log level is determined by (highest priority first):
//! 1. `WTD_LOG` environment variable (e.g. `WTD_LOG=debug`)
//! 2. `logLevel` field in global settings YAML
//! 3. Default: `info`

use std::path::{Path, PathBuf};

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::global_settings::LogLevel;

// ── Constants ────────────────────────────────────────────────────────────────

/// Default log subdirectory under data dir.
const LOG_DIR_NAME: &str = "logs";

/// Host log file prefix.
const HOST_LOG_PREFIX: &str = "wtd-host.log";

/// Maximum number of rotated log files to keep (§31.1).
pub const MAX_LOG_FILES: usize = 5;

// ── LogLevel → tracing ──────────────────────────────────────────────────────

impl LogLevel {
    /// Convert to a `tracing::Level`.
    pub fn to_tracing_level(&self) -> tracing::Level {
        match self {
            LogLevel::Trace => tracing::Level::TRACE,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }

    /// Convert to the string used in `EnvFilter` directives.
    pub fn as_filter_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

// ── Resolve effective level ──────────────────────────────────────────────────

/// Determine the effective log level: `WTD_LOG` env var overrides the settings
/// value. Returns the filter string (e.g. `"debug"`).
pub fn effective_log_filter(settings_level: &LogLevel) -> String {
    std::env::var("WTD_LOG").unwrap_or_else(|_| settings_level.as_filter_str().to_owned())
}

// ── Host logging (file + stderr) ─────────────────────────────────────────────

/// Compute the log directory path.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(LOG_DIR_NAME)
}

/// Initialise logging for `wtd-host` (§31.1): log file with rotation + stderr.
///
/// The log file is written to `<data_dir>/logs/wtd-host.log`. Rotation happens
/// daily (tracing-appender does not support size-based rotation out of the box;
/// daily rotation with `MAX_LOG_FILES` kept files approximates the §31.1 target
/// of 10 MB × 5 files for typical workloads).
///
/// Returns the [`tracing_appender::non_blocking::WorkerGuard`] which **must be
/// held alive** for the lifetime of the process (dropping it flushes and stops
/// the background writer).
pub fn init_host_logging(
    settings_level: &LogLevel,
    data_dir: &Path,
) -> tracing_appender::non_blocking::WorkerGuard {
    let log_path = log_dir(data_dir);
    std::fs::create_dir_all(&log_path).ok();

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .max_log_files(MAX_LOG_FILES)
        .filename_prefix(HOST_LOG_PREFIX)
        .build(&log_path)
        .expect("failed to create log file appender");

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = effective_log_filter(settings_level);

    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true);

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true);

    tracing_subscriber::registry()
        .with(EnvFilter::new(&filter))
        .with(file_layer)
        .with(stderr_layer)
        .init();

    guard
}

/// Initialise logging for `wtd-host` writing to a **specific file path**
/// (useful for tests). Returns the worker guard.
pub fn init_host_logging_to_file(
    settings_level: &LogLevel,
    log_file_dir: &Path,
    log_file_name: &str,
) -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix(log_file_name)
        .build(log_file_dir)
        .expect("failed to create log file appender");

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = effective_log_filter(settings_level);

    let layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);

    tracing_subscriber::registry()
        .with(EnvFilter::new(&filter))
        .with(layer)
        .init();

    guard
}

// ── Stderr-only logging (CLI + UI) ──────────────────────────────────────────

/// Initialise logging for `wtd` CLI and `wtd-ui` (§31.1): stderr only.
pub fn init_stderr_logging(settings_level: &LogLevel) {
    let filter = effective_log_filter(settings_level);

    tracing_subscriber::registry()
        .with(EnvFilter::new(&filter))
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false)
                .with_target(true),
        )
        .init();
}

// ── Test helpers ─────────────────────────────────────────────────────────────

/// Initialise a test-friendly subscriber that writes to a buffer and returns
/// the collected output. Only call once per test process (tracing global
/// subscriber can only be set once).
///
/// Returns the guard — drop it to flush.
pub fn init_test_logging_to_file(
    level: &LogLevel,
    dir: &Path,
    file_name: &str,
) -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix(file_name)
        .build(dir)
        .expect("failed to create test log appender");

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = effective_log_filter(level);

    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new(&filter))
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true),
        );

    // Use try_init so tests that run after the first don't panic
    let _ = subscriber.try_init();

    guard
}
