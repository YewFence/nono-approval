#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use http_body_util::{BodyExt as _, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use nono_approval::broker::{Broker, BrokerConfig, ShowApproval, TerminalState};
use nono_approval::control::{ControlClient, ControlContext, DebugCaptureStatus, DecisionRequest};
use nono_approval::display::MAX_DETAIL_BYTES;
use nono_approval::protocol::{DEFAULT_MAX_BODY_BYTES, WebhookDecision};
use nono_approval::webhook::{WEBHOOK_PATH, WebhookContext};
use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream, UnixListener};

#[tokio::test]
async fn bridges_webhook_to_exact_control_decision() {
    let temporary = tempdir().unwrap();
    let socket_path = temporary.path().join("control.sock");
    let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp_listener.local_addr().unwrap();
    let unix_listener = UnixListener::bind(&socket_path).unwrap();
    let broker = Broker::new(BrokerConfig {
        request_timeout: Duration::from_secs(2),
        ..BrokerConfig::default()
    });

    let webhook_task = tokio::spawn(nono_approval::webhook::serve(
        tcp_listener,
        WebhookContext {
            broker: broker.clone(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_detail_bytes: MAX_DETAIL_BYTES,
        },
    ));
    let control_task = tokio::spawn(nono_approval::control::serve(
        unix_listener,
        ControlContext {
            broker: broker.clone(),
            started_at: Instant::now(),
            webhook_listen: address.to_string(),
            max_pending: 64,
            max_per_session: 8,
            debug_capture: DebugCaptureStatus::Disabled,
        },
    ));

    let webhook = tokio::spawn(send_webhook(address));
    let client = ControlClient::new(&socket_path);
    let approval = loop {
        let approvals = client.list().await.unwrap().approvals;
        if let Some(approval) = approvals.into_iter().next() {
            break approval;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let decision = client
        .decide(&approval.approval_id, &DecisionRequest::Granted)
        .await
        .unwrap();
    assert_eq!(decision.state, TerminalState::Granted);
    assert_eq!(webhook.await.unwrap(), WebhookDecision::Granted);
    assert!(matches!(
        broker.show(&approval.approval_id).await.unwrap(),
        ShowApproval::Completed(completed) if completed.state == TerminalState::Granted
    ));

    webhook_task.abort();
    control_task.abort();
}

async fn send_webhook(address: std::net::SocketAddr) -> WebhookDecision {
    let stream = TcpStream::connect(address).await.unwrap();
    let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let body = br#"{"backend":"local-broker","request":{"capability_type":"command","request_id":"req-bridge","command":"date","args":["date"],"caller":"session","intercept_rule":"approve","reason":null,"child_pid":42,"session_id":"session-bridge"}}"#;
    let request = Request::builder()
        .method(Method::POST)
        .uri(WEBHOOK_PATH)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from_static(body)))
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}
