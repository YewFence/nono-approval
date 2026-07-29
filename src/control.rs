use std::convert::Infallible;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::http1 as client_http1;
use hyper::header::CONTENT_TYPE;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};

use crate::broker::{
    ApprovalDetail, ApprovalId, ApprovalSummary, Broker, BrokerError, CompletedApproval,
    ShowApproval, TerminalState,
};
use crate::peer_identity::verify_owner;
use crate::protocol::WebhookDecision;

const MAX_CONTROL_BODY_BYTES: usize = 8 * 1024;

#[derive(Clone)]
pub struct ControlContext {
    pub broker: Broker,
    pub started_at: Instant,
    pub webhook_listen: String,
    pub max_pending: usize,
    pub max_per_session: usize,
    pub debug_capture: DebugCaptureStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DebugCaptureStatus {
    Disabled,
    Enabled { path: PathBuf },
    Failed { error_category: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub uptime_seconds: u64,
    pub pending: usize,
    pub max_pending: usize,
    pub max_per_session: usize,
    pub webhook_listen: String,
    pub debug_capture: DebugCaptureStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalList {
    pub approvals: Vec<ApprovalSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalView {
    Pending(ApprovalDetail),
    Completed(CompletedApproval),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum DecisionRequest {
    Granted,
    Denied { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    pub approval_id: ApprovalId,
    pub state: TerminalState,
}

#[derive(Debug, Error)]
pub enum ControlClientError {
    #[error("could not connect to approval daemon: {0}")]
    Connect(io::Error),
    #[error("control HTTP transport failed: {0}")]
    Transport(hyper::Error),
    #[error("control request failed with HTTP {status}: {message}")]
    Response { status: StatusCode, message: String },
    #[error("invalid control response: {0}")]
    InvalidResponse(serde_json::Error),
    #[error("could not build control request: {0}")]
    BuildRequest(hyper::http::Error),
}

#[derive(Clone, Debug)]
pub struct ControlClient {
    socket_path: PathBuf,
}

impl ControlClient {
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    /// Queries daemon status.
    ///
    /// # Errors
    ///
    /// Returns an error for connection, HTTP, or response decoding failures.
    pub async fn status(&self) -> Result<DaemonStatus, ControlClientError> {
        self.request(Method::GET, "/v1/status", None::<&()>).await
    }

    /// Lists pending approvals in FIFO order.
    ///
    /// # Errors
    ///
    /// Returns an error for connection, HTTP, or response decoding failures.
    pub async fn list(&self) -> Result<ApprovalList, ControlClientError> {
        self.request(Method::GET, "/v1/approvals", None::<&()>)
            .await
    }

    /// Fetches one exact approval view.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid ID, connection failure, or non-success response.
    pub async fn show(
        &self,
        approval_id: &ApprovalId,
        debug: bool,
    ) -> Result<ApprovalView, ControlClientError> {
        let suffix = if debug { "?debug=true" } else { "" };
        self.request(
            Method::GET,
            &format!("/v1/approvals/{approval_id}{suffix}"),
            None::<&()>,
        )
        .await
    }

    /// Applies one exact decision.
    ///
    /// # Errors
    ///
    /// Returns an error for connection failures, invalid reasons, unknown IDs, or conflicts.
    pub async fn decide(
        &self,
        approval_id: &ApprovalId,
        decision: &DecisionRequest,
    ) -> Result<DecisionResponse, ControlClientError> {
        self.request(
            Method::POST,
            &format!("/v1/approvals/{approval_id}/decision"),
            Some(decision),
        )
        .await
    }

    async fn request<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ControlClientError> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(ControlClientError::Connect)?;
        let (mut sender, connection) = client_http1::handshake(TokioIo::new(stream))
            .await
            .map_err(ControlClientError::Transport)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let body = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).unwrap_or_default()
        });
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(ControlClientError::BuildRequest)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(ControlClientError::Transport)?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(ControlClientError::Transport)?
            .to_bytes();
        if !status.is_success() {
            let message = serde_json::from_slice::<ErrorBody>(&bytes).map_or_else(
                |_| String::from_utf8_lossy(&bytes).into_owned(),
                |body| body.error,
            );
            return Err(ControlClientError::Response { status, message });
        }
        serde_json::from_slice(&bytes).map_err(ControlClientError::InvalidResponse)
    }
}

/// Serves owner-authenticated control connections until cancelled.
///
/// # Errors
///
/// Returns an error if accepting a Unix connection fails.
pub async fn serve(listener: UnixListener, context: ControlContext) -> io::Result<()> {
    let context = Arc::new(context);
    loop {
        let (stream, _) = listener.accept().await?;
        if verify_owner(&stream).is_err() {
            continue;
        }
        let context = Arc::clone(&context);
        tokio::spawn(async move {
            let service = service_fn(move |request| handle(request, Arc::clone(&context)));
            let _ = server_http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

async fn handle(
    request: Request<Incoming>,
    context: Arc<ControlContext>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if method == Method::GET && path == "/v1/status" {
        let status = DaemonStatus {
            version: crate::VERSION.to_owned(),
            uptime_seconds: context.started_at.elapsed().as_secs(),
            pending: context.broker.pending_count().await,
            max_pending: context.max_pending,
            max_per_session: context.max_per_session,
            webhook_listen: context.webhook_listen.clone(),
            debug_capture: context.debug_capture.clone(),
        };
        return Ok(json_response(StatusCode::OK, &status));
    }
    if method == Method::GET && path == "/v1/approvals" {
        return Ok(json_response(
            StatusCode::OK,
            &ApprovalList {
                approvals: context.broker.list().await,
            },
        ));
    }
    let Some(remainder) = path.strip_prefix("/v1/approvals/") else {
        return Ok(empty_response(StatusCode::NOT_FOUND));
    };
    Ok(handle_approval(request, context, remainder).await)
}

async fn handle_approval(
    request: Request<Incoming>,
    context: Arc<ControlContext>,
    remainder: &str,
) -> Response<Full<Bytes>> {
    let method = request.method().clone();
    if method == Method::GET && !remainder.contains('/') {
        let Ok(approval_id) = remainder.parse::<ApprovalId>() else {
            return error_response(StatusCode::BAD_REQUEST, "invalid approval ID");
        };
        return show_response(
            &context.broker,
            &approval_id,
            request.uri().query() == Some("debug=true"),
        )
        .await;
    }
    if method == Method::POST {
        let Some(id) = remainder.strip_suffix("/decision") else {
            return empty_response(StatusCode::NOT_FOUND);
        };
        let Ok(approval_id) = id.parse::<ApprovalId>() else {
            return error_response(StatusCode::BAD_REQUEST, "invalid approval ID");
        };
        let body = match request.into_body().collect().await {
            Ok(body) => body.to_bytes(),
            Err(_) => {
                return error_response(StatusCode::BAD_REQUEST, "invalid decision body");
            }
        };
        if body.len() > MAX_CONTROL_BODY_BYTES {
            return error_response(StatusCode::BAD_REQUEST, "invalid decision body");
        }
        let Ok(request) = serde_json::from_slice::<DecisionRequest>(&body) else {
            return error_response(StatusCode::BAD_REQUEST, "invalid decision body");
        };
        let decision = match request {
            DecisionRequest::Granted => WebhookDecision::Granted,
            DecisionRequest::Denied { reason } => WebhookDecision::Denied { reason },
        };
        return decision_response(&context.broker, approval_id, decision).await;
    }
    empty_response(StatusCode::NOT_FOUND)
}

async fn show_response(
    broker: &Broker,
    approval_id: &ApprovalId,
    debug: bool,
) -> Response<Full<Bytes>> {
    match broker.show(approval_id).await {
        Ok(ShowApproval::Pending(mut detail)) => {
            if !debug {
                detail.content.debug_fields.clear();
            }
            json_response(StatusCode::OK, &ApprovalView::Pending(detail))
        }
        Ok(ShowApproval::Completed(completed)) => {
            json_response(StatusCode::OK, &ApprovalView::Completed(completed))
        }
        Err(_) => error_response(StatusCode::NOT_FOUND, "approval not found"),
    }
}

async fn decision_response(
    broker: &Broker,
    approval_id: ApprovalId,
    decision: WebhookDecision,
) -> Response<Full<Bytes>> {
    match broker.decide(&approval_id, decision).await {
        Ok(state) => json_response(StatusCode::OK, &DecisionResponse { approval_id, state }),
        Err(BrokerError::NotPending) => {
            error_response(StatusCode::CONFLICT, "approval is no longer pending")
        }
        Err(BrokerError::NotFound) => error_response(StatusCode::NOT_FOUND, "approval not found"),
        Err(BrokerError::EmptyDenialReason | BrokerError::DenialReasonTooLarge) => {
            error_response(StatusCode::BAD_REQUEST, "invalid denial reason")
        }
        Err(_) => error_response(StatusCode::BAD_REQUEST, "decision rejected"),
    }
}

#[derive(Serialize, Deserialize)]
struct ErrorBody {
    error: String,
}

fn error_response(status: StatusCode, error: &str) -> Response<Full<Bytes>> {
    json_response(
        status,
        &ErrorBody {
            error: error.to_owned(),
        },
    )
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

#[must_use]
pub fn socket_path(client: &ControlClient) -> &Path {
    &client.socket_path
}
