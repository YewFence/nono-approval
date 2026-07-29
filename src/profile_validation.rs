use std::io::{self, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use thiserror::Error;
use tokio::net::UnixStream;
use tokio::process::Command;

use crate::control::ControlClient;

const PROBE_PREFIX: &str = "nono-approval-probe-v1";

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("nono profile validation timed out")]
    Timeout,
    #[error("nono failed to start the validation probe: {0}")]
    NonoFailed(String),
    #[error("validation probe returned an invalid protocol")]
    InvalidProtocol,
    #[error("control socket was reachable from the sandbox")]
    Reachable,
    #[error("control socket denial was inconclusive: {0}")]
    Inconclusive(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Runs the owner-control isolation probe through the selected nono profile.
///
/// # Errors
///
/// Returns an error unless a started sandbox reports `EACCES` or `EPERM` for the socket connect.
pub async fn validate_profile(profile: &str, control_socket: &Path) -> Result<(), ValidationError> {
    let executable = std::env::current_exe()?;
    let mut command = Command::new("nono");
    command
        .arg("run")
        .arg("--profile")
        .arg(profile)
        .arg("--")
        .arg(executable)
        .arg("__probe-control-socket")
        .arg("--control-socket")
        .arg(control_socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| ValidationError::Timeout)??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let started = stdout
        .lines()
        .any(|line| line == format!("{PROBE_PREFIX} started"));
    if !started {
        return Err(ValidationError::NonoFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let result = stdout
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{PROBE_PREFIX} result ")))
        .ok_or(ValidationError::InvalidProtocol)?;
    match result {
        "denied 1" | "denied 13" => Ok(()),
        "reachable" => Err(ValidationError::Reachable),
        other => Err(ValidationError::Inconclusive(other.to_owned())),
    }
}

/// Executes the hidden in-sandbox control socket probe.
///
/// # Errors
///
/// Returns an error only when writing the probe protocol to stdout fails.
pub async fn run_probe(control_socket: &Path) -> io::Result<()> {
    println!("{PROBE_PREFIX} started");
    io::stdout().flush()?;
    match UnixStream::connect(control_socket).await {
        Ok(stream) => {
            drop(stream);
            let result = if ControlClient::new(control_socket).status().await.is_ok() {
                "reachable".to_owned()
            } else {
                "error http".to_owned()
            };
            println!("{PROBE_PREFIX} result {result}");
        }
        Err(error) => {
            let errno = error.raw_os_error().map_or_else(
                || format!("kind:{:?}", error.kind()),
                |errno| errno.to_string(),
            );
            if matches!(error.raw_os_error(), Some(1 | 13)) {
                println!("{PROBE_PREFIX} result denied {errno}");
            } else {
                println!("{PROBE_PREFIX} result error {errno}");
            }
        }
    }
    Ok(())
}
