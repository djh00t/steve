//! OpenAI Chat Completions ingress parsing.
//!
//! `POST /v1/chat/completions` becomes a logical [`Request`](crate::proxy::Request)
//! and a pending [`RequestAttempt`](crate::proxy::RequestAttempt). Invalid bodies
//! are HTTP 400 in the Steve error model. Non-stream requests use the configured
//! OpenAI-compatible upstream, including raw streaming SSE forwarding.

use super::{
    stream::{pump_upstream, CancelToken, ReplayGate, SSE_CONTENT_TYPE},
    AttemptId, AttemptStatus, OpenAiUpstream, Request, RequestAttempt, RequestId,
    SteveError as UpstreamError,
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
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionBody {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ChatCompletionHandoff {
    request: Request,
    attempt: RequestAttempt,
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct ChatCompletionStub {
    request_id: RequestId,
    attempt_id: AttemptId,
    model: String,
    stream: bool,
    status: &'static str,
}

impl From<&ChatCompletionHandoff> for ChatCompletionStub {
    fn from(handoff: &ChatCompletionHandoff) -> Self {
        Self {
            request_id: handoff.request.id,
            attempt_id: handoff.attempt.id,
            model: handoff.model.clone(),
            stream: handoff.stream,
            status: "not_implemented",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ChatCompletionReplyBody {
    Error(SteveErrorResponse),
    Stub(ChatCompletionStub),
    Success(Value),
    #[serde(skip)]
    Stream(Body),
}

pub(crate) struct ChatCompletionReply {
    pub(crate) status: StatusCode,
    pub(crate) body: ChatCompletionReplyBody,
    pub(crate) request: Option<Request>,
    pub(crate) model: Option<String>,
    pub(crate) requested_stream: bool,
    pub(crate) attempts: Vec<RequestAttempt>,
}

pub(crate) type TerminalAttemptCallback =
    Arc<dyn Fn(&Request, &str, &RequestAttempt) + Send + Sync>;

impl IntoResponse for ChatCompletionReply {
    fn into_response(self) -> Response {
        match self.body {
            ChatCompletionReplyBody::Stream(body) => {
                (self.status, [("content-type", SSE_CONTENT_TYPE)], body).into_response()
            }
            body => (self.status, Json(body)).into_response(),
        }
    }
}

pub(crate) fn handle_chat_completions(body: &[u8]) -> ChatCompletionReply {
    match parse_chat_completions(body) {
        Ok(handoff) => {
            record_handoff(&handoff);
            ChatCompletionReply {
                status: StatusCode::NOT_IMPLEMENTED,
                body: ChatCompletionReplyBody::Stub(ChatCompletionStub::from(&handoff)),
                request: Some(handoff.request),
                model: Some(handoff.model),
                requested_stream: handoff.stream,
                attempts: vec![handoff.attempt],
            }
        }
        Err(error) => ChatCompletionReply {
            status: StatusCode::BAD_REQUEST,
            body: ChatCompletionReplyBody::Error(SteveErrorResponse { error }),
            request: None,
            model: None,
            requested_stream: false,
            attempts: Vec::new(),
        },
    }
}

#[cfg(test)]
pub(crate) async fn handle_chat_completions_with_upstream(
    body: &[u8],
    upstream: &OpenAiUpstream,
) -> ChatCompletionReply {
    handle_chat_completions_with_upstream_and_terminal(body, upstream, Arc::new(|_, _, _| {})).await
}

pub(crate) async fn handle_chat_completions_with_upstream_and_terminal(
    body: &[u8],
    upstream: &OpenAiUpstream,
    on_terminal: TerminalAttemptCallback,
) -> ChatCompletionReply {
    let mut handoff = match parse_chat_completions(body) {
        Ok(handoff) => handoff,
        Err(error) => {
            return ChatCompletionReply {
                status: StatusCode::BAD_REQUEST,
                body: ChatCompletionReplyBody::Error(SteveErrorResponse { error }),
                request: None,
                model: None,
                requested_stream: false,
                attempts: Vec::new(),
            }
        }
    };
    record_handoff(&handoff);
    handoff.attempt.provider = "openai".into();
    let request: Value = serde_json::from_slice(body).expect("validated JSON");
    if handoff.stream {
        let attempt = Arc::new(Mutex::new(handoff.attempt));
        let terminal = StreamAttempt {
            attempt,
            request: handoff.request.clone(),
            model: handoff.model.clone(),
            on_terminal,
        };
        let cancel = CancelToken::new();
        let gate = ReplayGate::new();
        let mut pending = PendingAttempt {
            terminal: terminal.clone(),
            cancel: cancel.clone(),
            armed: true,
        };
        return match upstream
            .chat_completion_stream(&request, cancel.clone())
            .await
        {
            Ok(stream) => {
                let body = pump_upstream(&gate, cancel, stream).expect("fresh replay gate");
                pending.armed = false;
                ChatCompletionReply {
                    status: StatusCode::OK,
                    body: ChatCompletionReplyBody::Stream(Body::from_stream(AttemptBody {
                        inner: body.into_data_stream(),
                        terminal: terminal.clone(),
                    })),
                    request: Some(handoff.request),
                    model: Some(handoff.model),
                    requested_stream: handoff.stream,
                    attempts: vec![terminal.attempt.lock().expect("attempt lock").clone()],
                }
            }
            Err(error) => {
                pending.finish(AttemptStatus::UpstreamError);
                let status = if matches!(error, UpstreamError::Timeout { .. }) {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                ChatCompletionReply {
                    status,
                    body: ChatCompletionReplyBody::Error(SteveErrorResponse {
                        error: SteveError {
                            message: error.to_string(),
                            kind: "api_error",
                            code: "upstream_error",
                            param: None,
                        },
                    }),
                    request: Some(handoff.request),
                    model: Some(handoff.model),
                    requested_stream: handoff.stream,
                    attempts: vec![terminal.attempt.lock().expect("attempt lock").clone()],
                }
            }
        };
    }

    let mut attempts = Vec::new();
    let result = loop {
        let result = upstream.chat_completion(&request).await;
        handoff.attempt.finished_at = Some(Utc::now());
        let retry = result.as_ref().err().is_some_and(is_retryable) && attempts.is_empty();
        if retry {
            handoff.attempt.status = AttemptStatus::UpstreamError;
            attempts.push(handoff.attempt.clone());
            let attempt_id = AttemptId(Uuid::now_v7());
            handoff.request.attempts.push(attempt_id);
            handoff.attempt = RequestAttempt {
                id: attempt_id,
                request_id: handoff.request.id,
                provider: "openai".into(),
                account: UNASSIGNED.into(),
                status: AttemptStatus::Pending,
                started_at: Utc::now(),
                finished_at: None,
            };
            continue;
        }
        break result;
    };
    let (status, response) = match result {
        Ok(value) => {
            handoff.attempt.status = AttemptStatus::Success;
            (StatusCode::OK, ChatCompletionReplyBody::Success(value))
        }
        Err(error) => {
            handoff.attempt.status = AttemptStatus::UpstreamError;
            let status = if matches!(error, UpstreamError::Timeout { .. }) {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            (
                status,
                ChatCompletionReplyBody::Error(SteveErrorResponse {
                    error: SteveError {
                        message: error.to_string(),
                        kind: "api_error",
                        code: "upstream_error",
                        param: None,
                    },
                }),
            )
        }
    };
    attempts.push(handoff.attempt.clone());
    ChatCompletionReply {
        status,
        body: response,
        request: Some(handoff.request),
        model: Some(handoff.model),
        requested_stream: handoff.stream,
        attempts,
    }
}

fn finish_attempt(
    attempt: &Arc<Mutex<RequestAttempt>>,
    status: AttemptStatus,
) -> Option<RequestAttempt> {
    let mut attempt = attempt.lock().expect("attempt lock");
    if attempt.finished_at.is_some() {
        return None;
    }
    attempt.status = status;
    attempt.finished_at = Some(Utc::now());
    tracing::info!(
        request_id = %attempt.request_id.0,
        attempt_id = %attempt.id.0,
        status = ?attempt.status,
        finished_at = ?attempt.finished_at,
        "chat completions upstream attempt finished"
    );
    Some(attempt.clone())
}

#[derive(Clone)]
struct StreamAttempt {
    attempt: Arc<Mutex<RequestAttempt>>,
    request: Request,
    model: String,
    on_terminal: TerminalAttemptCallback,
}

impl StreamAttempt {
    fn finish(&self, status: AttemptStatus) {
        if let Some(attempt) = finish_attempt(&self.attempt, status) {
            (self.on_terminal)(&self.request, &self.model, &attempt);
        }
    }
}

struct PendingAttempt {
    terminal: StreamAttempt,
    cancel: CancelToken,
    armed: bool,
}

impl PendingAttempt {
    fn finish(&mut self, status: AttemptStatus) {
        self.armed = false;
        self.terminal.finish(status);
    }
}

impl Drop for PendingAttempt {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
            self.terminal.finish(AttemptStatus::Cancelled);
        }
    }
}

struct AttemptBody {
    inner: BodyDataStream,
    terminal: StreamAttempt,
}

impl Stream for AttemptBody {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_next(cx);
        match &result {
            Poll::Ready(None) => this.terminal.finish(AttemptStatus::Success),
            Poll::Ready(Some(Err(_))) => this.terminal.finish(AttemptStatus::UpstreamError),
            _ => {}
        }
        result
    }
}

impl Drop for AttemptBody {
    fn drop(&mut self) {
        self.terminal.finish(AttemptStatus::Cancelled);
    }
}

fn is_retryable(error: &UpstreamError) -> bool {
    matches!(error, UpstreamError::UpstreamStatus { status: 503 })
}

fn parse_chat_completions(body: &[u8]) -> Result<ChatCompletionHandoff, SteveError> {
    let parsed: ChatCompletionBody = serde_json::from_slice(body).map_err(invalid_json)?;
    validate_chat_completion(&parsed)?;

    let created_at = Utc::now();
    let request_id = RequestId(Uuid::now_v7());
    let attempt_id = AttemptId(Uuid::now_v7());

    Ok(ChatCompletionHandoff {
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
        messages: parsed.messages,
        stream: parsed.stream,
    })
}

fn validate_chat_completion(body: &ChatCompletionBody) -> Result<(), SteveError> {
    if body.model.trim().is_empty() {
        return Err(invalid_request(
            "model must be a non-empty string",
            Some("model".to_string()),
            "invalid_model",
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

fn record_handoff(handoff: &ChatCompletionHandoff) {
    let messages_with_content = handoff
        .messages
        .iter()
        .filter(|message| message.content.is_some())
        .count();

    tracing::info!(
        event = "chat_completions_parsed",
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
        stream = handoff.stream,
        message_count = handoff.messages.len(),
        messages_with_content,
        "chat completions ingress parsed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{net::bind_listener, test_upstream};
    use axum::{response::IntoResponse, routing::post, Router};
    use serde_json::json;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    #[tokio::test]
    async fn non_stream_chat_reaches_test_upstream_and_finishes_attempt() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .unwrap();
        });
        let client =
            super::super::OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2))
                .unwrap();
        let raw = br#"{"model":"steve-test-model","messages":[{"role":"user","content":"hi"}],"temperature":0.2}"#;

        let reply = handle_chat_completions_with_upstream(raw, &client).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(
            serde_json::to_value(&reply.body).unwrap()["choices"][0]["message"]["content"],
            "steve-test-response"
        );
        assert_eq!(reply.attempts.len(), 1);
        assert_eq!(reply.attempts[0].status, AttemptStatus::Success);
        assert!(reply.attempts[0].finished_at.is_some());
        server.abort();
    }

    #[tokio::test]
    async fn retries_one_transient_failure_and_records_both_attempts() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let calls = Arc::clone(&handler_calls);
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        axum::Json(json!({"id":"retry-success","choices":[]})).into_response()
                    }
                }
            }),
        );
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let raw = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#;

        let reply = handle_chat_completions_with_upstream(raw, &client).await;

        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(reply.attempts.len(), 2);
        assert_ne!(reply.attempts[0].id, reply.attempts[1].id);
        assert_eq!(reply.attempts[0].status, AttemptStatus::UpstreamError);
        assert_eq!(reply.attempts[1].status, AttemptStatus::Success);
        assert!(reply
            .attempts
            .iter()
            .all(|attempt| attempt.finished_at.is_some()));
        assert_eq!(reply.attempts[0].request_id, reply.attempts[1].request_id);
        let request = reply.request.expect("logical request");
        assert_eq!(
            request.attempts,
            reply
                .attempts
                .iter()
                .map(|attempt| attempt.id)
                .collect::<Vec<_>>()
        );
        server.abort();
    }

    #[tokio::test]
    async fn stops_after_one_retry_when_503_repeats() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        );
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let raw = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#;

        let reply = handle_chat_completions_with_upstream(raw, &client).await;

        assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
        assert_eq!(reply.attempts.len(), 2);
        assert!(reply.attempts.iter().all(|attempt| {
            attempt.status == AttemptStatus::UpstreamError && attempt.finished_at.is_some()
        }));
        assert_eq!(
            reply.request.expect("logical request").attempts,
            reply
                .attempts
                .iter()
                .map(|attempt| attempt.id)
                .collect::<Vec<_>>()
        );
        server.abort();
    }

    #[test]
    fn retries_only_http_503() {
        assert!(is_retryable(&UpstreamError::UpstreamStatus { status: 503 }));
        for error in [
            UpstreamError::UpstreamStatus { status: 500 },
            UpstreamError::UpstreamStatus { status: 501 },
            UpstreamError::UpstreamStatus { status: 504 },
            UpstreamError::Timeout { timeout_ms: 1 },
            UpstreamError::Transport {
                message: "closed".into(),
            },
        ] {
            assert!(!is_retryable(&error));
        }
    }

    #[test]
    fn dropping_stream_body_cancels_and_finishes_shared_attempt() {
        let handoff = parse_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        )
        .expect("valid streaming request");
        let attempt = Arc::new(Mutex::new(handoff.attempt));
        let body = AttemptBody {
            inner: Body::empty().into_data_stream(),
            terminal: StreamAttempt {
                attempt: Arc::clone(&attempt),
                request: handoff.request,
                model: handoff.model,
                on_terminal: Arc::new(|_, _, _| {}),
            },
        };

        drop(body);

        let attempt = attempt.lock().expect("attempt lock");
        assert_eq!(attempt.status, AttemptStatus::Cancelled);
        assert!(attempt.finished_at.is_some());
    }

    fn error_body(reply: ChatCompletionReply) -> SteveErrorResponse {
        match reply.body {
            ChatCompletionReplyBody::Error(error) => error,
            _ => panic!("expected Steve error"),
        }
    }

    fn stub_body(reply: ChatCompletionReply) -> ChatCompletionStub {
        match reply.body {
            ChatCompletionReplyBody::Stub(stub) => stub,
            _ => panic!("expected typed stub"),
        }
    }

    #[test]
    fn bad_json_is_400_steve_error() {
        let reply = handle_chat_completions(b"{");
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
            handle_chat_completions(br#"{"messages":[{"role":"user","content":"hi"}]}"#);
        assert_eq!(missing_model.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_model).error.code, "invalid_body");

        let empty_model = handle_chat_completions(
            br#"{"model":"  ","messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(empty_model.status, StatusCode::BAD_REQUEST);
        let empty_model = error_body(empty_model);
        assert_eq!(empty_model.error.code, "invalid_model");
        assert_eq!(empty_model.error.param.as_deref(), Some("model"));

        let empty_messages = handle_chat_completions(br#"{"model":"gpt-test","messages":[]}"#);
        assert_eq!(empty_messages.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(empty_messages).error.code, "invalid_messages");

        let empty_role = handle_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"  ","content":"hi"}]}"#,
        );
        assert_eq!(empty_role.status, StatusCode::BAD_REQUEST);
        let empty_role = error_body(empty_role);
        assert_eq!(empty_role.error.code, "invalid_role");
        assert_eq!(empty_role.error.param.as_deref(), Some("messages[0].role"));

        let bad_stream = handle_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}],"stream":"yes"}"#,
        );
        assert_eq!(bad_stream.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_stream).error.code, "invalid_body");
    }

    #[test]
    fn valid_body_reaches_handler_as_request_attempt_stub() {
        let raw = br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"text","text":"ok"}]}],"temperature":0}"#;
        let handoff = parse_chat_completions(raw).expect("valid body");

        assert_eq!(handoff.model, "gpt-test");
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

        let stub = ChatCompletionStub::from(&handoff);
        assert_eq!(stub.request_id, handoff.request.id);
        assert_eq!(stub.attempt_id, handoff.attempt.id);
        assert_eq!(stub.model, handoff.model);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");

        let reply = handle_chat_completions(raw);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert_eq!(stub.model, "gpt-test");
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");
        assert_ne!(stub.request_id.0, Uuid::nil());
        assert_ne!(stub.attempt_id.0, Uuid::nil());
    }

    #[test]
    fn stream_flag_defaults_false_and_can_be_true() {
        let omitted = parse_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .expect("stream omitted");
        assert!(!omitted.stream);

        let enabled = parse_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"tool"}],"stream":true}"#,
        )
        .expect("stream true");
        assert!(enabled.stream);
        assert!(enabled.messages[0].content.is_none());

        let reply = handle_chat_completions(
            br#"{"model":"gpt-test","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        );
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        assert!(stub_body(reply).stream);
    }
}
