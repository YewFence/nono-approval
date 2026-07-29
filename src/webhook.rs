use std::convert::Infallible;
use std::io;
use std::sync::Arc;

use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::TcpListener;

use crate::broker::{Broker, BrokerError};
use crate::protocol::{ProtocolError, parse_webhook_body};

pub const WEBHOOK_PATH: &str = "/v1/webhooks/approval";

#[derive(Clone)]
pub struct WebhookContext {
    pub broker: Broker,
    pub max_body_bytes: usize,
    pub max_detail_bytes: usize,
}

/// Serves webhook HTTP connections until the task is cancelled.
///
/// # Errors
///
/// Returns an error if accepting a TCP connection fails.
pub async fn serve(listener: TcpListener, context: WebhookContext) -> io::Result<()> {
    let context = Arc::new(context);
    loop {
        let (stream, _) = listener.accept().await?;
        let context = Arc::clone(&context);
        tokio::spawn(async move {
            let service = service_fn(move |request| handle(request, Arc::clone(&context)));
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

async fn handle(
    request: Request<Incoming>,
    context: Arc<WebhookContext>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if request.method() != Method::POST {
        return Ok(logged_empty_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ));
    }
    if request.uri().path() != WEBHOOK_PATH {
        return Ok(logged_empty_response(StatusCode::NOT_FOUND, "unknown_path"));
    }
    if !has_json_content_type(&request) {
        return Ok(logged_error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "content-type must be application/json",
        ));
    }
    let body = match read_limited(request.into_body(), context.max_body_bytes).await {
        Ok(body) => body,
        Err(ReadBodyError::TooLarge) => {
            return Ok(logged_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "request body is too large",
            ));
        }
        Err(ReadBodyError::Transport) => {
            return Ok(logged_error_response(
                StatusCode::BAD_REQUEST,
                "body_transport",
                "could not read request body",
            ));
        }
    };
    let incoming = match parse_webhook_body(&body, context.max_body_bytes, context.max_detail_bytes)
    {
        Ok(incoming) => incoming,
        Err(ProtocolError::BodyTooLarge { .. }) => {
            return Ok(logged_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "request body is too large",
            ));
        }
        Err(ProtocolError::DetailTooLarge { .. }) => {
            return Ok(logged_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "detail_too_large",
                "approval detail is too large",
            ));
        }
        Err(_) => {
            return Ok(logged_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid webhook request",
            ));
        }
    };
    let submission = match context.broker.submit(incoming).await {
        Ok(submission) => submission,
        Err(BrokerError::DuplicateRequest) => {
            return Ok(logged_error_response(
                StatusCode::CONFLICT,
                "duplicate_request",
                "duplicate approval request",
            ));
        }
        Err(BrokerError::PerSessionCapacity) => {
            return Ok(logged_error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "per_session_capacity",
                "per-session pending limit reached",
            ));
        }
        Err(BrokerError::GlobalCapacity) => {
            return Ok(logged_error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "global_capacity",
                "global pending limit reached",
            ));
        }
        Err(_) => {
            tracing::error!(
                status = 500,
                error_category = "broker_registration",
                "webhook rejected"
            );
            return Ok(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not register approval request",
            ));
        }
    };
    Ok(json_response(StatusCode::OK, &submission.wait().await))
}

fn has_json_content_type(request: &Request<Incoming>) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
}

fn logged_error_response(
    status: StatusCode,
    error_category: &'static str,
    error: &str,
) -> Response<Full<Bytes>> {
    tracing::warn!(status = status.as_u16(), error_category, "webhook rejected");
    error_response(status, error)
}

fn logged_empty_response(
    status: StatusCode,
    error_category: &'static str,
) -> Response<Full<Bytes>> {
    tracing::warn!(status = status.as_u16(), error_category, "webhook rejected");
    empty_response(status)
}

enum ReadBodyError {
    TooLarge,
    Transport,
}

async fn read_limited(mut body: Incoming, limit: usize) -> Result<Vec<u8>, ReadBodyError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ReadBodyError::Transport)?;
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > limit {
                return Err(ReadBodyError::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

fn error_response(status: StatusCode, error: &str) -> Response<Full<Bytes>> {
    json_response(status, &ErrorBody { error })
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| {
        serde_json::to_vec(&ErrorBody {
            error: "response serialization failed",
        })
        .unwrap_or_default()
    });
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

#[cfg(test)]
mod tests {
    use super::WEBHOOK_PATH;

    #[test]
    fn webhook_path_is_fixed() {
        assert_eq!(WEBHOOK_PATH, "/v1/webhooks/approval");
    }
}
