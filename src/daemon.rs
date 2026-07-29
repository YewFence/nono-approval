use std::fs;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;
use tokio::net::{TcpListener, UnixListener};

use crate::broker::{Broker, BrokerConfig};
use crate::control::ControlContext;
use crate::debug_capture::DebugCapture;
use crate::display::MAX_DETAIL_BYTES;
use crate::protocol::DEFAULT_MAX_BODY_BYTES;
use crate::runtime_path::{
    RuntimePathError, ensure_owner_directory, remove_stale_socket, validate_socket_path,
};
use crate::webhook::{WEBHOOK_PATH, WebhookContext};

#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub webhook_listen: SocketAddr,
    pub control_socket: PathBuf,
    pub broker: BrokerConfig,
    pub max_body_bytes: usize,
    pub max_detail_bytes: usize,
    pub debug_capture: Option<DebugCapture>,
}

impl DaemonConfig {
    #[must_use]
    pub fn new(webhook_listen: SocketAddr, control_socket: PathBuf) -> Self {
        Self {
            webhook_listen,
            control_socket,
            broker: BrokerConfig::default(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_detail_bytes: MAX_DETAIL_BYTES,
            debug_capture: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("webhook listener must use a loopback IP address")]
    NonLoopback,
    #[error(transparent)]
    RuntimePath(#[from] RuntimePathError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("webhook server stopped unexpectedly: {0}")]
    WebhookTask(String),
    #[error("control server stopped unexpectedly: {0}")]
    ControlTask(String),
    #[error(transparent)]
    Broker(#[from] crate::broker::BrokerError),
}

/// Runs the daemon until SIGINT, SIGTERM, or a server failure.
///
/// # Errors
///
/// Returns an error for unsafe paths, invalid listeners, bind failures, or server task failures.
pub async fn run(config: DaemonConfig) -> Result<(), DaemonError> {
    if !config.webhook_listen.ip().is_loopback() {
        return Err(DaemonError::NonLoopback);
    }
    validate_socket_path(&config.control_socket)?;
    let runtime_dir = config.control_socket.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "control socket has no parent")
    })?;
    ensure_owner_directory(runtime_dir)?;
    remove_stale_socket(&config.control_socket).await?;

    let tcp_listener = TcpListener::bind(config.webhook_listen).await?;
    let unix_listener = UnixListener::bind(&config.control_socket)?;
    fs::set_permissions(&config.control_socket, fs::Permissions::from_mode(0o600))?;
    let socket_guard = SocketGuard(config.control_socket.clone());
    let broker = if let Some(capture) = &config.debug_capture {
        Broker::with_debug_capture(config.broker.clone(), capture.clone())?
    } else {
        Broker::new(config.broker.clone())?
    };
    let started_at = std::time::Instant::now();

    println!("nono-approval is ready");
    println!(
        "  webhook: http://{}{}",
        config.webhook_listen, WEBHOOK_PATH
    );
    println!(
        "  control: {}",
        crate::display::sanitize(&config.control_socket.display().to_string())
    );
    if let Some(capture) = &config.debug_capture
        && let crate::debug_capture::DebugCaptureStatus::Enabled { path } = capture.status()
    {
        println!(
            "  debug capture: {}",
            crate::display::sanitize(&path.display().to_string())
        );
    }

    let mut webhook_task = tokio::spawn(crate::webhook::serve(
        tcp_listener,
        WebhookContext {
            broker: broker.clone(),
            max_body_bytes: config.max_body_bytes,
            max_detail_bytes: config.max_detail_bytes,
        },
    ));
    let mut control_task = tokio::spawn(crate::control::serve(
        unix_listener,
        ControlContext {
            broker: broker.clone(),
            started_at,
            webhook_listen: config.webhook_listen.to_string(),
            max_pending: config.broker.max_pending,
            max_per_session: config.broker.max_per_session,
            debug_capture: config.debug_capture,
        },
    ));

    let outcome = tokio::select! {
        signal = shutdown_signal() => signal.map_err(DaemonError::Io),
        result = &mut webhook_task => Err(DaemonError::WebhookTask(format_task_result(result))),
        result = &mut control_task => Err(DaemonError::ControlTask(format_task_result(result))),
    };
    broker.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    webhook_task.abort();
    control_task.abort();
    drop(socket_guard);
    outcome
}

fn format_task_result(result: Result<io::Result<()>, tokio::task::JoinError>) -> String {
    match result {
        Ok(Ok(())) => "server exited".to_owned(),
        Ok(Err(error)) => error.to_string(),
        Err(error) => error.to_string(),
    }
}

async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
