use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use getrandom::fill;
use jiff::Timestamp;
use nix::unistd::getuid;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::broker::{ApprovalId, ResponseDeliveryOutcome, TerminalState};
use crate::display::sanitize;
use crate::protocol::IncomingApproval;
use crate::runtime_path::{
    RuntimePathError, ensure_owner_directory, validate_owner_directory, validate_path_components,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DebugCaptureStatus {
    Disabled,
    Enabled { path: PathBuf },
    Failed { error_category: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CaptureInfo {
    pub name: String,
    pub created_at: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct DebugCapture {
    inner: Arc<Mutex<CaptureState>>,
}

#[derive(Debug)]
struct CaptureState {
    path: PathBuf,
    file: Option<File>,
    failure: Option<String>,
}

#[derive(Debug, Error)]
pub enum DebugCaptureError {
    #[error("unsafe entry in managed capture directory: {0}")]
    UnsafeEntry(PathBuf),
    #[error("operating-system randomness failed: {0}")]
    Random(String),
    #[error(transparent)]
    RuntimePath(#[from] RuntimePathError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl DebugCapture {
    /// Creates one new managed owner-only capture file.
    ///
    /// # Errors
    ///
    /// Returns an error when the managed directory is unsafe or the file cannot be created.
    pub fn create(state_dir: &Path) -> Result<Self, DebugCaptureError> {
        let directory = capture_directory(state_dir);
        ensure_owner_directory(&directory)?;
        let mut random = [0_u8; 8];
        fill(&mut random).map_err(|error| DebugCaptureError::Random(error.to_string()))?;
        let random = random
            .iter()
            .fold(String::with_capacity(16), |mut output, byte| {
                let _ = write!(output, "{byte:02x}");
                output
            });
        let timestamp = Timestamp::try_from(SystemTime::now())
            .map_or_else(
                |_| "1970-01-01T00-00-00Z".to_owned(),
                |value| value.to_string(),
            )
            .replace([':', '.'], "-");
        let path = directory.join(format!("{timestamp}-{random}.ndjson"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(CaptureState {
                path,
                file: Some(file),
                failure: None,
            })),
        })
    }

    #[must_use]
    pub fn status(&self) -> DebugCaptureStatus {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failure.as_ref().map_or_else(
            || DebugCaptureStatus::Enabled {
                path: state.path.clone(),
            },
            |error_category| DebugCaptureStatus::Failed {
                error_category: error_category.clone(),
            },
        )
    }

    pub fn record_received(
        &self,
        approval_id: &ApprovalId,
        incoming: &IncomingApproval,
        deadline: &str,
    ) {
        self.append(&json!({
            "schema_version": 1,
            "event": "request_received",
            "approval_id": approval_id,
            "claimed_backend": incoming.claimed_backend,
            "source_kind": incoming.request.source_kind(),
            "wire_request": incoming.request,
            "deadline": deadline,
        }));
    }

    pub fn record_completed(
        &self,
        approval_id: &ApprovalId,
        state: TerminalState,
        source: &str,
        reason: Option<&str>,
        wait_duration: Duration,
        response_delivery_outcome: ResponseDeliveryOutcome,
    ) {
        let reason = reason.map(sanitize);
        self.append(&json!({
            "schema_version": 1,
            "event": "request_completed",
            "approval_id": approval_id,
            "terminal_state": state,
            "decision_source": source,
            "reason": reason,
            "wait_duration_ms": wait_duration.as_millis(),
            "response_delivery_outcome": response_delivery_outcome,
        }));
    }

    fn append(&self, value: &serde_json::Value) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.failure.is_some() {
            return;
        }
        let line = match serde_json::to_vec(value) {
            Ok(line) => line,
            Err(error) => {
                state.file = None;
                state.failure = Some(format!("serialization:{error}"));
                return;
            }
        };
        let result = state.file.as_mut().map_or_else(
            || {
                Err(io::Error::other(
                    "capture file is unavailable without a recorded failure",
                ))
            },
            |file| {
                file.write_all(&line)?;
                file.write_all(b"\n")?;
                file.flush()
            },
        );
        if let Err(error) = result {
            let category = format!("io:{:?}", error.kind());
            state.file = None;
            state.failure = Some(category.clone());
            tracing::error!(error_category = %category, "debug capture failed and has been disabled");
        }
    }
}

#[must_use]
pub fn capture_directory(state_dir: &Path) -> PathBuf {
    state_dir.join("debug-captures")
}

/// Lists metadata for every valid managed capture without reading its contents.
///
/// # Errors
///
/// Returns an error if any entry is a symlink, non-file, wrong-owner, or has an unmanaged name.
pub fn list_captures(state_dir: &Path) -> Result<Vec<CaptureInfo>, DebugCaptureError> {
    let directory = capture_directory(state_dir);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            validate_path_components(&directory)?;
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    validate_owner_directory(&directory)?;
    let mut captures = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_capture_file(&metadata, &name) {
            return Err(DebugCaptureError::UnsafeEntry(path));
        }
        captures.push(CaptureInfo {
            name,
            created_at: Timestamp::try_from(metadata.created().unwrap_or(SystemTime::UNIX_EPOCH))
                .map_or_else(|_| "unknown".to_owned(), |value| value.to_string()),
            size_bytes: metadata.len(),
        });
    }
    captures.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(captures)
}

/// Deletes all valid managed capture files without recursion.
///
/// # Errors
///
/// Returns an error before deletion if any directory entry fails managed-file validation.
pub fn clean_captures(state_dir: &Path) -> Result<usize, DebugCaptureError> {
    let captures = list_captures(state_dir)?;
    let directory = capture_directory(state_dir);
    for capture in &captures {
        let path = directory.join(&capture.name);
        let metadata = fs::symlink_metadata(&path)?;
        if !valid_capture_file(&metadata, &capture.name) {
            return Err(DebugCaptureError::UnsafeEntry(path));
        }
        fs::remove_file(path)?;
    }
    Ok(captures.len())
}

fn valid_capture_file(metadata: &fs::Metadata, name: &str) -> bool {
    !metadata.file_type().is_symlink()
        && metadata.is_file()
        && metadata.uid() == getuid().as_raw()
        && metadata.permissions().mode() & 0o777 == 0o600
        && managed_name(name)
}

fn managed_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".ndjson") else {
        return false;
    };
    let Some((timestamp, random)) = stem.rsplit_once('-') else {
        return false;
    };
    !timestamp.is_empty()
        && timestamp
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'T' | b'Z' | b'-' | b'+'))
        && random.len() == 16
        && random
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        DebugCapture, DebugCaptureError, DebugCaptureStatus, clean_captures, list_captures,
    };
    use crate::broker::{ApprovalId, ResponseDeliveryOutcome, TerminalState};
    use crate::protocol::parse_default_webhook_body;

    #[test]
    fn creates_lists_and_cleans_managed_capture() {
        let temporary = tempdir().unwrap();
        let capture = DebugCapture::create(temporary.path()).unwrap();
        drop(capture);
        assert_eq!(list_captures(temporary.path()).unwrap().len(), 1);
        assert_eq!(clean_captures(temporary.path()).unwrap(), 1);
    }

    #[test]
    fn refuses_unmanaged_entries() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("debug-captures");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(directory.join("notes.txt"), "unsafe").unwrap();
        assert!(matches!(
            list_captures(temporary.path()),
            Err(DebugCaptureError::UnsafeEntry(_))
        ));
    }

    #[test]
    fn records_sanitized_completion_with_explicit_delivery_outcome() {
        let temporary = tempdir().unwrap();
        let capture = DebugCapture::create(temporary.path()).unwrap();
        let path = match capture.status() {
            DebugCaptureStatus::Enabled { path } => path,
            status => panic!("unexpected capture status: {status:?}"),
        };
        let incoming = parse_default_webhook_body(
            br#"{"backend":"local","request":{"capability_type":"command","request_id":"r1","command":"date","args":["date"],"caller":"session","intercept_rule":"approve","reason":null,"child_pid":1,"session_id":"s1"}}"#,
        )
        .unwrap();
        let approval_id: ApprovalId = "appr_0123456789abcdef".parse().unwrap();
        capture.record_received(&approval_id, &incoming, "2026-07-29T00:00:00Z");
        capture.record_completed(
            &approval_id,
            TerminalState::Denied,
            "control",
            Some("line\nnext"),
            Duration::from_millis(25),
            ResponseDeliveryOutcome::NotObserved,
        );
        drop(capture);

        let records = fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["event"], "request_received");
        assert_eq!(records[1]["reason"], "line\\nnext");
        assert_eq!(records[1]["response_delivery_outcome"], "not_observed");
    }
}
