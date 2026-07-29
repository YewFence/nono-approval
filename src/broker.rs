use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use getrandom::fill;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, oneshot};
use tokio::time::Instant;

use crate::debug_capture::DebugCapture;
use crate::display::{ApprovalDetailContent, sanitize, truncate_summary};
use crate::protocol::{
    IncomingApproval, KnownApprovalRequest, SourceKind, WIRE_ADAPTER_VERSION, WebhookDecision,
};

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(270);
pub const DEFAULT_MAX_PENDING: usize = 64;
pub const DEFAULT_MAX_PER_SESSION: usize = 8;
pub const DEFAULT_TOMBSTONE_LIMIT: usize = 1024;
pub const DEFAULT_TOMBSTONE_TTL: Duration = Duration::from_mins(10);
pub const DEFAULT_DENIAL_REASON: &str = "denied by local user";
pub const MAX_DENIAL_REASON_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug)]
pub struct BrokerConfig {
    pub request_timeout: Duration,
    pub max_pending: usize,
    pub max_per_session: usize,
    pub tombstone_limit: usize,
    pub tombstone_ttl: Duration,
}

impl Default for BrokerConfig {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_pending: DEFAULT_MAX_PENDING,
            max_per_session: DEFAULT_MAX_PER_SESSION,
            tombstone_limit: DEFAULT_TOMBSTONE_LIMIT,
            tombstone_ttl: DEFAULT_TOMBSTONE_TTL,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApprovalId(String);

impl ApprovalId {
    fn random() -> Result<Self, BrokerError> {
        let mut bytes = [0_u8; 8];
        fill(&mut bytes).map_err(|error| BrokerError::Random(error.to_string()))?;
        let mut value = String::with_capacity(21);
        value.push_str("appr_");
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ApprovalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ApprovalId {
    type Err = BrokerError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = value.len() == 21
            && value.starts_with("appr_")
            && value[5..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(BrokerError::InvalidApprovalId)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Granted,
    Denied,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalSummary {
    pub approval_id: ApprovalId,
    pub capability_type: String,
    pub summary: String,
    pub received_at: String,
    pub deadline: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDetail {
    pub approval_id: ApprovalId,
    pub received_at: String,
    pub deadline: String,
    #[serde(flatten)]
    pub content: ApprovalDetailContent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<ApprovalDebugMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDebugMetadata {
    pub claimed_backend: String,
    pub source_kind: SourceKind,
    pub wire_request: KnownApprovalRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletedApproval {
    pub approval_id: ApprovalId,
    pub state: TerminalState,
    pub completed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowApproval {
    Pending(Box<ApprovalDetail>),
    Completed(CompletedApproval),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseDeliveryOutcome {
    NotObserved,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BrokerError {
    #[error("approval ID must be appr_ followed by 16 lowercase hexadecimal characters")]
    InvalidApprovalId,
    #[error("approval request is already active or was recently completed")]
    DuplicateRequest,
    #[error("per-session pending approval limit reached")]
    PerSessionCapacity,
    #[error("global pending approval limit reached")]
    GlobalCapacity,
    #[error("approval not found")]
    NotFound,
    #[error("approval is no longer pending")]
    NotPending,
    #[error("denial reason must not be empty")]
    EmptyDenialReason,
    #[error("denial reason must not consist only of NUL characters")]
    NullDenialReason,
    #[error("denial reason exceeds {MAX_DENIAL_REASON_BYTES} bytes")]
    DenialReasonTooLarge,
    #[error("operating-system randomness failed: {0}")]
    Random(String),
}

/// Validates a user-supplied denial reason.
///
/// # Errors
///
/// Returns an error when the reason is empty or larger than the protocol limit.
pub fn validate_denial_reason(reason: &str) -> Result<(), BrokerError> {
    if reason.is_empty() {
        return Err(BrokerError::EmptyDenialReason);
    }
    if reason.chars().all(|character| character == '\0') {
        return Err(BrokerError::NullDenialReason);
    }
    if reason.len() > MAX_DENIAL_REASON_BYTES {
        return Err(BrokerError::DenialReasonTooLarge);
    }
    Ok(())
}

struct PendingApproval {
    approval_id: ApprovalId,
    claimed_backend: String,
    session_id: String,
    request_id: String,
    capability_type: String,
    wire_request: KnownApprovalRequest,
    raw_request: Box<serde_json::value::RawValue>,
    detail: ApprovalDetailContent,
    received_at: SystemTime,
    deadline_wall: SystemTime,
    deadline: Instant,
    decision_tx: Option<oneshot::Sender<WebhookDecision>>,
}

struct Tombstone {
    approval_id: ApprovalId,
    capability_type: String,
    state: TerminalState,
    received_at: SystemTime,
    completed_at: SystemTime,
    wait_duration: Duration,
    response_delivery_outcome: ResponseDeliveryOutcome,
    identity_hash: blake3::Hash,
    wire_adapter_version: u32,
}

#[derive(Default)]
struct BrokerState {
    pending: HashMap<ApprovalId, PendingApproval>,
    replay: HashMap<(String, String), Instant>,
    tombstones: VecDeque<Tombstone>,
}

#[derive(Clone)]
pub struct Broker {
    config: BrokerConfig,
    state: Arc<Mutex<BrokerState>>,
    debug_capture: Option<DebugCapture>,
    hash_key: [u8; 32],
}

pub struct Submission {
    pub approval_id: ApprovalId,
    deadline: Instant,
    receiver: Option<oneshot::Receiver<WebhookDecision>>,
    broker: Broker,
    active: bool,
}

impl Submission {
    pub async fn wait(mut self) -> WebhookDecision {
        let Some(receiver) = self.receiver.take() else {
            self.active = false;
            return WebhookDecision::Denied {
                reason: "approval submission is unavailable".to_owned(),
            };
        };
        let decision = tokio::select! {
            decision = receiver => decision.unwrap_or_else(|_| WebhookDecision::Denied {
                reason: "approval daemon stopped".to_owned(),
            }),
            () = tokio::time::sleep_until(self.deadline) => {
                self.broker.expire(&self.approval_id).await.unwrap_or(WebhookDecision::Denied {
                    reason: "approval request expired".to_owned(),
                })
            }
        };
        self.active = false;
        decision
    }
}

impl Drop for Submission {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let approval_id = self.approval_id.clone();
        let broker = self.broker.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = broker.cancel(&approval_id).await;
            });
        }
    }
}

impl Broker {
    /// Creates an in-memory broker with a fresh process-lifetime hash key.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating-system random source is unavailable.
    pub fn new(config: BrokerConfig) -> Result<Self, BrokerError> {
        let mut hash_key = [0_u8; 32];
        fill(&mut hash_key).map_err(|error| BrokerError::Random(error.to_string()))?;
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(BrokerState::default())),
            debug_capture: None,
            hash_key,
        })
    }

    /// Creates a broker that writes explicit managed debug-capture events.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating-system random source is unavailable.
    pub fn with_debug_capture(
        config: BrokerConfig,
        debug_capture: DebugCapture,
    ) -> Result<Self, BrokerError> {
        let mut hash_key = [0_u8; 32];
        fill(&mut hash_key).map_err(|error| BrokerError::Random(error.to_string()))?;
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(BrokerState::default())),
            debug_capture: Some(debug_capture),
            hash_key,
        })
    }

    /// Registers a validated incoming approval request.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicates, exhausted capacity, or unavailable OS randomness.
    pub async fn submit(&self, incoming: IncomingApproval) -> Result<Submission, BrokerError> {
        let mut state = self.state.lock().await;
        self.prune(&mut state);
        let replay_key = (
            incoming.request.session_id().to_owned(),
            incoming.request.request_id().to_owned(),
        );
        if state.replay.contains_key(&replay_key)
            || state.pending.values().any(|pending| {
                pending.session_id == replay_key.0 && pending.request_id == replay_key.1
            })
        {
            return Err(BrokerError::DuplicateRequest);
        }
        let session_count = state
            .pending
            .values()
            .filter(|pending| pending.session_id == replay_key.0)
            .count();
        if session_count >= self.config.max_per_session {
            return Err(BrokerError::PerSessionCapacity);
        }
        if state.pending.len() >= self.config.max_pending {
            return Err(BrokerError::GlobalCapacity);
        }

        let approval_id = loop {
            let candidate = ApprovalId::random()?;
            let collision = state.pending.contains_key(&candidate)
                || state
                    .tombstones
                    .iter()
                    .any(|tombstone| tombstone.approval_id == candidate);
            if !collision {
                break candidate;
            }
        };
        let received_at = SystemTime::now();
        let deadline_wall = received_at + self.config.request_timeout;
        let deadline = Instant::now() + self.config.request_timeout;
        if let Some(debug_capture) = &self.debug_capture {
            debug_capture.record_received(&approval_id, &incoming, &timestamp(deadline_wall));
        }
        let IncomingApproval {
            claimed_backend,
            raw_request,
            request,
            detail,
        } = incoming;
        let capability_type = request.capability_type().to_owned();
        let session_log = short_session_id(&replay_key.0);
        let (decision_tx, receiver) = oneshot::channel();
        state.pending.insert(
            approval_id.clone(),
            PendingApproval {
                approval_id: approval_id.clone(),
                claimed_backend,
                session_id: replay_key.0,
                request_id: replay_key.1,
                capability_type: capability_type.clone(),
                wire_request: request,
                raw_request,
                detail,
                received_at,
                deadline_wall,
                deadline,
                decision_tx: Some(decision_tx),
            },
        );
        tracing::info!(
            approval_id = %approval_id,
            capability_type = %capability_type,
            session = %session_log,
            "approval received"
        );
        Ok(Submission {
            approval_id,
            deadline,
            receiver: Some(receiver),
            broker: self.clone(),
            active: true,
        })
    }

    pub async fn list(&self) -> Vec<ApprovalSummary> {
        let mut state = self.state.lock().await;
        self.expire_elapsed(&mut state);
        self.prune(&mut state);
        let mut approvals = state
            .pending
            .values()
            .map(|pending| ApprovalSummary {
                approval_id: pending.approval_id.clone(),
                capability_type: pending.capability_type.clone(),
                summary: pending.detail.summary.clone(),
                received_at: timestamp(pending.received_at),
                deadline: timestamp(pending.deadline_wall),
            })
            .collect::<Vec<_>>();
        approvals.sort_by(|left, right| {
            left.received_at
                .cmp(&right.received_at)
                .then_with(|| left.approval_id.cmp(&right.approval_id))
        });
        approvals
    }

    /// Returns a pending approval detail or its minimal recent tombstone.
    ///
    /// # Errors
    ///
    /// Returns [`BrokerError::NotFound`] when the ID is unknown or its tombstone expired.
    pub async fn show(&self, approval_id: &ApprovalId) -> Result<ShowApproval, BrokerError> {
        let mut state = self.state.lock().await;
        self.expire_elapsed(&mut state);
        self.prune(&mut state);
        if let Some(pending) = state.pending.get(approval_id) {
            return Ok(ShowApproval::Pending(Box::new(ApprovalDetail {
                approval_id: pending.approval_id.clone(),
                received_at: timestamp(pending.received_at),
                deadline: timestamp(pending.deadline_wall),
                content: pending.detail.clone(),
                debug: Some(ApprovalDebugMetadata {
                    claimed_backend: pending.claimed_backend.clone(),
                    source_kind: pending.wire_request.source_kind(),
                    wire_request: pending.wire_request.clone(),
                }),
            })));
        }
        state
            .tombstones
            .iter()
            .find(|tombstone| &tombstone.approval_id == approval_id)
            .map(|tombstone| {
                ShowApproval::Completed(CompletedApproval {
                    approval_id: tombstone.approval_id.clone(),
                    state: tombstone.state,
                    completed_at: timestamp(tombstone.completed_at),
                })
            })
            .ok_or(BrokerError::NotFound)
    }

    /// Applies one exact decision to one pending approval.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid reason, unknown ID, or approval already in a terminal state.
    pub async fn decide(
        &self,
        approval_id: &ApprovalId,
        decision: WebhookDecision,
    ) -> Result<TerminalState, BrokerError> {
        if let WebhookDecision::Denied { reason } = &decision {
            validate_denial_reason(reason)?;
        }
        let state = match decision {
            WebhookDecision::Granted => TerminalState::Granted,
            WebhookDecision::Denied { .. } => TerminalState::Denied,
        };
        let mut broker_state = self.state.lock().await;
        self.expire_elapsed(&mut broker_state);
        self.prune(&mut broker_state);
        let pending = broker_state.pending.remove(approval_id).ok_or_else(|| {
            if broker_state
                .tombstones
                .iter()
                .any(|tombstone| &tombstone.approval_id == approval_id)
            {
                BrokerError::NotPending
            } else {
                BrokerError::NotFound
            }
        })?;
        self.complete(&mut broker_state, pending, state, Some(decision), "control");
        Ok(state)
    }

    /// Best-effort cancellation for a disconnected webhook handler.
    ///
    /// # Errors
    ///
    /// Returns [`BrokerError::NotFound`] when the request is no longer pending.
    pub async fn cancel(&self, approval_id: &ApprovalId) -> Result<(), BrokerError> {
        let mut state = self.state.lock().await;
        let pending = state
            .pending
            .remove(approval_id)
            .ok_or(BrokerError::NotFound)?;
        self.complete(
            &mut state,
            pending,
            TerminalState::Cancelled,
            None,
            "disconnect",
        );
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        let pending = state
            .pending
            .drain()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        for pending in pending {
            self.complete(
                &mut state,
                pending,
                TerminalState::Denied,
                Some(WebhookDecision::Denied {
                    reason: "approval daemon is shutting down".to_owned(),
                }),
                "shutdown",
            );
        }
    }

    pub async fn pending_count(&self) -> usize {
        self.list().await.len()
    }

    async fn expire(&self, approval_id: &ApprovalId) -> Option<WebhookDecision> {
        let mut state = self.state.lock().await;
        let pending = state.pending.remove(approval_id)?;
        let decision = WebhookDecision::Denied {
            reason: "approval request expired".to_owned(),
        };
        self.complete(
            &mut state,
            pending,
            TerminalState::Expired,
            Some(decision.clone()),
            "lease_expired",
        );
        Some(decision)
    }

    fn expire_elapsed(&self, state: &mut BrokerState) {
        let now = Instant::now();
        let expired = state
            .pending
            .iter()
            .filter_map(|(id, pending)| (pending.deadline <= now).then_some(id.clone()))
            .collect::<Vec<_>>();
        for approval_id in expired {
            if let Some(pending) = state.pending.remove(&approval_id) {
                self.complete(
                    state,
                    pending,
                    TerminalState::Expired,
                    Some(WebhookDecision::Denied {
                        reason: "approval request expired".to_owned(),
                    }),
                    "lease_expired",
                );
            }
        }
    }

    fn complete(
        &self,
        state: &mut BrokerState,
        mut pending: PendingApproval,
        terminal_state: TerminalState,
        decision: Option<WebhookDecision>,
        source: &str,
    ) {
        let completed_at = SystemTime::now();
        let wait_duration = completed_at
            .duration_since(pending.received_at)
            .unwrap_or_default();
        let reason = match &decision {
            Some(WebhookDecision::Denied { reason }) => Some(reason.as_str()),
            Some(WebhookDecision::Granted) | None => None,
        };
        if let Some(debug_capture) = &self.debug_capture {
            debug_capture.record_completed(
                &pending.approval_id,
                terminal_state,
                source,
                reason,
                wait_duration,
                ResponseDeliveryOutcome::NotObserved,
            );
        }
        tracing::info!(
            approval_id = %pending.approval_id,
            capability_type = %pending.capability_type,
            session = %short_session_id(&pending.session_id),
            state = ?terminal_state,
            wait_ms = wait_duration.as_millis(),
            "approval completed"
        );
        let replay_key = (pending.session_id.clone(), pending.request_id.clone());
        let mut hasher = blake3::Hasher::new_keyed(&self.hash_key);
        for value in [
            pending.claimed_backend.as_bytes(),
            pending.session_id.as_bytes(),
            pending.request_id.as_bytes(),
        ] {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        let identity_hash = hasher.finalize();
        state.replay.insert(
            replay_key.clone(),
            Instant::now() + self.config.tombstone_ttl,
        );
        if let (Some(sender), Some(decision)) = (pending.decision_tx.take(), decision) {
            let _ = sender.send(decision);
        }
        state.tombstones.push_back(Tombstone {
            approval_id: pending.approval_id,
            capability_type: pending.capability_type,
            state: terminal_state,
            received_at: pending.received_at,
            completed_at,
            wait_duration,
            response_delivery_outcome: ResponseDeliveryOutcome::NotObserved,
            identity_hash,
            wire_adapter_version: WIRE_ADAPTER_VERSION,
        });
        self.prune(state);
    }

    fn prune(&self, state: &mut BrokerState) {
        let now = Instant::now();
        state.replay.retain(|_, deadline| *deadline > now);
        while state.tombstones.len() > self.config.tombstone_limit {
            state.tombstones.pop_front();
        }
        let cutoff = SystemTime::now()
            .checked_sub(self.config.tombstone_ttl)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        while state
            .tombstones
            .front()
            .is_some_and(|tombstone| tombstone.completed_at < cutoff)
        {
            state.tombstones.pop_front();
        }
        debug_assert!(state.tombstones.iter().all(|tombstone| {
            !tombstone.capability_type.is_empty()
                && tombstone.received_at <= tombstone.completed_at
                && tombstone.wait_duration <= self.config.tombstone_ttl + DEFAULT_REQUEST_TIMEOUT
                && tombstone.response_delivery_outcome == ResponseDeliveryOutcome::NotObserved
                && tombstone.identity_hash != blake3::Hash::from_bytes([0; 32])
                && tombstone.wire_adapter_version == WIRE_ADAPTER_VERSION
        }));
        debug_assert!(
            state
                .pending
                .values()
                .all(|pending| !pending.raw_request.get().is_empty())
        );
    }
}

fn timestamp(time: SystemTime) -> String {
    Timestamp::try_from(time).map_or_else(
        |_| "1970-01-01T00:00:00Z".to_owned(),
        |value| value.to_string(),
    )
}

fn short_session_id(session_id: &str) -> String {
    truncate_summary(&sanitize(session_id), 12)
}

#[must_use]
pub fn sanitized_denial_reason(reason: &str) -> String {
    sanitize(reason)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ApprovalId, Broker, BrokerConfig, BrokerError, MAX_DENIAL_REASON_BYTES, ShowApproval,
        TerminalState, validate_denial_reason,
    };
    use crate::protocol::{WebhookDecision, parse_default_webhook_body};

    fn incoming(request_id: &str, session_id: &str) -> crate::protocol::IncomingApproval {
        let json = format!(
            r#"{{"backend":"local","request":{{"capability_type":"command","request_id":"{request_id}","command":"date","args":["date"],"caller":"session","intercept_rule":"approve","reason":null,"child_pid":1,"session_id":"{session_id}"}}}}"#
        );
        parse_default_webhook_body(json.as_bytes()).unwrap()
    }

    #[test]
    fn validates_exact_approval_ids() {
        assert!("appr_0123456789abcdef".parse::<ApprovalId>().is_ok());
        assert!("0123456789abcdef".parse::<ApprovalId>().is_err());
        assert!("appr_0123456789ABCDEF".parse::<ApprovalId>().is_err());
        assert!("appr_0123".parse::<ApprovalId>().is_err());
    }

    #[test]
    fn validates_denial_reason_boundaries() {
        assert_eq!(
            validate_denial_reason(""),
            Err(BrokerError::EmptyDenialReason)
        );
        assert_eq!(
            validate_denial_reason("\0\0"),
            Err(BrokerError::NullDenialReason)
        );
        assert!(validate_denial_reason(&"a".repeat(MAX_DENIAL_REASON_BYTES)).is_ok());
        assert_eq!(
            validate_denial_reason(&"a".repeat(MAX_DENIAL_REASON_BYTES + 1)),
            Err(BrokerError::DenialReasonTooLarge)
        );
    }

    #[tokio::test]
    async fn grants_once_and_destroys_detail() {
        let broker = Broker::new(BrokerConfig::default()).unwrap();
        let submission = broker.submit(incoming("r1", "s1")).await.unwrap();
        let id = submission.approval_id.clone();
        assert!(matches!(
            broker.show(&id).await.unwrap(),
            ShowApproval::Pending(_)
        ));
        broker.decide(&id, WebhookDecision::Granted).await.unwrap();
        assert_eq!(submission.wait().await, WebhookDecision::Granted);
        assert_eq!(
            broker
                .decide(&id, WebhookDecision::Granted)
                .await
                .unwrap_err(),
            BrokerError::NotPending
        );
        assert!(matches!(
            broker.show(&id).await.unwrap(),
            ShowApproval::Completed(completed) if completed.state == TerminalState::Granted
        ));
    }

    #[tokio::test]
    async fn expires_using_the_daemon_lease() {
        let broker = Broker::new(BrokerConfig {
            request_timeout: Duration::from_millis(10),
            ..BrokerConfig::default()
        })
        .unwrap();
        let submission = broker.submit(incoming("r1", "s1")).await.unwrap();
        assert_eq!(
            submission.wait().await,
            WebhookDecision::Denied {
                reason: "approval request expired".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn enforces_duplicate_and_capacity_limits() {
        let broker = Broker::new(BrokerConfig {
            max_pending: 2,
            max_per_session: 1,
            ..BrokerConfig::default()
        })
        .unwrap();
        broker.submit(incoming("r1", "s1")).await.unwrap();
        assert!(matches!(
            broker.submit(incoming("r1", "s1")).await,
            Err(BrokerError::DuplicateRequest)
        ));
        assert!(matches!(
            broker.submit(incoming("r2", "s1")).await,
            Err(BrokerError::PerSessionCapacity)
        ));
        broker.submit(incoming("r2", "s2")).await.unwrap();
        assert!(matches!(
            broker.submit(incoming("r3", "s3")).await,
            Err(BrokerError::GlobalCapacity)
        ));
    }
}
