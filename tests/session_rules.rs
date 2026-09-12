#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use nono_approval::broker::{
    Broker, BrokerConfig, BrokerError, IngressOutcome, ShowApproval, Submission, TerminalState,
};
use nono_approval::control::{
    ControlClient, ControlContext, MAX_SESSION_RULES_BODY_BYTES, SessionRuleRequest,
};
use nono_approval::debug_capture::{DebugCapture, DebugCaptureStatus};
use nono_approval::display::MAX_DETAIL_BYTES;
use nono_approval::policy::{MAX_SESSION_RULES, PolicyError, RuleAction, RuleScope, load_policy};
use nono_approval::protocol::{IncomingApproval, WebhookDecision, parse_default_webhook_body};
use nono_approval::webhook::{WEBHOOK_PATH, WebhookContext};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::task::JoinHandle;

fn body(id: &str, path: &str, access: &str) -> Value {
    json!({"backend":"test", "request": {
        "capability_type":"capability", "request_id":id, "session_id":format!("session-{id}"),
        "child_pid":1, "path":path, "access":access, "reason":null,
    }})
}

fn incoming(id: &str, path: &str, access: &str) -> IncomingApproval {
    parse_default_webhook_body(&serde_json::to_vec(&body(id, path, access)).unwrap()).unwrap()
}

async fn seed(broker: &Broker, id: &str, path: &str) -> Submission {
    broker.submit(incoming(id, path, "Read")).await.unwrap()
}

struct Bridge {
    broker: Broker,
    client: ControlClient,
    address: SocketAddr,
    socket: std::path::PathBuf,
    capture: std::path::PathBuf,
    tasks: Vec<JoinHandle<std::io::Result<()>>>,
    _temporary: TempDir,
}

impl Bridge {
    async fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("control.sock");
        let capture = DebugCapture::create(&temporary.path().canonicalize().unwrap()).unwrap();
        let DebugCaptureStatus::Enabled { path: capture_path } = capture.status() else {
            panic!("capture disabled");
        };
        let broker = Broker::with_debug_capture(
            BrokerConfig {
                request_timeout: Duration::from_secs(5),
                max_pending: 1,
                max_per_session: 1,
                ..BrokerConfig::default()
            },
            capture.clone(),
        )
        .unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let unix = UnixListener::bind(&socket).unwrap();
        let tasks = vec![
            tokio::spawn(nono_approval::webhook::serve(
                tcp,
                WebhookContext {
                    broker: broker.clone(),
                    max_body_bytes: 256 * 1024,
                    max_detail_bytes: MAX_DETAIL_BYTES,
                },
            )),
            tokio::spawn(nono_approval::control::serve(
                unix,
                ControlContext {
                    broker: broker.clone(),
                    started_at: Instant::now(),
                    webhook_listen: address.to_string(),
                    max_pending: 1,
                    max_per_session: 1,
                    debug_capture: Some(capture),
                },
            )),
        ];
        Self {
            broker,
            client: ControlClient::new(&socket),
            socket,
            address,
            capture: capture_path,
            tasks,
            _temporary: temporary,
        }
    }

    async fn webhook(&self, body: Value) -> (StatusCode, Value) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let stream = TcpStream::connect(self.address).await.unwrap();
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
            let task = tokio::spawn(connection);
            let request = Request::post(WEBHOOK_PATH)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            task.abort();
            (status, serde_json::from_slice(&bytes).unwrap())
        })
        .await
        .expect("webhook did not return immediately")
    }

    async fn cli(&self, args: &[&str]) -> std::process::Output {
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_nono-approval"))
                .args(args)
                .arg("--control-socket")
                .arg(&self.socket)
                .output(),
        )
        .await
        .unwrap()
        .unwrap()
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[tokio::test]
async fn cli_rules_bypass_capacity_and_capture_hits_without_approval_ids() {
    let bridge = Bridge::new().await;
    let original = seed(&bridge.broker, "seed", "/work").await;
    let id = original.approval_id.clone();
    let output = bridge.cli(&["approve", id.as_str(), "--session-dir"]).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(original.wait().await, WebhookDecision::Granted);
    assert!(
        matches!(bridge.broker.show(&id).await.unwrap(), ShowApproval::Completed(done) if done.state == TerminalState::Granted)
    );
    let pending = seed(&bridge.broker, "other", "/other").await;
    assert_eq!(bridge.client.status().await.unwrap().session_rule_count, 1);
    let (code, decision) = bridge.webhook(body("hit", "/work/sub/file", "Read")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(decision, json!({"decision":"granted"}));
    // Both path boundary and exact access failures must still encounter pending capacity.
    for (path, access) in [
        ("/workspace", "Read"),
        ("/work/file", "ReadWrite"),
        ("/work/../secret", "Read"),
    ] {
        assert_eq!(
            bridge.webhook(body("miss", path, access)).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let mut invalid = body("invalid", "/work", "Read");
    invalid["request"]["session_id"] = json!("");
    assert_eq!(bridge.webhook(invalid).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(bridge.broker.pending_count().await, 1);
    let status_output = bridge.cli(&["status"]).await;
    assert!(String::from_utf8_lossy(&status_output.stdout).contains("Session rules: 1"));
    let output = bridge.cli(&["session-rules", "clear"]).await;
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Cleared 1"));
    assert_eq!(bridge.client.status().await.unwrap().session_rule_count, 0);
    assert_eq!(
        bridge
            .webhook(body("after-clear", "/work/file", "Read"))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(bridge.broker.pending_count().await, 1);
    bridge
        .broker
        .decide(&pending.approval_id, WebhookDecision::Granted)
        .await
        .unwrap();
    pending.wait().await;
    let records = std::fs::read_to_string(&bridge.capture)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let hits = records
        .iter()
        .filter(|record| record["event"] == "policy_decision")
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].get("approval_id").is_none());
    assert_eq!(hits[0]["rule"]["access"], "Read");
    assert_eq!(hits[0]["decision_source"], "session_rule");
    assert_eq!(hits[0]["response_delivery_outcome"], "not_observed");
}

#[tokio::test]
async fn deny_cli_preserves_reason_and_records_automatic_denials() {
    let bridge = Bridge::new().await;
    let original = seed(&bridge.broker, "seed", "/config").await;
    let output = bridge
        .cli(&[
            "deny",
            original.approval_id.as_str(),
            "--session-path",
            "--reason",
            "private",
        ])
        .await;
    assert!(output.status.success());
    assert_eq!(
        original.wait().await,
        WebhookDecision::Denied {
            reason: "private".to_owned()
        }
    );
    let (code, denied) = bridge.webhook(body("deny-hit", "/config", "Read")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(denied["decision"], "denied");
    assert!(denied["reason"].as_str().unwrap().contains("session rule"));
    let IngressOutcome::Pending(child) = bridge
        .broker
        .ingress(incoming("child", "/config/known", "Read"))
        .await
        .unwrap()
    else {
        panic!("exact rule must not match children");
    };
    let rule = SessionRuleRequest {
        action: RuleAction::Deny,
        scope: RuleScope::Directory,
        reason: None,
        path: None,
    };
    bridge
        .client
        .remember(&child.approval_id, &rule)
        .await
        .unwrap();
    child.wait().await;
    let (_, denied) = bridge
        .webhook(body("tree-hit", "/config/known/sub/file", "Read"))
        .await;
    assert_eq!(denied["decision"], "denied");
}

#[tokio::test]
async fn stale_or_unsupported_requests_do_not_install_rules() {
    let broker = Broker::new(BrokerConfig::default()).unwrap();
    let command = parse_default_webhook_body(br#"{"backend":"x","request":{"capability_type":"command","request_id":"c","session_id":"s","command":"date","args":[],"caller":"test","intercept_rule":"test","child_pid":1}}"#).unwrap();
    let command = broker.submit(command).await.unwrap();
    assert_eq!(
        broker
            .decide_and_remember(
                &command.approval_id,
                RuleAction::Allow,
                RuleScope::Directory,
                None
            )
            .await,
        Err(BrokerError::Policy(PolicyError::NotCapability))
    );
    assert!(matches!(
        broker.show(&command.approval_id).await.unwrap(),
        ShowApproval::Pending(_)
    ));
    for path in ["relative/path", "/work/../secret", "/nul\0"] {
        let submission = seed(&broker, path, path).await;
        assert_eq!(
            broker
                .decide_and_remember(
                    &submission.approval_id,
                    RuleAction::Allow,
                    RuleScope::Directory,
                    None
                )
                .await,
            Err(BrokerError::Policy(PolicyError::InvalidPath))
        );
        assert!(matches!(
            broker.show(&submission.approval_id).await.unwrap(),
            ShowApproval::Pending(_)
        ));
    }
    let completed = seed(&broker, "done", "/work").await;
    broker
        .decide(&completed.approval_id, WebhookDecision::Granted)
        .await
        .unwrap();
    assert_eq!(
        broker
            .decide_and_remember(
                &completed.approval_id,
                RuleAction::Allow,
                RuleScope::Directory,
                None
            )
            .await,
        Err(BrokerError::NotPending)
    );
    assert_eq!(broker.session_rule_count().await, 0);
    let expired_broker = Broker::new(BrokerConfig {
        request_timeout: Duration::ZERO,
        ..BrokerConfig::default()
    })
    .unwrap();
    let expired = seed(&expired_broker, "expired", "/work").await;
    assert_eq!(
        expired_broker
            .decide_and_remember(
                &expired.approval_id,
                RuleAction::Allow,
                RuleScope::Directory,
                None
            )
            .await,
        Err(BrokerError::NotPending)
    );
    assert_eq!(expired_broker.session_rule_count().await, 0);
}

#[tokio::test]
async fn rule_limit_failure_is_atomic_and_replacements_still_work() {
    let broker = Broker::new(BrokerConfig::default()).unwrap();
    for index in 0..MAX_SESSION_RULES {
        let source = seed(&broker, &index.to_string(), &format!("/work/{index}")).await;
        broker
            .decide_and_remember(
                &source.approval_id,
                RuleAction::Allow,
                RuleScope::Directory,
                None,
            )
            .await
            .unwrap();
        source.wait().await;
    }
    let extra = seed(&broker, "extra", "/extra").await;
    assert_eq!(
        broker
            .decide_and_remember(&extra.approval_id, RuleAction::Deny, RuleScope::Path, None)
            .await,
        Err(BrokerError::Policy(PolicyError::Capacity))
    );
    assert!(matches!(
        broker.show(&extra.approval_id).await.unwrap(),
        ShowApproval::Pending(_)
    ));
    let replacement = seed(&broker, "replace", "/work/0").await;
    broker
        .decide_and_remember(
            &replacement.approval_id,
            RuleAction::Deny,
            RuleScope::Directory,
            None,
        )
        .await
        .unwrap();
    replacement.wait().await;
    assert_eq!(broker.session_rule_count().await, MAX_SESSION_RULES);
    assert!(matches!(
        broker
            .ingress(incoming("denied", "/work/0", "Read"))
            .await
            .unwrap(),
        IngressOutcome::Automatic(WebhookDecision::Denied { .. })
    ));
    broker.shutdown().await;
    assert_eq!(broker.session_rule_count().await, 0);
    assert!(matches!(
        broker
            .ingress(incoming("shutdown", "/work/1", "Read"))
            .await
            .unwrap(),
        IngressOutcome::Automatic(WebhookDecision::Denied { .. })
    ));
    let fresh = Broker::new(BrokerConfig::default()).unwrap();
    assert!(matches!(
        fresh
            .ingress(incoming("restart", "/work/1", "Read"))
            .await
            .unwrap(),
        IngressOutcome::Pending(_)
    ));
}

#[tokio::test]
async fn concurrent_decisions_install_at_most_one_rule_and_leave_other_pending_requests_alone() {
    let broker = Broker::new(BrokerConfig::default()).unwrap();
    let source = seed(&broker, "source", "/work").await;
    let waiting = seed(&broker, "waiting", "/work/child").await;
    let (left, right) = tokio::join!(
        broker.decide_and_remember(
            &source.approval_id,
            RuleAction::Allow,
            RuleScope::Directory,
            None
        ),
        broker.decide_and_remember(&source.approval_id, RuleAction::Deny, RuleScope::Path, None),
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(left.err().or(right.err()), Some(BrokerError::NotPending));
    assert_eq!(broker.session_rule_count().await, 1);
    assert!(matches!(
        broker.show(&waiting.approval_id).await.unwrap(),
        ShowApproval::Pending(_)
    ));
    assert_eq!(broker.clear_session_rules().await, 1);
    assert!(matches!(
        broker.show(&waiting.approval_id).await.unwrap(),
        ShowApproval::Pending(_)
    ));
}

#[tokio::test]
async fn control_rejects_malformed_or_oversized_rule_bodies_without_deciding() {
    let bridge = Bridge::new().await;
    let source = seed(&bridge.broker, "source", "/work").await;
    for body in [
        br#"{"action":"allow","scope":"directory","path":"/injected"}"#.to_vec(),
        br#"{"action":"allow","scope":"directory","access":"ReadWrite"}"#.to_vec(),
        br#"{"action":"allow","scope":"directory","reason":"ignored?"}"#.to_vec(),
        br#"{"action":"allow","scope":"unknown"}"#.to_vec(),
        br#"{"action":"deny","scope":"path","reason":""}"#.to_vec(),
        vec![b'x'; 8193],
    ] {
        let stream = UnixStream::connect(&bridge.socket).await.unwrap();
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
        let task = tokio::spawn(connection);
        let request = Request::post(format!("/v1/approvals/{}/session-rule", source.approval_id))
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        assert_eq!(
            sender.send_request(request).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        task.abort();
        assert_eq!(bridge.broker.session_rule_count().await, 0);
        assert_eq!(bridge.broker.pending_count().await, 1);
    }
}

#[tokio::test]
async fn cli_edited_tree_path_covers_source_and_keeps_exact_access() {
    let bridge = Bridge::new().await;
    let source = seed(
        &bridge.broker,
        "project-file",
        "/path/to/project/src/main.rs",
    )
    .await;
    let id = source.approval_id.clone();
    for args in [
        vec![
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "/unrelated",
        ],
        vec![
            "approve",
            id.as_str(),
            "--session-path",
            "--rule-path",
            "/path/to/project",
        ],
        vec![
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "/path/../",
        ],
        vec![
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "relative",
        ],
        vec![
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "/path/*",
        ],
    ] {
        let result = bridge.cli(&args).await;
        assert!(!result.status.success());
        assert_eq!(bridge.broker.session_rule_count().await, 0);
        assert!(matches!(
            bridge.broker.show(&id).await.unwrap(),
            ShowApproval::Pending(_)
        ));
    }
    let result = bridge
        .cli(&[
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "/path/to/project",
        ])
        .await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(source.wait().await, WebhookDecision::Granted);
    assert_eq!(
        bridge
            .webhook(body("other", "/path/to/project/tests/test.rs", "Read"))
            .await
            .1,
        json!({"decision":"granted"})
    );
    let pending = seed(&bridge.broker, "capacity", "/other").await;
    for (path, access) in [
        ("/path/to/project-other", "Read"),
        ("/path/to/project/file", "ReadWrite"),
    ] {
        assert_eq!(
            bridge.webhook(body("miss", path, access)).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(bridge.broker.pending_count().await, 1);
    drop(pending);
}

#[tokio::test]
async fn edited_path_cannot_be_installed_from_stale_source_and_exact_allow_is_repeatable() {
    let bridge = Bridge::new().await;
    let source = seed(&bridge.broker, "exact", "/work/file").await;
    let id = source.approval_id.clone();
    let result = bridge
        .cli(&["approve", id.as_str(), "--session-path"])
        .await;
    assert!(result.status.success());
    source.wait().await;
    assert_eq!(
        bridge
            .webhook(body("same-file", "/work/file", "Read"))
            .await
            .1,
        json!({"decision":"granted"})
    );
    let result = bridge
        .cli(&[
            "approve",
            id.as_str(),
            "--session-dir",
            "--rule-path",
            "/work",
        ])
        .await;
    assert!(!result.status.success());
    assert_eq!(bridge.broker.session_rule_count().await, 1);
    assert!(matches!(
        bridge
            .broker
            .ingress(incoming("other", "/work/other", "Read"))
            .await
            .unwrap(),
        IngressOutcome::Pending(_)
    ));
}

#[tokio::test]
async fn rule_matched_webhook_repeats_conflict_instead_of_regranting() {
    let bridge = Bridge::new().await;
    let source = seed(&bridge.broker, "exact", "/work/file").await;
    let result = bridge
        .cli(&["approve", source.approval_id.as_str(), "--session-path"])
        .await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    source.wait().await;
    let repeated = body("dup", "/work/file", "Read");
    let (code, first) = bridge.webhook(repeated.clone()).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(first, json!({"decision":"granted"}));
    let (code, second) = bridge.webhook(repeated).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(second["error"], "duplicate approval request");
    // Reserving one replay key must not block other requests from matching.
    let (code, fresh) = bridge.webhook(body("fresh", "/work/file", "Read")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(fresh, json!({"decision":"granted"}));
}

#[tokio::test]
async fn control_replaces_full_size_policy_and_rejects_oversized_batches() {
    let bridge = Bridge::new().await;
    let temporary = tempfile::tempdir().unwrap();
    let long = "a".repeat(1024);
    let mut policy = String::new();
    for index in 0..MAX_SESSION_RULES {
        let _ = write!(
            policy,
            "[[rules]]\naction = \"allow\"\npath = \"/work/{index:03}/{long}\"\nscope = \"directory\"\naccess = \"Read\"\n\n"
        );
    }
    let policy_path = temporary.path().join("full-policy.toml");
    std::fs::write(&policy_path, policy).unwrap();
    let rules = load_policy(&policy_path).unwrap();
    assert_eq!(rules.len(), MAX_SESSION_RULES);
    let response = bridge.client.replace_session_rules(&rules).await.unwrap();
    assert_eq!(response.installed, MAX_SESSION_RULES);
    assert_eq!(
        bridge.client.status().await.unwrap().session_rule_count,
        MAX_SESSION_RULES
    );
    let (code, decision) = bridge
        .webhook(body("hit", &format!("/work/000/{long}"), "Read"))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(decision, json!({"decision":"granted"}));

    let over_limit = vec![b'x'; MAX_SESSION_RULES_BODY_BYTES + 1];
    let stream = UnixStream::connect(&bridge.socket).await.unwrap();
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
    let task = tokio::spawn(connection);
    let request = Request::put("/v1/session-rules")
        .body(Full::new(Bytes::from(over_limit)))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    task.abort();

    let too_many = json!({"rules": (0..=MAX_SESSION_RULES).map(|index| json!({
        "action": "allow",
        "path": format!("/work/{index}"),
        "scope": "directory",
        "access": "Read",
    })).collect::<Vec<_>>()});
    let stream = UnixStream::connect(&bridge.socket).await.unwrap();
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
    let task = tokio::spawn(connection);
    let request = Request::put("/v1/session-rules")
        .body(Full::new(Bytes::from(
            serde_json::to_vec(&too_many).unwrap(),
        )))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    task.abort();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["error"],
        "session rule limit reached (128)"
    );
    assert_eq!(
        bridge.client.status().await.unwrap().session_rule_count,
        MAX_SESSION_RULES
    );
}

#[tokio::test]
async fn policy_flag_with_subcommand_fails_without_touching_rules() {
    let bridge = Bridge::new().await;
    let source = seed(&bridge.broker, "source", "/work").await;
    let output = bridge
        .cli(&["approve", source.approval_id.as_str(), "--session-dir"])
        .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    source.wait().await;
    let output = bridge.cli(&["--policy", "work", "status"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--policy"), "{stderr}");
    assert_eq!(bridge.client.status().await.unwrap().session_rule_count, 1);
}
