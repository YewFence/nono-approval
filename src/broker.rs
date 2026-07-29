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

use crate::display::{ApprovalDetailContent, sanitize};
use crate::protocol::{IncomingApproval, WIRE_ADAPTER_VERSION, WebhookDecision};

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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletedApproval {
    pub approval_id: ApprovalId,
    pub state: TerminalState,
    pub completed_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowApproval {
    Pending(ApprovalDetail),
    Completed(CompletedApproval),
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
    if reason.len() > MAX_DENIAL_REASON_BYTES {
        return Err(BrokerError::DenialReasonTooLarge);
    }
    Ok(())
}

struct PendingApproval {
    approval_id: ApprovalId,
    session_id: String,
    request_id: String,
    capability_type: String,
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
    replay_key: (String, String),
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
}

pub struct Submission {
    pub approval_id: ApprovalId,
    deadline: Instant,
    receiver: oneshot::Receiver<WebhookDecision>,
    broker: Broker,
}

impl Submission {
    pub async fn wait(self) -> WebhookDecision {
        tokio::select! {
            decision = self.receiver => decision.unwrap_or_else(|_| WebhookDecision::Denied {
                reason: "approval daemon stopped".to_owned(),
            }),
            () = tokio::time::sleep_until(self.deadline) => {
                self.broker.expire(&self.approval_id).await.unwrap_or(WebhookDecision::Denied {
                    reason: "approval request expired".to_owned(),
                })
            }
        }
    }
}

impl Broker {
    #[must_use]
    pub fn new(config: BrokerConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(BrokerState::default())),
        }
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
        let (decision_tx, receiver) = oneshot::channel();
        state.pending.insert(
            approval_id.clone(),
            PendingApproval {
                approval_id: approval_id.clone(),
                session_id: replay_key.0,
                request_id: replay_key.1,
                capability_type: incoming.request.capability_type().to_owned(),
                detail: incoming.detail,
                received_at,
                deadline_wall,
                deadline,
                decision_tx: Some(decision_tx),
            },
        );
        Ok(Submission {
            approval_id,
            deadline,
            receiver,
            broker: self.clone(),
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
            return Ok(ShowApproval::Pending(ApprovalDetail {
                approval_id: pending.approval_id.clone(),
                received_at: timestamp(pending.received_at),
                deadline: timestamp(pending.deadline_wall),
                content: pending.detail.clone(),
            }));
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
        self.complete(&mut broker_state, pending, state, Some(decision));
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
        self.complete(&mut state, pending, TerminalState::Cancelled, None);
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
    ) {
        let completed_at = SystemTime::now();
        let replay_key = (pending.session_id.clone(), pending.request_id.clone());
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
            wait_duration: completed_at
                .duration_since(pending.received_at)
                .unwrap_or_default(),
            replay_key,
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
                && !tombstone.replay_key.0.is_empty()
                && tombstone.wire_adapter_version == WIRE_ADAPTER_VERSION
        }));
    }
}

fn timestamp(time: SystemTime) -> String {
    Timestamp::try_from(time).map_or_else(
        |_| "1970-01-01T00:00:00Z".to_owned(),
        |value| value.to_string(),
    )
}

#[must_use]
pub fn sanitized_denial_reason(reason: &str) -> String {
    sanitize(reason)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ApprovalId, Broker, BrokerConfig, BrokerError, ShowApproval, TerminalState};
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

    #[tokio::test]
    async fn grants_once_and_destroys_detail() {
        let broker = Broker::new(BrokerConfig::default());
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
        });
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
        });
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
