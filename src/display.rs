use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use shlex::try_quote;
use vte::{Params, Parser, Perform};

use crate::protocol::{KnownApprovalRequest, SourceKind};

pub const MAX_DETAIL_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DetailField {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalDetailContent {
    pub capability_type: String,
    pub summary: String,
    pub source_kind: SourceKind,
    pub fields: Vec<DetailField>,
    pub debug_fields: Vec<DetailField>,
}

impl ApprovalDetailContent {
    #[must_use]
    pub fn serialized_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |value| value.len())
    }
}

#[derive(Default)]
struct Sanitizer {
    output: String,
}

impl Sanitizer {
    fn escaped_control(&mut self, byte: u8) {
        match byte {
            b'\n' => self.output.push_str("\\n"),
            b'\r' => self.output.push_str("\\r"),
            b'\t' => self.output.push_str("\\t"),
            _ => {
                let _ = write!(self.output, "\\x{byte:02x}");
            }
        }
    }
}

impl Perform for Sanitizer {
    fn print(&mut self, character: char) {
        if character.is_control() {
            let _ = write!(self.output, "\\u{{{:x}}}", u32::from(character));
        } else {
            self.output.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        self.escaped_control(byte);
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {}
    fn csi_dispatch(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {}
}

#[must_use]
pub fn sanitize(value: &str) -> String {
    let mut parser = Parser::new();
    let mut sanitizer = Sanitizer::default();
    parser.advance(&mut sanitizer, value.as_bytes());
    sanitizer.output
}

#[must_use]
pub fn truncate_summary(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        return "…".to_owned();
    }
    let mut result: String = value.chars().take(max_chars - 1).collect();
    result.push('…');
    result
}

fn quoted_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let sanitized = sanitize(arg);
            try_quote(&sanitized)
                .map_or_else(|_| format!("'{sanitized}'"), std::borrow::Cow::into_owned)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn field(label: &str, value: impl AsRef<str>) -> DetailField {
    DetailField {
        label: label.to_owned(),
        value: sanitize(value.as_ref()),
    }
}

fn optional_field(fields: &mut Vec<DetailField>, label: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        fields.push(field(label, value));
    }
}

#[must_use]
pub fn build_detail(request: &KnownApprovalRequest) -> ApprovalDetailContent {
    let mut fields = Vec::new();
    match request {
        KnownApprovalRequest::Command {
            command,
            args,
            caller,
            intercept_rule,
            reason,
            ..
        } => {
            fields.push(field("Command", quoted_args(args)));
            if args.is_empty() {
                fields[0] = field("Command", command);
            }
            fields.push(field("Requested by", "Tool Sandbox"));
            fields.push(field("Caller", caller));
            fields.push(field("Rule", intercept_rule));
            optional_field(&mut fields, "Reason", reason.as_deref());
        }
        KnownApprovalRequest::Endpoint {
            route_id,
            upstream,
            method,
            path,
            rule_label,
            reason,
            ..
        } => {
            fields.push(field("Endpoint", format!("{method} {path}")));
            fields.push(field("Route", route_id));
            fields.push(field("Upstream", upstream));
            fields.push(field("Rule", rule_label));
            optional_field(&mut fields, "Reason", reason.as_deref());
        }
        KnownApprovalRequest::Capability {
            path,
            access,
            reason,
            ..
        } => {
            fields.push(field("Path", path));
            fields.push(field("Access", access));
            optional_field(&mut fields, "Reason", reason.as_deref());
        }
        KnownApprovalRequest::Network {
            host,
            port,
            protocol,
            resolved_ips,
            reason,
            ..
        } => {
            fields.push(field("Destination", format!("{host}:{port}")));
            fields.push(field("Protocol", protocol));
            if !resolved_ips.is_empty() {
                fields.push(field("Resolved IPs", resolved_ips.join(", ")));
            }
            optional_field(&mut fields, "Reason", reason.as_deref());
        }
    }

    let debug_fields = vec![
        field("Request ID", request.request_id()),
        field("Session ID", request.session_id()),
        field("Child PID", request.child_pid().to_string()),
    ];

    ApprovalDetailContent {
        capability_type: request.capability_type().to_owned(),
        summary: summary(request),
        source_kind: request.source_kind(),
        fields,
        debug_fields,
    }
}

#[must_use]
pub fn summary(request: &KnownApprovalRequest) -> String {
    match request {
        KnownApprovalRequest::Command { command, args, .. } => {
            if args.is_empty() {
                sanitize(command)
            } else {
                quoted_args(args)
            }
        }
        KnownApprovalRequest::Endpoint { method, path, .. } => {
            sanitize(&format!("{method} {path}"))
        }
        KnownApprovalRequest::Capability { path, access, .. } => {
            sanitize(&format!("{access} {path}"))
        }
        KnownApprovalRequest::Network {
            host,
            port,
            protocol,
            ..
        } => sanitize(&format!("{protocol} {host}:{port}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize, truncate_summary};

    #[test]
    fn removes_terminal_escape_sequences() {
        assert_eq!(
            sanitize("safe\u{1b}]8;;https://evil.invalid\u{7}link\u{1b}]8;;\u{7}"),
            "safelink"
        );
        assert_eq!(sanitize("a\n\tb\u{7f}"), "a\\n\\tb\\u{7f}");
    }

    #[test]
    fn truncates_only_navigation_summaries() {
        assert_eq!(truncate_summary("abcdef", 4), "abc…");
        assert_eq!(truncate_summary("abc", 4), "abc");
    }
}
