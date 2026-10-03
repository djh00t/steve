//! OpenAI Responses ingress and forwarding.
//!
//! `POST /v1/responses` becomes a logical [`Request`](crate::proxy::Request)
//! and a pending [`RequestAttempt`](crate::proxy::RequestAttempt). Invalid bodies
//! are HTTP 400 in the Steve error model. Configured upstreams return JSON or
//! raw SSE; without one, valid requests retain the typed HTTP 501 stub.

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
struct ResponseInputItem {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Text(String),
    Items(Vec<ResponseInputItem>),
}

#[derive(Debug, Deserialize)]
struct ResponsesBody {
    model: String,
    input: ResponsesInput,
    #[serde(default)]
    stream: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ResponsesHandoff {
    request: Request,
    attempt: RequestAttempt,
    model: String,
    input: ResponsesInput,
    stream: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct ResponsesStub {
    request_id: RequestId,
    attempt_id: AttemptId,
    model: String,
    stream: bool,
    status: &'static str,
}

impl From<&ResponsesHandoff> for ResponsesStub {
    fn from(handoff: &ResponsesHandoff) -> Self {
        Self {
            request_id: handoff.request.id,
            attempt_id: handoff.attempt.id,
            model: handoff.model.clone(),
            stream: handoff.stream,
            status: "not_implemented",
        }
    }
}

pub(crate) enum ResponsesReplyBody {
    Error(SteveErrorResponse),
    Stub(ResponsesStub),
    Success(Value),
    Stream(Body),
}

pub(crate) struct ResponsesReply {
    cache_headers: axum::http::HeaderMap,
    pub(crate) status: StatusCode,
    pub(crate) body: ResponsesReplyBody,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) attempt: Option<Arc<Mutex<RequestAttempt>>>,
}

impl IntoResponse for ResponsesReply {
    fn into_response(self) -> Response {
        let mut response = match self.body {
            ResponsesReplyBody::Error(body) => (self.status, Json(body)).into_response(),
            ResponsesReplyBody::Stub(body) => (self.status, Json(body)).into_response(),
            ResponsesReplyBody::Success(body) => (self.status, Json(body)).into_response(),
            ResponsesReplyBody::Stream(body) => {
                (self.status, [("content-type", SSE_CONTENT_TYPE)], body).into_response()
            }
        };
        response.headers_mut().extend(self.cache_headers);
        response
    }
}

pub(crate) fn handle_responses(body: &[u8]) -> ResponsesReply {
    match parse_responses(body) {
        Ok(handoff) => {
            record_handoff(&handoff);
            ResponsesReply {
                cache_headers: axum::http::HeaderMap::new(),
                status: StatusCode::NOT_IMPLEMENTED,
                body: ResponsesReplyBody::Stub(ResponsesStub::from(&handoff)),
                attempt: Some(Arc::new(Mutex::new(handoff.attempt))),
            }
        }
        Err(error) => ResponsesReply {
            cache_headers: axum::http::HeaderMap::new(),
            status: StatusCode::BAD_REQUEST,
            body: ResponsesReplyBody::Error(SteveErrorResponse { error }),
            attempt: None,
        },
    }
}

pub(crate) async fn handle_responses_with_upstream(
    body: &[u8],
    upstream: &OpenAiUpstream,
) -> ResponsesReply {
    let mut handoff = match parse_responses(body) {
        Ok(handoff) => handoff,
        Err(error) => {
            return ResponsesReply {
                cache_headers: axum::http::HeaderMap::new(),
                status: StatusCode::BAD_REQUEST,
                body: ResponsesReplyBody::Error(SteveErrorResponse { error }),
                attempt: None,
            }
        }
    };
    let mut cache_headers = axum::http::HeaderMap::new();
    record_handoff(&handoff);
    handoff.attempt.provider = "openai".into();
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
            .create_response_stream_with_headers(&request, cancel.clone(), &mut cache_headers)
            .await
        {
            Ok(stream) => {
                let body = pump_upstream(&gate, cancel, stream).expect("fresh replay gate");
                pending.armed = false;
                return ResponsesReply {
                    cache_headers,
                    status: StatusCode::OK,
                    body: ResponsesReplyBody::Stream(Body::from_stream(AttemptBody {
                        inner: body.into_data_stream(),
                        attempt: attempt.clone(),
                    })),
                    attempt: Some(attempt),
                };
            }
            Err(error) => {
                let reply = upstream_failure(attempt, error, cache_headers);
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
    let reply = match upstream
        .create_response_with_headers(&request, &mut cache_headers)
        .await
    {
        Ok(value) => {
            finish_attempt(&attempt, AttemptStatus::Success);
            ResponsesReply {
                cache_headers,
                status: StatusCode::OK,
                body: ResponsesReplyBody::Success(value),
                attempt: Some(attempt),
            }
        }
        Err(error) => upstream_failure(attempt, error, cache_headers),
    };
    pending.armed = false;
    reply
}

fn upstream_failure(
    attempt: Arc<Mutex<RequestAttempt>>,
    error: UpstreamError,
    cache_headers: axum::http::HeaderMap,
) -> ResponsesReply {
    finish_attempt(&attempt, AttemptStatus::UpstreamError);
    let status = if matches!(error, UpstreamError::Timeout { .. }) {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    ResponsesReply {
        cache_headers,
        status,
        body: ResponsesReplyBody::Error(SteveErrorResponse {
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
        "responses upstream attempt finished"
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

fn parse_responses(body: &[u8]) -> Result<ResponsesHandoff, SteveError> {
    let parsed: ResponsesBody = serde_json::from_slice(body).map_err(invalid_json)?;
    validate_responses(&parsed)?;

    let created_at = Utc::now();
    let request_id = RequestId(Uuid::now_v7());
    let attempt_id = AttemptId(Uuid::now_v7());

    Ok(ResponsesHandoff {
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
        input: parsed.input,
        stream: parsed.stream,
    })
}

fn validate_responses(body: &ResponsesBody) -> Result<(), SteveError> {
    if body.model.trim().is_empty() {
        return Err(invalid_request(
            "model must be a non-empty string",
            Some("model".to_string()),
            "invalid_model",
        ));
    }

    match &body.input {
        ResponsesInput::Text(text) if text.trim().is_empty() => Err(invalid_request(
            "input must be a non-empty string or a non-empty item array",
            Some("input".to_string()),
            "invalid_input",
        )),
        ResponsesInput::Items(items) if items.is_empty() => Err(invalid_request(
            "input must be a non-empty string or a non-empty item array",
            Some("input".to_string()),
            "invalid_input",
        )),
        ResponsesInput::Text(_) => Ok(()),
        ResponsesInput::Items(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_input_item(index, item)?;
            }
            Ok(())
        }
    }
}

fn validate_input_item(index: usize, item: &ResponseInputItem) -> Result<(), SteveError> {
    if item
        .kind
        .as_deref()
        .is_some_and(|kind| kind.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].type must be a non-empty string"),
            Some(format!("input[{index}].type")),
            "invalid_type",
        ));
    }

    let message = item.kind.as_deref().map(str::trim).unwrap_or("message") == "message";

    if message
        && item
            .role
            .as_deref()
            .is_none_or(|role| role.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].role must be a non-empty string"),
            Some(format!("input[{index}].role")),
            "invalid_role",
        ));
    }

    if item
        .role
        .as_deref()
        .is_some_and(|role| role.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].role must be a non-empty string"),
            Some(format!("input[{index}].role")),
            "invalid_role",
        ));
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

fn record_handoff(handoff: &ResponsesHandoff) {
    let (input_kind, item_count, text_len, items_with_content) = match &handoff.input {
        ResponsesInput::Text(text) => ("text", 0, text.len(), 0),
        ResponsesInput::Items(items) => (
            "items",
            items.len(),
            0,
            items.iter().filter(|item| item.content.is_some()).count(),
        ),
    };

    tracing::info!(
        event = "responses_parsed",
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
        input_kind,
        item_count,
        text_len,
        items_with_content,
        "responses ingress parsed"
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

    fn error_body(reply: ResponsesReply) -> SteveErrorResponse {
        match reply.body {
            ResponsesReplyBody::Error(error) => error,
            _ => panic!("expected Steve error"),
        }
    }

    fn stub_body(reply: ResponsesReply) -> ResponsesStub {
        match reply.body {
            ResponsesReplyBody::Stub(stub) => stub,
            _ => panic!("expected typed stub"),
        }
    }

    async fn fixture_upstream() -> (OpenAiUpstream, tokio::task::JoinHandle<()>) {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .unwrap();
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        (upstream, server)
    }

    fn attempt(reply: &ResponsesReply) -> Arc<Mutex<RequestAttempt>> {
        reply.attempt.as_ref().expect("attempt").clone()
    }

    #[tokio::test]
    async fn aborted_header_wait_cancels_pending_attempt() {
        let handoff = parse_responses(br#"{"model":"m","input":"hi","stream":true}"#).unwrap();
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
        assert_eq!(attempt.lock().unwrap().status, AttemptStatus::Pending);
        waiting.abort();
        let _ = waiting.await;
        assert!(cancel.is_cancelled());
        let attempt = attempt.lock().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Cancelled);
        assert!(attempt.finished_at.is_some());
    }

    #[tokio::test]
    async fn aborted_json_response_wait_cancels_pending_attempt() {
        let reached = Arc::new(tokio::sync::Notify::new());
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn({
            let reached = reached.clone();
            async move {
                axum::serve(
                    listener,
                    axum::Router::new().route(
                        "/v1/responses",
                        axum::routing::post(move || {
                            let reached = reached.clone();
                            async move {
                                reached.notify_one();
                                std::future::pending::<StatusCode>().await
                            }
                        }),
                    ),
                )
                .await
                .unwrap();
            }
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let handoff = parse_responses(br#"{"model":"m","input":"hi"}"#).unwrap();
        let attempt = Arc::new(Mutex::new(handoff.attempt));
        let waiting = tokio::spawn({
            let attempt = attempt.clone();
            async move {
                let _pending = PendingAttempt {
                    attempt,
                    cancel: None,
                    armed: true,
                };
                let _ = upstream
                    .create_response(&json!({"model":"m","input":"hi"}))
                    .await;
            }
        });
        tokio::time::timeout(Duration::from_secs(1), reached.notified())
            .await
            .expect("upstream received JSON request");
        assert_eq!(attempt.lock().unwrap().status, AttemptStatus::Pending);
        waiting.abort();
        let _ = waiting.await;
        {
            let attempt = attempt.lock().unwrap();
            assert_eq!(attempt.status, AttemptStatus::Cancelled);
            assert!(attempt.finished_at.is_some());
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn configured_json_forwards_fixture_and_finishes_attempt() {
        let (upstream, server) = fixture_upstream().await;
        let reply = handle_responses_with_upstream(
            br#"{"model":"responses-fixture","input":"hi","temperature":0.2}"#,
            &upstream,
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        let recorded = attempt(&reply);
        let response = reply.into_response();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["object"], "response");
        assert_eq!(body["model"], "responses-fixture");
        assert_eq!(
            body["output"][0]["content"][0]["text"],
            "steve-test-response"
        );
        {
            let recorded = recorded.lock().unwrap();
            assert_eq!(recorded.status, AttemptStatus::Success);
            assert!(recorded.finished_at.is_some());
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn configured_sse_forwards_fixture_and_tracks_body_lifecycle() {
        let (upstream, server) = fixture_upstream().await;
        let reply = handle_responses_with_upstream(
            br#"{"model":"responses-fixture","input":"hi","stream":true}"#,
            &upstream,
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        let recorded = attempt(&reply);
        assert_eq!(recorded.lock().unwrap().status, AttemptStatus::Pending);
        let response = reply.into_response();
        assert_eq!(response.headers()["content-type"], SSE_CONTENT_TYPE);
        let mut body = response.into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert_eq!(recorded.lock().unwrap().status, AttemptStatus::Pending);
        let mut events = first.to_vec();
        while let Some(chunk) = body.next().await {
            events.extend_from_slice(&chunk.unwrap());
        }
        let events = String::from_utf8(events).unwrap();
        assert!(events.contains("event: response.created"));
        assert!(events.contains("event: response.output_text.delta"));
        assert!(events.contains("event: response.completed"));
        {
            let recorded = recorded.lock().unwrap();
            assert_eq!(recorded.status, AttemptStatus::Success);
            assert!(recorded.finished_at.is_some());
        }

        let cancelled = handle_responses_with_upstream(
            br#"{"model":"responses-fixture","input":"hi","stream":true}"#,
            &upstream,
        )
        .await;
        let cancelled_attempt = attempt(&cancelled);
        assert_eq!(
            cancelled_attempt.lock().unwrap().status,
            AttemptStatus::Pending
        );
        drop(cancelled.into_response());
        {
            let cancelled_attempt = cancelled_attempt.lock().unwrap();
            assert_eq!(cancelled_attempt.status, AttemptStatus::Cancelled);
            assert!(cancelled_attempt.finished_at.is_some());
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn configured_errors_and_invalid_input_keep_http_contract() {
        let (upstream, server) = fixture_upstream().await;
        let invalid =
            handle_responses_with_upstream(br#"{"model":"","input":"hi"}"#, &upstream).await;
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
        assert!(invalid.attempt.is_none());
        assert_eq!(error_body(invalid).error.code, "invalid_model");
        server.abort();
        let _ = server.await;

        let failed = handle_responses_with_upstream(
            br#"{"model":"responses-fixture","input":"hi"}"#,
            &upstream,
        )
        .await;
        assert_eq!(failed.status, StatusCode::BAD_GATEWAY);
        let recorded = attempt(&failed);
        assert_eq!(error_body(failed).error.code, "upstream_error");
        assert_eq!(
            recorded.lock().unwrap().status,
            AttemptStatus::UpstreamError
        );
        assert!(recorded.lock().unwrap().finished_at.is_some());
    }

    #[tokio::test]
    async fn stream_header_failure_and_timeout_finish_attempt() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/responses",
                    axum::routing::post(|| async { StatusCode::BAD_GATEWAY }),
                ),
            )
            .await
            .unwrap();
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(1)).unwrap();
        let failed = handle_responses_with_upstream(
            br#"{"model":"m","input":"hi","stream":true}"#,
            &upstream,
        )
        .await;
        assert_eq!(failed.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            attempt(&failed).lock().unwrap().status,
            AttemptStatus::UpstreamError
        );
        server.abort();
        let _ = server.await;

        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/responses",
                    axum::routing::post(|| async {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        StatusCode::OK
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_millis(30)).unwrap();
        let failed = handle_responses_with_upstream(
            br#"{"model":"m","input":"hi","stream":true}"#,
            &upstream,
        )
        .await;
        assert_eq!(failed.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            attempt(&failed).lock().unwrap().status,
            AttemptStatus::UpstreamError
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn stream_body_error_finishes_attempt_as_upstream_error() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/responses",
                    axum::routing::post(|| async {
                        let chunks = tokio_stream::iter([
                            Ok(bytes::Bytes::from_static(b"event: response.created\n\n")),
                            Err(std::io::Error::other("body failed")),
                        ])
                        .then(|item| async move {
                            if item.is_err() {
                                tokio::time::sleep(Duration::from_millis(30)).await;
                            }
                            item
                        });
                        (
                            [("content-type", SSE_CONTENT_TYPE)],
                            Body::from_stream(chunks),
                        )
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(1)).unwrap();
        let reply = handle_responses_with_upstream(
            br#"{"model":"m","input":"hi","stream":true}"#,
            &upstream,
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
        let recorded = attempt(&reply);
        assert!(reply.into_response().into_body().collect().await.is_err());
        {
            let recorded = recorded.lock().unwrap();
            assert_eq!(recorded.status, AttemptStatus::UpstreamError);
            assert!(recorded.finished_at.is_some());
        }
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn bad_json_is_400_steve_error() {
        let reply = handle_responses(b"{");
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
        let missing_model = handle_responses(br#"{"input":"hi"}"#);
        assert_eq!(missing_model.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_model).error.code, "invalid_body");

        let missing_input = handle_responses(br#"{"model":"gpt-test"}"#);
        assert_eq!(missing_input.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_input).error.code, "invalid_body");

        let empty_model = handle_responses(br#"{"model":"  ","input":"hi"}"#);
        assert_eq!(empty_model.status, StatusCode::BAD_REQUEST);
        let empty_model = error_body(empty_model);
        assert_eq!(empty_model.error.code, "invalid_model");
        assert_eq!(empty_model.error.param.as_deref(), Some("model"));

        let empty_input = handle_responses(br#"{"model":"gpt-test","input":"  "}"#);
        assert_eq!(empty_input.status, StatusCode::BAD_REQUEST);
        let empty_input = error_body(empty_input);
        assert_eq!(empty_input.error.code, "invalid_input");
        assert_eq!(empty_input.error.param.as_deref(), Some("input"));

        let empty_items = handle_responses(br#"{"model":"gpt-test","input":[]}"#);
        assert_eq!(empty_items.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(empty_items).error.code, "invalid_input");

        let empty_role =
            handle_responses(br#"{"model":"gpt-test","input":[{"role":"  ","content":"hi"}]}"#);
        assert_eq!(empty_role.status, StatusCode::BAD_REQUEST);
        let empty_role = error_body(empty_role);
        assert_eq!(empty_role.error.code, "invalid_role");
        assert_eq!(empty_role.error.param.as_deref(), Some("input[0].role"));

        let missing_role = handle_responses(br#"{"model":"gpt-test","input":[{"content":"hi"}]}"#);
        assert_eq!(missing_role.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_role).error.code, "invalid_role");

        let empty_type =
            handle_responses(br#"{"model":"gpt-test","input":[{"type":"  ","id":"item_1"}]}"#);
        assert_eq!(empty_type.status, StatusCode::BAD_REQUEST);
        let empty_type = error_body(empty_type);
        assert_eq!(empty_type.error.code, "invalid_type");
        assert_eq!(empty_type.error.param.as_deref(), Some("input[0].type"));

        let bad_stream = handle_responses(br#"{"model":"gpt-test","input":"hi","stream":"yes"}"#);
        assert_eq!(bad_stream.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_stream).error.code, "invalid_body");

        let bad_input = handle_responses(br#"{"model":"gpt-test","input":1}"#);
        assert_eq!(bad_input.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_input).error.code, "invalid_body");
    }

    #[test]
    fn valid_text_input_reaches_handler_as_request_attempt_stub() {
        let raw = br#"{"model":"gpt-test","input":"hi","temperature":0}"#;
        let handoff = parse_responses(raw).expect("valid body");

        assert_eq!(handoff.model, "gpt-test");
        assert!(!handoff.stream);
        assert_eq!(handoff.input, ResponsesInput::Text("hi".to_string()));
        assert_eq!(handoff.request.attempts, vec![handoff.attempt.id]);
        assert_eq!(handoff.attempt.request_id, handoff.request.id);
        assert_eq!(handoff.attempt.status, AttemptStatus::Pending);
        assert!(handoff.attempt.finished_at.is_none());
        assert_eq!(handoff.attempt.provider, UNASSIGNED);
        assert_eq!(handoff.attempt.account, UNASSIGNED);
        assert_eq!(handoff.request.created_at, handoff.attempt.started_at);

        let stub = ResponsesStub::from(&handoff);
        assert_eq!(stub.request_id, handoff.request.id);
        assert_eq!(stub.attempt_id, handoff.attempt.id);
        assert_eq!(stub.model, handoff.model);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");

        let reply = handle_responses(raw);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert_eq!(stub.model, "gpt-test");
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");
        assert_ne!(stub.request_id.0, Uuid::nil());
        assert_ne!(stub.attempt_id.0, Uuid::nil());
    }

    #[test]
    fn valid_item_array_keeps_message_content() {
        let raw = br#"{"model":"gpt-test","input":[{"role":"user","content":"hi"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]},{"type":"item_reference","id":"msg_1"}]}"#;
        let handoff = parse_responses(raw).expect("valid items");

        let ResponsesInput::Items(items) = handoff.input else {
            panic!("expected item array");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].role.as_deref(), Some("user"));
        assert_eq!(items[0].content, Some(json!("hi")));
        assert!(items[0].kind.is_none());
        assert_eq!(items[1].kind.as_deref(), Some("message"));
        assert_eq!(items[1].role.as_deref(), Some("assistant"));
        assert_eq!(
            items[1].content,
            Some(json!([{"type": "output_text", "text": "ok"}]))
        );
        assert_eq!(items[2].kind.as_deref(), Some("item_reference"));
        assert!(items[2].role.is_none());
        assert!(items[2].content.is_none());
    }

    #[test]
    fn stream_flag_defaults_false_and_can_be_true() {
        let omitted = parse_responses(br#"{"model":"gpt-test","input":"hi"}"#).expect("omitted");
        assert!(!omitted.stream);

        let enabled =
            parse_responses(br#"{"model":"gpt-test","input":[{"role":"user"}],"stream":true}"#)
                .expect("stream true");
        assert!(enabled.stream);

        let reply = handle_responses(br#"{"model":"gpt-test","input":"hi","stream":true}"#);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        assert!(stub_body(reply).stream);
    }
}
