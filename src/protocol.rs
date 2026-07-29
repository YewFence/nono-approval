use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::display::{ApprovalDetailContent, MAX_DETAIL_BYTES, build_detail};

pub const DEFAULT_MAX_BODY_BYTES: usize = 256 * 1024;
pub const WIRE_ADAPTER_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    ToolSandbox,
    Proxy,
    Capability,
    Network,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AccessMode {
    Read,
    Write,
    ReadWrite,
}

impl std::fmt::Display for AccessMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => formatter.write_str("read"),
            Self::Write => formatter.write_str("write"),
            Self::ReadWrite => formatter.write_str("read+write"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkProtocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for NetworkProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => formatter.write_str("tcp"),
            Self::Udp => formatter.write_str("udp"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "capability_type", rename_all = "snake_case")]
pub enum KnownApprovalRequest {
    Command {
        request_id: String,
        command: String,
        args: Vec<String>,
        caller: String,
        intercept_rule: String,
        reason: Option<String>,
        child_pid: u32,
        session_id: String,
    },
    Endpoint {
        request_id: String,
        route_id: String,
        upstream: String,
        method: String,
        path: String,
        rule_label: String,
        reason: Option<String>,
        child_pid: u32,
        session_id: String,
    },
    Capability {
        request_id: String,
        path: String,
        access: AccessMode,
        reason: Option<String>,
        child_pid: u32,
        session_id: String,
    },
    Network {
        request_id: String,
        host: String,
        port: u16,
        protocol: NetworkProtocol,
        resolved_ips: Vec<String>,
        reason: Option<String>,
        child_pid: u32,
        session_id: String,
    },
}

impl KnownApprovalRequest {
    #[must_use]
    pub fn capability_type(&self) -> &'static str {
        match self {
            Self::Command { .. } => "command",
            Self::Endpoint { .. } => "endpoint",
            Self::Capability { .. } => "capability",
            Self::Network { .. } => "network",
        }
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::Command { request_id, .. }
            | Self::Endpoint { request_id, .. }
            | Self::Capability { request_id, .. }
            | Self::Network { request_id, .. } => request_id,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Command { session_id, .. }
            | Self::Endpoint { session_id, .. }
            | Self::Capability { session_id, .. }
            | Self::Network { session_id, .. } => session_id,
        }
    }

    #[must_use]
    pub fn child_pid(&self) -> u32 {
        match self {
            Self::Command { child_pid, .. }
            | Self::Endpoint { child_pid, .. }
            | Self::Capability { child_pid, .. }
            | Self::Network { child_pid, .. } => *child_pid,
        }
    }

    #[must_use]
    pub fn source_kind(&self) -> SourceKind {
        match self {
            Self::Command { .. } => SourceKind::ToolSandbox,
            Self::Endpoint { .. } => SourceKind::Proxy,
            Self::Capability { .. } => SourceKind::Capability,
            Self::Network { .. } => SourceKind::Network,
        }
    }
}

#[derive(Debug)]
pub struct IncomingApproval {
    pub claimed_backend: String,
    pub raw_request: Box<RawValue>,
    pub request: KnownApprovalRequest,
    pub detail: ApprovalDetailContent,
}

#[derive(Deserialize)]
struct Envelope {
    backend: String,
    request: Box<RawValue>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("request body exceeds {limit} bytes")]
    BodyTooLarge { limit: usize },
    #[error("invalid webhook JSON: {0}")]
    InvalidJson(String),
    #[error("backend must not be empty")]
    EmptyBackend,
    #[error("request_id must not be empty")]
    EmptyRequestId,
    #[error("session_id must not be empty")]
    EmptySessionId,
    #[error("required request field must not be empty: {0}")]
    EmptyField(&'static str),
    #[error("approval detail exceeds {limit} bytes")]
    DetailTooLarge { limit: usize },
}

/// Parses and validates a complete webhook request body.
///
/// # Errors
///
/// Returns an error for size violations, malformed JSON, unsupported variants, or missing identity fields.
pub fn parse_webhook_body(
    body: &[u8],
    max_body_bytes: usize,
    max_detail_bytes: usize,
) -> Result<IncomingApproval, ProtocolError> {
    if body.len() > max_body_bytes {
        return Err(ProtocolError::BodyTooLarge {
            limit: max_body_bytes,
        });
    }
    let envelope: Envelope = serde_json::from_slice(body)
        .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
    if envelope.backend.is_empty() {
        return Err(ProtocolError::EmptyBackend);
    }
    let request: KnownApprovalRequest = serde_json::from_str(envelope.request.get())
        .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
    if request.request_id().is_empty() {
        return Err(ProtocolError::EmptyRequestId);
    }
    if request.session_id().is_empty() {
        return Err(ProtocolError::EmptySessionId);
    }
    validate_required_fields(&request)?;
    let detail = build_detail(&request);
    if detail.serialized_len() > max_detail_bytes {
        return Err(ProtocolError::DetailTooLarge {
            limit: max_detail_bytes,
        });
    }
    Ok(IncomingApproval {
        claimed_backend: envelope.backend,
        raw_request: envelope.request,
        request,
        detail,
    })
}

fn validate_required_fields(request: &KnownApprovalRequest) -> Result<(), ProtocolError> {
    let fields: &[(&str, &'static str)] = match request {
        KnownApprovalRequest::Command {
            command,
            caller,
            intercept_rule,
            ..
        } => &[
            (command, "command"),
            (caller, "caller"),
            (intercept_rule, "intercept_rule"),
        ],
        KnownApprovalRequest::Endpoint {
            route_id,
            upstream,
            method,
            path,
            rule_label,
            ..
        } => &[
            (route_id, "route_id"),
            (upstream, "upstream"),
            (method, "method"),
            (path, "path"),
            (rule_label, "rule_label"),
        ],
        KnownApprovalRequest::Capability { path, .. } => &[(path, "path")],
        KnownApprovalRequest::Network { host, .. } => &[(host, "host")],
    };
    fields
        .iter()
        .find(|(value, _)| value.is_empty())
        .map_or(Ok(()), |(_, name)| Err(ProtocolError::EmptyField(name)))
}

/// Parses a webhook request using the built-in body and detail limits.
///
/// # Errors
///
/// Returns the same validation errors as [`parse_webhook_body`].
pub fn parse_default_webhook_body(body: &[u8]) -> Result<IncomingApproval, ProtocolError> {
    parse_webhook_body(body, DEFAULT_MAX_BODY_BYTES, MAX_DETAIL_BYTES)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum WebhookDecision {
    Granted,
    Denied { reason: String },
}

#[cfg(test)]
mod tests {
    use super::{
        KnownApprovalRequest, ProtocolError, parse_default_webhook_body, parse_webhook_body,
    };

    fn command_json(extra: &str) -> String {
        format!(
            r#"{{"backend":"local-broker","request":{{"capability_type":"command","request_id":"req-1","command":"date","args":["date"],"caller":"session","intercept_rule":"<catch-all>","reason":null,"child_pid":42,"session_id":"sess-1"{extra}}}}}"#
        )
    }

    #[test]
    fn parses_known_variant_and_allows_additional_fields() {
        let incoming =
            parse_default_webhook_body(command_json(",\"future\":true").as_bytes()).unwrap();
        assert!(matches!(
            incoming.request,
            KnownApprovalRequest::Command { .. }
        ));
        assert_eq!(incoming.detail.summary, "date");
    }

    #[test]
    fn rejects_unknown_or_incomplete_variants() {
        let unknown = br#"{"backend":"x","request":{"capability_type":"future","request_id":"r","session_id":"s"}}"#;
        assert!(matches!(
            parse_default_webhook_body(unknown),
            Err(ProtocolError::InvalidJson(_))
        ));
        let incomplete = br#"{"backend":"x","request":{"capability_type":"command","request_id":"r","session_id":"s"}}"#;
        assert!(matches!(
            parse_default_webhook_body(incomplete),
            Err(ProtocolError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_trailing_json_and_body_over_limit() {
        let mut body = command_json("").into_bytes();
        body.extend_from_slice(b" true");
        assert!(matches!(
            parse_default_webhook_body(&body),
            Err(ProtocolError::InvalidJson(_))
        ));
        assert_eq!(
            parse_webhook_body(b"12345", 4, 1024).unwrap_err(),
            ProtocolError::BodyTooLarge { limit: 4 }
        );
    }

    #[test]
    fn parses_every_supported_variant() {
        let fixtures = [
            r#"{"backend":"x","request":{"capability_type":"endpoint","request_id":"r","route_id":"github","upstream":"https://api.github.com","method":"POST","path":"/repos/a/b/issues","rule_label":"approve","reason":null,"child_pid":0,"session_id":"proxy"}}"#,
            r#"{"backend":"x","request":{"capability_type":"capability","request_id":"r","path":"/tmp/demo","access":"ReadWrite","reason":"needed","child_pid":1,"session_id":"s"}}"#,
            r#"{"backend":"x","request":{"capability_type":"network","request_id":"r","host":"example.com","port":443,"protocol":"tcp","resolved_ips":["192.0.2.1"],"reason":null,"child_pid":1,"session_id":"s"}}"#,
        ];
        for fixture in fixtures {
            parse_default_webhook_body(fixture.as_bytes()).unwrap();
        }
    }

    #[test]
    fn rejects_unknown_access_mode_protocol_and_empty_operation_fields() {
        let invalid_access = r#"{"backend":"x","request":{"capability_type":"capability","request_id":"r","path":"/tmp/demo","access":"Execute","reason":null,"child_pid":1,"session_id":"s"}}"#;
        let invalid_protocol = r#"{"backend":"x","request":{"capability_type":"network","request_id":"r","host":"example.com","port":53,"protocol":"sctp","resolved_ips":[],"reason":null,"child_pid":1,"session_id":"s"}}"#;
        let empty_command = command_json("").replace("\"command\":\"date\"", "\"command\":\"\"");
        assert!(matches!(
            parse_default_webhook_body(invalid_access.as_bytes()),
            Err(ProtocolError::InvalidJson(_))
        ));
        assert!(matches!(
            parse_default_webhook_body(invalid_protocol.as_bytes()),
            Err(ProtocolError::InvalidJson(_))
        ));
        assert_eq!(
            parse_default_webhook_body(empty_command.as_bytes()).unwrap_err(),
            ProtocolError::EmptyField("command")
        );
    }
}
