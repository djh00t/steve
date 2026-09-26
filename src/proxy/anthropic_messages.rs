//! Anthropic Messages ingress parsing.
//!
//! `POST /v1/messages` on the inference listener. Clients send:
//!
//! - `content-type: application/json`
//! - `x-api-key: <api-key>`
//! - `anthropic-version: 2023-06-01`
//!
//! A minimal body is `model`, `max_tokens`, and `messages`, with optional
//! `stream` (default `false`). Invalid bodies are HTTP 400 in the Steve error
//! model. A valid body forwards through a configured upstream when
//! non-streaming; unconfigured and streaming requests return HTTP 501 with a
//! typed stub.

use super::{
    stream::{pump_upstream, CancelToken, ReplayGate, SSE_CONTENT_TYPE},
    AnthropicError as UpstreamError, AnthropicUpstream, AttemptId, AttemptStatus, Request,
    RequestAttempt, RequestId,
};
use axum::{
    body::{Body, BodyDataStream},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio_stream::Stream;
use uuid::Uuid;

const UNASSIGNED: &str = "unassigned";
const INVALID_REQUEST: &str = "invalid_request_error";

#[derive(Clone, Debug, PartialEq, Serialize)]
struct SteveError {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
    code: &'static str,
    param: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SteveErrorResponse {
    error: SteveError,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct Message {
    role: String,
    #[serde(default)]
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct MessagesBody {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    #[serde(default)]
    stream: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct MessagesHandoff {
    request: Request,
    attempt: RequestAttempt,
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    stream: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct MessagesStub {
    request_id: RequestId,
    attempt_id: AttemptId,
    model: String,
    max_tokens: u32,
    stream: bool,
    status: &'static str,
}

impl From<&MessagesHandoff> for MessagesStub {
    fn from(handoff: &MessagesHandoff) -> Self {
        Self {
            request_id: handoff.request.id,
            attempt_id: handoff.attempt.id,
            model: handoff.model.clone(),
            max_tokens: handoff.max_tokens,
            stream: handoff.stream,
            status: "not_implemented",
        }
    }
}

pub(crate) enum MessagesReplyBody {
    Error(SteveErrorResponse),
    Stub(MessagesStub),
    Success(Value),
    Stream(Body),
}

pub(crate) struct MessagesReply {
    pub(crate) status: StatusCode,
    pub(crate) body: MessagesReplyBody,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) attempt: Option<Arc<Mutex<RequestAttempt>>>,
}

impl IntoResponse for MessagesReply {
    fn into_response(self) -> Response {
        match self.body {
            MessagesReplyBody::Error(body) => (self.status, Json(body)).into_response(),
            MessagesReplyBody::Stub(body) => (self.status, Json(body)).into_response(),
            MessagesReplyBody::Success(body) => (self.status, Json(body)).into_response(),
            MessagesReplyBody::Stream(body) => {
                (self.status, [("content-type", SSE_CONTENT_TYPE)], body).into_response()
            }
        }
    }
}

pub(crate) fn handle_messages(body: &[u8]) -> MessagesReply {
    match parse_messages(body) {
        Ok(handoff) => {
            record_handoff(&handoff);
            MessagesReply {
                status: StatusCode::NOT_IMPLEMENTED,
                body: MessagesReplyBody::Stub(MessagesStub::from(&handoff)),
                attempt: Some(Arc::new(Mutex::new(handoff.attempt))),
            }
        }
        Err(error) => MessagesReply {
            status: StatusCode::BAD_REQUEST,
            body: MessagesReplyBody::Error(SteveErrorResponse { error }),
            attempt: None,
        },
    }
}

pub(crate) async fn handle_messages_with_upstream(
    body: &[u8],
    upstream: &AnthropicUpstream,
) -> MessagesReply {
    let mut handoff = match parse_messages(body) {
        Ok(handoff) => handoff,
        Err(error) => {
            return MessagesReply {
                status: StatusCode::BAD_REQUEST,
                body: MessagesReplyBody::Error(SteveErrorResponse { error }),
                attempt: None,
            }
        }
    };
    record_handoff(&handoff);
    handoff.attempt.provider = "anthropic".into();
    let attempt = Arc::new(Mutex::new(handoff.attempt));
    let request: Value = serde_json::from_slice(body).expect("validated JSON");

    if handoff.stream {
        let cancel = CancelToken::new();
        let gate = ReplayGate::new();
        let mut pending = PendingAttempt {
            attempt: attempt.clone(),
            cancel: Some(cancel.clone()),
            armed: true,
        };
        match upstream
            .create_message_stream(&request, cancel.clone())
            .await
        {
            Ok(stream) => {
                let body = pump_upstream(&gate, cancel, stream).expect("fresh replay gate");
                pending.armed = false;
                return MessagesReply {
                    status: StatusCode::OK,
                    body: MessagesReplyBody::Stream(Body::from_stream(AttemptBody {
                        inner: body.into_data_stream(),
                        attempt: attempt.clone(),
                    })),
                    attempt: Some(attempt),
                };
            }
            Err(error) => {
                let reply = upstream_failure(attempt, error);
                pending.armed = false;
                return reply;
            }
        }
    }

    let mut pending = PendingAttempt {
        attempt: attempt.clone(),
        cancel: None,
        armed: true,
    };
    let reply = match upstream.create_message(&request).await {
        Ok(value) => {
            finish_attempt(&attempt, AttemptStatus::Success);
            MessagesReply {
                status: StatusCode::OK,
                body: MessagesReplyBody::Success(value),
                attempt: Some(attempt),
            }
        }
        Err(error) => upstream_failure(attempt, error),
    };
    pending.armed = false;
    reply
}

fn upstream_failure(attempt: Arc<Mutex<RequestAttempt>>, error: UpstreamError) -> MessagesReply {
    finish_attempt(&attempt, AttemptStatus::UpstreamError);
    let status = if matches!(error, UpstreamError::Timeout { .. }) {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    MessagesReply {
        status,
        body: MessagesReplyBody::Error(SteveErrorResponse {
            error: SteveError {
                message: error.to_string(),
                kind: "api_error",
                code: "upstream_error",
                param: None,
            },
        }),
        attempt: Some(attempt),
    }
}

fn finish_attempt(attempt: &Arc<Mutex<RequestAttempt>>, status: AttemptStatus) {
    let mut attempt = attempt.lock().expect("attempt lock");
    if attempt.finished_at.is_some() {
        return;
    }
    attempt.status = status;
    attempt.finished_at = Some(Utc::now());
    tracing::info!(
        request_id = %attempt.request_id.0,
        attempt_id = %attempt.id.0,
        status = ?attempt.status,
        finished_at = ?attempt.finished_at,
        "messages upstream attempt finished"
    );
}

struct PendingAttempt {
    attempt: Arc<Mutex<RequestAttempt>>,
    cancel: Option<CancelToken>,
    armed: bool,
}

impl Drop for PendingAttempt {
    fn drop(&mut self) {
        if self.armed {
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
            finish_attempt(&self.attempt, AttemptStatus::Cancelled);
        }
    }
}

struct AttemptBody {
    inner: BodyDataStream,
    attempt: Arc<Mutex<RequestAttempt>>,
}

impl Stream for AttemptBody {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_next(cx);
        match &result {
            Poll::Ready(None) => finish_attempt(&this.attempt, AttemptStatus::Success),
            Poll::Ready(Some(Err(_))) => {
                finish_attempt(&this.attempt, AttemptStatus::UpstreamError)
            }
            _ => {}
        }
        result
    }
}

impl Drop for AttemptBody {
    fn drop(&mut self) {
        finish_attempt(&self.attempt, AttemptStatus::Cancelled);
    }
}

fn parse_messages(body: &[u8]) -> Result<MessagesHandoff, SteveError> {
    let parsed: MessagesBody = serde_json::from_slice(body).map_err(invalid_json)?;
    validate_messages(&parsed)?;

    let created_at = Utc::now();
    let request_id = RequestId(Uuid::now_v7());
    let attempt_id = AttemptId(Uuid::now_v7());

    Ok(MessagesHandoff {
        request: Request {
            id: request_id,
            created_at,
            attempts: vec![attempt_id],
        },
        attempt: RequestAttempt {
            id: attempt_id,
            request_id,
            provider: UNASSIGNED.to_string(),
            account: UNASSIGNED.to_string(),
            status: AttemptStatus::Pending,
            started_at: created_at,
            finished_at: None,
        },
        model: parsed.model,
        max_tokens: parsed.max_tokens,
        messages: parsed.messages,
        stream: parsed.stream,
    })
}

fn validate_messages(body: &MessagesBody) -> Result<(), SteveError> {
    if body.model.trim().is_empty() {
        return Err(invalid_request(
            "model must be a non-empty string",
            Some("model".to_string()),
            "invalid_model",
        ));
    }

    if body.max_tokens == 0 {
        return Err(invalid_request(
            "max_tokens must be a positive integer",
            Some("max_tokens".to_string()),
            "invalid_max_tokens",
        ));
    }

    if body.messages.is_empty() {
        return Err(invalid_request(
            "messages must contain at least one message",
            Some("messages".to_string()),
            "invalid_messages",
        ));
    }

    for (index, message) in body.messages.iter().enumerate() {
        if message.role.trim().is_empty() {
            return Err(invalid_request(
                format!("messages[{index}].role must be a non-empty string"),
                Some(format!("messages[{index}].role")),
                "invalid_role",
            ));
        }
    }

    Ok(())
}

fn invalid_json(err: serde_json::Error) -> SteveError {
    let code = if err.is_syntax() || err.is_eof() {
        "invalid_json"
    } else {
        "invalid_body"
    };
    invalid_request(err.to_string(), None, code)
}

fn invalid_request(
    message: impl Into<String>,
    param: Option<String>,
    code: &'static str,
) -> SteveError {
    SteveError {
        message: message.into(),
        kind: INVALID_REQUEST,
        code,
        param,
    }
}

fn record_handoff(handoff: &MessagesHandoff) {
    let messages_with_content = handoff
        .messages
        .iter()
        .filter(|message| message.content.is_some())
        .count();

    tracing::info!(
        event = "messages_parsed",
        request_id = %handoff.request.id.0,
        created_at = %handoff.request.created_at,
        attempt_count = handoff.request.attempts.len(),
        attempt_id = %handoff.attempt.id.0,
        attempt_request_id = %handoff.attempt.request_id.0,
        provider = %handoff.attempt.provider,
        account = %handoff.attempt.account,
        status = ?handoff.attempt.status,
        started_at = %handoff.attempt.started_at,
        finished = handoff.attempt.finished_at.is_some(),
        model = %handoff.model,
        max_tokens = handoff.max_tokens,
        stream = handoff.stream,
        message_count = handoff.messages.len(),
        messages_with_content,
        "anthropic messages ingress parsed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{net::bind_listener, test_upstream};
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::time::Duration;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn non_stream_messages_reaches_test_upstream_and_finishes_attempt() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .unwrap();
        });
        let client =
            AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let raw = br#"{"model":"steve-test-model","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;

        let reply = handle_messages_with_upstream(raw, &client).await;
        assert_eq!(reply.status, StatusCode::OK);
        let MessagesReplyBody::Success(body) = reply.body else {
            panic!("expected success body");
        };
        assert_eq!(body["content"][0]["text"], "steve-test-response");
        let attempt = reply.attempt.expect("request attempt");
        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Success);
        assert!(attempt.finished_at.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn configured_stream_forwards_fixture_and_finishes_at_eof() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .unwrap();
        });
        let client =
            AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let raw = br#"{"model":"claude-fixture","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":true}"#;

        let reply = handle_messages_with_upstream(raw, &client).await;
        assert_eq!(reply.status, StatusCode::OK);
        let attempt = reply.attempt.as_ref().unwrap().clone();
        assert_eq!(attempt.lock().unwrap().status, AttemptStatus::Pending);
        let response = reply.into_response();
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            sse_events(body),
            vec![
                (
                    "message_start".to_string(),
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_steve_test",
                            "type": "message",
                            "role": "assistant",
                            "content": [],
                            "model": "claude-fixture",
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {"input_tokens": 10, "output_tokens": 0}
                        }
                    }),
                ),
                (
                    "content_block_delta".to_string(),
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": "steve-test-response"}
                    }),
                ),
                ("message_stop".to_string(), json!({"type": "message_stop"})),
            ]
        );
        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Success);
        assert!(attempt.finished_at.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn dropped_stream_body_cancels_attempt() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .unwrap();
        });
        let client =
            AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let reply = handle_messages_with_upstream(
            br#"{"model":"claude-fixture","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":true}"#,
            &client,
        )
        .await;
        let attempt = reply.attempt.as_ref().unwrap().clone();
        assert_eq!(attempt.lock().unwrap().status, AttemptStatus::Pending);

        drop(reply.into_response());

        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Cancelled);
        assert!(attempt.finished_at.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn dropped_header_wait_cancels_token_and_attempt() {
        let handoff = parse_messages(
            br#"{"model":"m","max_tokens":1,"messages":[{"role":"user"}],"stream":true}"#,
        )
        .unwrap();
        let attempt = Arc::new(Mutex::new(handoff.attempt));
        let cancel = CancelToken::new();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn({
            let attempt = attempt.clone();
            let cancel = cancel.clone();
            async move {
                let _pending = PendingAttempt {
                    attempt,
                    cancel: Some(cancel),
                    armed: true,
                };
                ready_tx.send(()).unwrap();
                std::future::pending::<()>().await;
            }
        });
        ready_rx.await.unwrap();
        waiting.abort();
        let _ = waiting.await;

        assert!(cancel.is_cancelled());
        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Cancelled);
        assert!(attempt.finished_at.is_some());
    }

    #[tokio::test]
    async fn stream_header_failure_finishes_attempt_as_upstream_error() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/messages",
                    axum::routing::post(|| async { StatusCode::BAD_GATEWAY }),
                ),
            )
            .await
            .unwrap();
        });
        let client =
            AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let reply = handle_messages_with_upstream(
            br#"{"model":"m","max_tokens":1,"messages":[{"role":"user"}],"stream":true}"#,
            &client,
        )
        .await;

        assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
        let attempt = reply.attempt.unwrap();
        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::UpstreamError);
        assert!(attempt.finished_at.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn stream_forwards_first_chunk_before_tail_is_available() {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let rx = Arc::new(Mutex::new(Some(rx)));
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/messages",
                    axum::routing::post(move || {
                        let rx = rx.clone();
                        async move {
                            (
                                [("content-type", "text/event-stream")],
                                Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(
                                    rx.lock().unwrap().take().unwrap(),
                                )),
                            )
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let first = bytes::Bytes::from_static(b"event: message_start\ndata: {}\n\n");
        tx.send(Ok::<_, std::io::Error>(first.clone()))
            .await
            .unwrap();
        let client =
            AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let reply = handle_messages_with_upstream(
            br#"{"model":"m","max_tokens":1,"messages":[{"role":"user"}],"stream":true}"#,
            &client,
        )
        .await;
        let mut body = reply.into_response().into_body().into_data_stream();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), body.next())
                .await
                .expect("first chunk was buffered behind the tail")
                .unwrap()
                .unwrap(),
            first
        );

        let tail = bytes::Bytes::from_static(b"event: message_stop\ndata: {}\n\n");
        tx.send(Ok(tail.clone())).await.unwrap();
        drop(tx);
        assert_eq!(body.next().await.unwrap().unwrap(), tail);
        assert!(body.next().await.is_none());
        server.abort();
    }

    fn sse_events(body: &str) -> Vec<(String, Value)> {
        body.split("\n\n")
            .filter(|block| !block.is_empty())
            .map(|block| {
                let event = block
                    .lines()
                    .find_map(|line| line.strip_prefix("event: "))
                    .unwrap();
                let data = block
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .unwrap();
                (event.to_string(), serde_json::from_str(data).unwrap())
            })
            .collect()
    }

    fn error_body(reply: MessagesReply) -> SteveErrorResponse {
        match reply.body {
            MessagesReplyBody::Error(error) => error,
            _ => panic!("expected Steve error"),
        }
    }

    fn stub_body(reply: MessagesReply) -> MessagesStub {
        match reply.body {
            MessagesReplyBody::Stub(stub) => stub,
            _ => panic!("expected typed stub"),
        }
    }

    #[test]
    fn bad_json_is_400_steve_error() {
        let reply = handle_messages(b"{");
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);

        let error = error_body(reply);
        assert_eq!(error.error.kind, "invalid_request_error");
        assert_eq!(error.error.code, "invalid_json");
        assert!(error.error.param.is_none());

        let value = serde_json::to_value(&error).expect("serialize error");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["code"], "invalid_json");
        assert!(value["error"]["param"].is_null());
        assert!(!value["error"]["message"].as_str().unwrap().is_empty());
    }

    #[test]
    fn missing_and_empty_fields_are_400() {
        let missing_model =
            handle_messages(br#"{"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#);
        assert_eq!(missing_model.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_model).error.code, "invalid_body");

        let empty_model = handle_messages(
            br#"{"model":"  ","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(empty_model.status, StatusCode::BAD_REQUEST);
        let empty_model = error_body(empty_model);
        assert_eq!(empty_model.error.code, "invalid_model");
        assert_eq!(empty_model.error.param.as_deref(), Some("model"));

        let missing_max_tokens = handle_messages(
            br#"{"model":"claude-test","messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(missing_max_tokens.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_max_tokens).error.code, "invalid_body");

        let zero_max_tokens = handle_messages(
            br#"{"model":"claude-test","max_tokens":0,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(zero_max_tokens.status, StatusCode::BAD_REQUEST);
        let zero_max_tokens = error_body(zero_max_tokens);
        assert_eq!(zero_max_tokens.error.code, "invalid_max_tokens");
        assert_eq!(zero_max_tokens.error.param.as_deref(), Some("max_tokens"));

        let negative_max_tokens = handle_messages(
            br#"{"model":"claude-test","max_tokens":-1,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(negative_max_tokens.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(negative_max_tokens).error.code, "invalid_body");

        let empty_messages =
            handle_messages(br#"{"model":"claude-test","max_tokens":16,"messages":[]}"#);
        assert_eq!(empty_messages.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(empty_messages).error.code, "invalid_messages");

        let empty_role = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"  ","content":"hi"}]}"#,
        );
        assert_eq!(empty_role.status, StatusCode::BAD_REQUEST);
        let empty_role = error_body(empty_role);
        assert_eq!(empty_role.error.code, "invalid_role");
        assert_eq!(empty_role.error.param.as_deref(), Some("messages[0].role"));

        let bad_stream = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":"yes"}"#,
        );
        assert_eq!(bad_stream.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_stream).error.code, "invalid_body");
    }

    #[test]
    fn valid_body_reaches_handler_as_request_attempt_stub() {
        let raw = br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"text","text":"ok"}]}],"system":"be brief"}"#;
        let handoff = parse_messages(raw).expect("valid body");

        assert_eq!(handoff.model, "claude-test");
        assert_eq!(handoff.max_tokens, 16);
        assert!(!handoff.stream);
        assert_eq!(handoff.messages.len(), 2);
        assert_eq!(handoff.messages[0].role, "user");
        assert_eq!(handoff.messages[0].content, Some(json!("hi")));
        assert_eq!(
            handoff.messages[1].content,
            Some(json!([{"type": "text", "text": "ok"}]))
        );
        assert_eq!(handoff.request.attempts, vec![handoff.attempt.id]);
        assert_eq!(handoff.attempt.request_id, handoff.request.id);
        assert_eq!(handoff.attempt.status, AttemptStatus::Pending);
        assert!(handoff.attempt.finished_at.is_none());
        assert_eq!(handoff.attempt.provider, UNASSIGNED);
        assert_eq!(handoff.attempt.account, UNASSIGNED);
        assert_eq!(handoff.request.created_at, handoff.attempt.started_at);

        let stub = MessagesStub::from(&handoff);
        assert_eq!(stub.request_id, handoff.request.id);
        assert_eq!(stub.attempt_id, handoff.attempt.id);
        assert_eq!(stub.model, handoff.model);
        assert_eq!(stub.max_tokens, 16);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");

        let reply = handle_messages(raw);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert_eq!(stub.model, "claude-test");
        assert_eq!(stub.max_tokens, 16);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");
        assert_ne!(stub.request_id.0, Uuid::nil());
        assert_ne!(stub.attempt_id.0, Uuid::nil());
    }

    #[test]
    fn stream_flag_defaults_false_and_can_be_true() {
        let omitted = parse_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .expect("stream omitted");
        assert!(!omitted.stream);

        let enabled = parse_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"assistant"}],"stream":true}"#,
        )
        .expect("stream true");
        assert!(enabled.stream);
        assert!(enabled.messages[0].content.is_none());

        let reply = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        );
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert!(stub.stream);
        assert_eq!(stub.max_tokens, 16);
    }
}
