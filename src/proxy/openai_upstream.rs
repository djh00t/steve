//! Non-streaming OpenAI-compatible chat completions client.
//!
//! Posts JSON to OpenAI-compatible chat completions and Responses endpoints.
//! Responses SSE chunks are returned unbuffered so ingress can stop the
//! upstream by dropping the stream. The base URL must be absolute `https` or
//! numeric-loopback `http`.

use super::{stream::CancelToken, validate_upstream_url};
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio_stream::{Stream, StreamExt};

const EVENT_STREAM: &str = "text/event-stream";

/// Stub of Steve's structured error model.
///
/// Only failures this client can observe are represented. Later proxy stages
/// extend the same model.
#[derive(Debug, thiserror::Error)]
pub enum SteveError {
    #[error("upstream client configuration is invalid: {message}")]
    Config { message: String },

    #[error("upstream request timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("upstream transport failed: {message}")]
    Transport { message: String },

    #[error("upstream returned HTTP {status}")]
    UpstreamStatus { status: u16 },

    #[error("upstream response was not valid JSON: {message}")]
    InvalidJson { message: String },

    #[error("streaming chat completions are not supported by this client")]
    StreamingNotSupported,

    #[error("upstream response content-type is not text/event-stream: {message}")]
    UnexpectedContentType { message: String },

    #[error("upstream request was cancelled")]
    Cancelled,
}

/// HTTP client for one OpenAI-compatible chat-completions origin.
#[derive(Clone, Debug)]
pub struct OpenAiUpstream {
    base_url: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl OpenAiUpstream {
    /// `base_url` is an absolute `https` or numeric-loopback `http` origin,
    /// with an optional OpenAI-style `/v1` root and no embedded credentials.
    /// `timeout` bounds JSON calls and the response-header wait for SSE.
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self, SteveError> {
        let base_url = normalize_base_url(base_url.into())?;
        if timeout.is_zero() {
            return Err(SteveError::Config {
                message: "timeout must be greater than zero".into(),
            });
        }

        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| SteveError::Config {
                message: format!("http client: {err}"),
            })?;

        Ok(Self {
            base_url,
            timeout,
            http,
        })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// POST a non-streaming chat completion and return the parsed JSON body.
    ///
    /// `request` is encoded as the JSON body. `stream: true` is rejected.
    pub async fn chat_completion(&self, request: &impl Serialize) -> Result<Value, SteveError> {
        let body = serde_json::to_value(request).map_err(|err| SteveError::Config {
            message: format!("chat completion request is not valid JSON: {err}"),
        })?;
        if body.get("stream").and_then(Value::as_bool) == Some(true) {
            return Err(SteveError::StreamingNotSupported);
        }

        let url = chat_completions_url(&self.base_url);
        let response = self
            .http
            .post(&url)
            .timeout(self.timeout)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|err| map_http_error(err, self.timeout))?;

        let status = response.status();
        if !status.is_success() {
            return Err(SteveError::UpstreamStatus {
                status: status.as_u16(),
            });
        }

        response
            .json()
            .await
            .map_err(|err| map_http_error(err, self.timeout))
    }

    /// POST a streaming chat completion and return the SSE body as bytes.
    /// The timeout only bounds the response header wait; body reads are unbounded.
    pub async fn chat_completion_stream(
        &self,
        request: &impl Serialize,
        cancel: CancelToken,
    ) -> Result<OpenAiEventStream, SteveError> {
        if cancel.is_cancelled() {
            return Err(SteveError::Cancelled);
        }

        let body = prepare_stream_body(request)?;
        let send = self
            .http
            .post(chat_completions_url(&self.base_url))
            .header(reqwest::header::ACCEPT, EVENT_STREAM)
            .json(&body)
            .send();
        let cancel_for_wait = cancel.clone();
        let response = tokio::select! {
            biased;
            () = cancel_for_wait.cancelled() => return Err(SteveError::Cancelled),
            response = tokio::time::timeout(self.timeout, send) => {
                match response {
                    Ok(result) => result.map_err(|err| map_http_error(err, self.timeout))?,
                    Err(_) => return Err(SteveError::Timeout { timeout_ms: duration_millis(self.timeout) }),
                }
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Err(SteveError::UpstreamStatus {
                status: status.as_u16(),
            });
        }
        if !is_event_stream(response.headers()) {
            let message = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("missing")
                .to_string();
            return Err(SteveError::UnexpectedContentType { message });
        }

        Ok(OpenAiEventStream {
            inner: Some(Box::pin(response.bytes_stream())),
            cancel_fut: Box::pin(cancel.cancelled_future()),
            _cancel: cancel,
            finished: false,
            timeout: self.timeout,
        })
    }
    /// POST a non-streaming Responses request and return its parsed JSON body.
    ///
    /// `stream: true` is rejected before opening a connection.
    pub async fn create_response(&self, request: &impl Serialize) -> Result<Value, SteveError> {
        let body = prepare_responses_body(request, false)?;
        let response = self
            .post_responses(&body, "application/json")
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|err| map_http_error(err, self.timeout))?;

        let status = response.status();
        if !status.is_success() {
            return Err(SteveError::UpstreamStatus {
                status: status.as_u16(),
            });
        }
        response
            .json()
            .await
            .map_err(|err| map_http_error(err, self.timeout))
    }

    /// POST a streaming Responses request and yield SSE bytes as they arrive.
    ///
    /// The timeout and `cancel` apply only while waiting for response headers.
    /// The returned body stream does not buffer; dropping it aborts the
    /// upstream request, including when the ingress pump cancels its body.
    pub async fn create_response_stream(
        &self,
        request: &impl Serialize,
        cancel: CancelToken,
    ) -> Result<impl Stream<Item = Result<Bytes, SteveError>> + Send + 'static, SteveError> {
        if cancel.is_cancelled() {
            return Err(SteveError::Cancelled);
        }
        let body = prepare_responses_body(request, true)?;
        let send = self.post_responses(&body, EVENT_STREAM).send();
        let cancel_for_wait = cancel.clone();
        let response = tokio::select! {
            biased;
            () = cancel_for_wait.cancelled() => return Err(SteveError::Cancelled),
            response = tokio::time::timeout(self.timeout, send) => {
                match response {
                    Ok(result) => result.map_err(|err| map_http_error(err, self.timeout))?,
                    Err(_) => return Err(SteveError::Timeout {
                        timeout_ms: duration_millis(self.timeout),
                    }),
                }
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Err(SteveError::UpstreamStatus {
                status: status.as_u16(),
            });
        }
        if !is_event_stream(response.headers()) {
            let message = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("missing")
                .to_string();
            return Err(SteveError::UnexpectedContentType { message });
        }

        let timeout = self.timeout;
        Ok(response
            .bytes_stream()
            .map(move |result| result.map_err(|err| map_http_error(err, timeout))))
    }

    fn post_responses(&self, body: &Value, accept: &'static str) -> reqwest::RequestBuilder {
        self.http
            .post(responses_url(&self.base_url))
            .header(reqwest::header::ACCEPT, accept)
            .json(body)
    }
}

type UpstreamByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Unbuffered OpenAI SSE body. Dropping it aborts the upstream response.
pub struct OpenAiEventStream {
    inner: Option<UpstreamByteStream>,
    cancel_fut: Pin<Box<dyn Future<Output = ()> + Send>>,
    _cancel: CancelToken,
    finished: bool,
    timeout: Duration,
}

impl OpenAiEventStream {
    fn shutdown(&mut self) {
        self.finished = true;
        self.inner = None;
    }
}

impl std::fmt::Debug for OpenAiEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiEventStream")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Stream for OpenAiEventStream {
    type Item = Result<Bytes, SteveError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if this.cancel_fut.as_mut().poll(cx).is_ready() {
            this.shutdown();
            return Poll::Ready(Some(Err(SteveError::Cancelled)));
        }
        let Some(inner) = this.inner.as_mut() else {
            this.finished = true;
            return Poll::Ready(None);
        };
        match inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                this.shutdown();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(err))) => {
                let mapped = map_http_error(err, this.timeout);
                this.shutdown();
                Poll::Ready(Some(Err(mapped)))
            }
        }
    }
}

fn prepare_stream_body(request: &impl Serialize) -> Result<Value, SteveError> {
    let mut body = serde_json::to_value(request).map_err(|err| SteveError::Config {
        message: format!("chat completion request is not valid JSON: {err}"),
    })?;
    if body.get("stream").and_then(Value::as_bool) == Some(false) {
        return Err(SteveError::Config {
            message: "SSE chat completion request must not set stream to false".into(),
        });
    }
    let object = body.as_object_mut().ok_or_else(|| SteveError::Config {
        message: "chat completion request must be a JSON object".into(),
    })?;
    object.insert("stream".into(), Value::Bool(true));
    Ok(body)
}

fn prepare_responses_body(request: &impl Serialize, streaming: bool) -> Result<Value, SteveError> {
    let mut body = serde_json::to_value(request).map_err(|err| SteveError::Config {
        message: format!("Responses request is not valid JSON: {err}"),
    })?;
    if !streaming && body.get("stream").and_then(Value::as_bool) == Some(true) {
        return Err(SteveError::StreamingNotSupported);
    }
    if streaming {
        let object = body.as_object_mut().ok_or_else(|| SteveError::Config {
            message: "Responses request must be a JSON object".into(),
        })?;
        object.insert("stream".to_string(), Value::Bool(true));
    }
    Ok(body)
}

fn normalize_base_url(raw: String) -> Result<String, SteveError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(SteveError::Config {
            message: "base URL is empty".into(),
        });
    }

    let url = reqwest::Url::parse(&trimmed).map_err(|err| SteveError::Config {
        message: format!("base URL is invalid: {err}"),
    })?;
    validate_upstream_url(&trimmed, &url).map_err(|message| SteveError::Config {
        message: message.into(),
    })?;

    Ok(trimmed)
}

fn chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

fn responses_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/responses")
    } else {
        format!("{base}/v1/responses")
    }
}

fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(EVENT_STREAM))
}

fn map_http_error(err: reqwest::Error, timeout: Duration) -> SteveError {
    if err.is_timeout() {
        SteveError::Timeout {
            timeout_ms: duration_millis(timeout),
        }
    } else if err.is_decode() {
        SteveError::InvalidJson {
            message: err.to_string(),
        }
    } else {
        SteveError::Transport {
            message: err.to_string(),
        }
    }
}

fn duration_millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{chat_completions_url, responses_url};
    use crate::net::bind_listener;
    use crate::proxy::stream::{pump_upstream, CancelToken, ReplayForbidden, ReplayGate};
    use crate::proxy::{OpenAiUpstream, SteveError};
    use crate::test_upstream;
    use axum::{
        body::Body,
        http::{header, HeaderValue, StatusCode},
        response::IntoResponse,
        routing::post,
        Router,
    };
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::{sync::Arc, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    use tokio::sync::{mpsc, oneshot, Notify};
    use tokio_stream::{wrappers::ReceiverStream, StreamExt};

    #[test]
    fn base_url_joins_chat_completions_path() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:18080"),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:18080/"),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:18080/v1"),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:18080/v1/"),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
    }

    #[test]
    fn rejects_invalid_base_url_and_zero_timeout() {
        for base_url in ["", "   ", "/v1", "127.0.0.1:18080", "ftp://127.0.0.1/v1"] {
            let err = OpenAiUpstream::new(base_url, Duration::from_secs(1)).expect_err(base_url);
            match err {
                SteveError::Config { message } => assert!(!message.is_empty()),
                other => panic!("expected config error for {base_url}, got {other}"),
            }
        }

        let err = OpenAiUpstream::new("http://127.0.0.1:9", Duration::ZERO).expect_err("zero");
        match err {
            SteveError::Config { message } => assert!(message.contains("timeout")),
            other => panic!("expected config error, got {other}"),
        }
    }

    #[tokio::test]
    async fn round_trip_against_test_upstream() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .expect("serve test upstream");
        });

        let request = json!({
            "model": "steve-test-model",
            "messages": [{"role": "user", "content": "hi"}]
        });
        for base_url in [format!("http://{addr}"), format!("http://{addr}/v1")] {
            let client =
                OpenAiUpstream::new(base_url.as_str(), Duration::from_secs(2)).expect("client");
            assert_eq!(client.timeout(), Duration::from_secs(2));
            let body = client
                .chat_completion(&request)
                .await
                .unwrap_or_else(|err| panic!("round trip {base_url}: {err}"));
            assert_chat_fixture(&body);

            let stream = client
                .chat_completion_stream(
                    &json!({
                        "model": "steve-test-model",
                        "stream": true,
                        "messages": [{"role": "user", "content": "hi"}]
                    }),
                    CancelToken::new(),
                )
                .await
                .expect("sse round trip");
            assert_eq!(collect_stream(stream).await, expected_chat_sse());
        }

        server.abort();
    }

    #[test]
    fn base_url_joins_responses_path() {
        assert_eq!(
            responses_url("http://127.0.0.1:18080"),
            "http://127.0.0.1:18080/v1/responses"
        );
        assert_eq!(
            responses_url("http://127.0.0.1:18080/v1"),
            "http://127.0.0.1:18080/v1/responses"
        );
    }

    #[tokio::test]
    async fn responses_json_and_sse_round_trip_against_test_upstream() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .expect("serve test upstream");
        });

        for base_url in [format!("http://{addr}"), format!("http://{addr}/v1")] {
            let client =
                OpenAiUpstream::new(base_url.as_str(), Duration::from_secs(2)).expect("client");
            let body = client
                .create_response(&json!({"model": "responses-fixture", "input": "hi"}))
                .await
                .unwrap_or_else(|err| panic!("JSON response {base_url}: {err}"));
            assert_eq!(body["object"], "response");
            assert_eq!(body["model"], "responses-fixture");

            let mut stream = client
                .create_response_stream(
                    &json!({"model": "responses-fixture", "input": "hi"}),
                    CancelToken::new(),
                )
                .await
                .unwrap_or_else(|err| panic!("SSE response {base_url}: {err}"));
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(chunk) = stream.next().await {
                    bytes.extend_from_slice(&chunk.expect("SSE bytes"));
                }
            })
            .await
            .expect("SSE body completes");
            let body = String::from_utf8(bytes).expect("SSE utf-8");
            assert!(body.contains("event: response.created"));
            assert!(body.contains("event: response.output_text.delta"));
            assert!(body.contains("responses-fixture"));
        }

        server.abort();
    }

    #[tokio::test]
    async fn response_stream_body_continues_after_header_timeout() {
        let (addr, server) = spawn(Router::new().route(
            "/v1/responses",
            post(|| async {
                let (tx, rx) = tokio::sync::mpsc::channel(1);
                tokio::spawn(async move {
                    tx.send(Ok::<_, std::io::Error>(Bytes::from_static(b"first")))
                        .await
                        .expect("send first chunk");
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    tx.send(Ok(Bytes::from_static(b"last")))
                        .await
                        .expect("send final chunk");
                });

                let mut response =
                    Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx))
                        .into_response();
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/event-stream"),
                );
                response
            }),
        ))
        .await;
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_millis(100))
            .expect("client");
        let mut stream = client
            .create_response_stream(&json!({"model": "m"}), CancelToken::new())
            .await
            .expect("headers arrive before timeout");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("first chunk arrives")
                .expect("first chunk exists")
                .expect("first chunk succeeds"),
            Bytes::from_static(b"first")
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("delayed final chunk arrives")
                .expect("final chunk exists")
                .expect("final chunk succeeds"),
            Bytes::from_static(b"last")
        );
        assert!(stream.next().await.is_none());
        server.abort();
    }

    #[tokio::test]
    async fn response_stream_rejects_status_and_content_type() {
        let (addr, server) =
            spawn(Router::new().route("/v1/responses", post(|| async { StatusCode::BAD_GATEWAY })))
                .await;
        let client =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(1)).expect("client");
        assert!(matches!(
            client
                .create_response_stream(&json!({"model": "m"}), CancelToken::new())
                .await,
            Err(SteveError::UpstreamStatus { status: 502 })
        ));
        server.abort();

        let (addr, server) = spawn(Router::new().route(
            "/v1/responses",
            post(|| async { (StatusCode::OK, [("content-type", "application/json")], "{}") }),
        ))
        .await;
        let client =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(1)).expect("client");
        assert!(matches!(
            client
                .create_response_stream(&json!({"model": "m"}), CancelToken::new())
                .await,
            Err(SteveError::UnexpectedContentType { .. })
        ));
        server.abort();
    }

    #[tokio::test]
    async fn response_stream_header_wait_times_out_and_cancels() {
        let (addr, server) = spawn(Router::new().route(
            "/v1/responses",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                StatusCode::OK
            }),
        ))
        .await;
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_millis(100))
            .expect("client");
        assert!(matches!(
            client
                .create_response_stream(&json!({"model": "m"}), CancelToken::new())
                .await,
            Err(SteveError::Timeout { timeout_ms: 100 })
        ));
        server.abort();

        let (addr, server) = spawn(Router::new().route(
            "/v1/responses",
            post(|| async { std::future::pending::<StatusCode>().await }),
        ))
        .await;
        let client =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).expect("client");
        let cancel = CancelToken::new();
        let cancel_after = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel_after.cancel();
        });
        assert!(matches!(
            client
                .create_response_stream(&json!({"model": "m"}), cancel)
                .await,
            Err(SteveError::Cancelled)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn sse_yields_first_chunk_before_delayed_tail() {
        let (tail_tx, tail_rx) = oneshot::channel::<()>();
        let tail_rx = Arc::new(Mutex::new(Some(tail_rx)));
        let (addr, server) = spawn(Router::new().route(
            "/v1/chat/completions",
            post({
                let tail_rx = tail_rx.clone();
                move || {
                    let tail_rx = tail_rx.clone();
                    async move {
                        let (tx, rx) = mpsc::channel(2);
                        let _ = tx
                            .send(Ok::<_, std::io::Error>(Bytes::from_static(
                                b"data: first\n\n",
                            )))
                            .await;
                        let tail_rx = tail_rx.lock().await.take().unwrap();
                        tokio::spawn(async move {
                            let _ = tail_rx.await;
                            let _ = tx.send(Ok(Bytes::from_static(b"data: tail\n\n"))).await;
                        });
                        (
                            [("content-type", "text/event-stream")],
                            Body::from_stream(ReceiverStream::new(rx)),
                        )
                    }
                }
            }),
        ))
        .await;
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let mut stream = client
            .chat_completion_stream(&json!({"model":"m"}), CancelToken::new())
            .await
            .unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Bytes::from_static(b"data: first\n\n")
        );
        tail_tx.send(()).unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Bytes::from_static(b"data: tail\n\n")
        );
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_before_dial_header_wait_and_body() {
        let client = OpenAiUpstream::new("http://127.0.0.1:9", Duration::from_secs(1)).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            client
                .chat_completion_stream(&json!({"model":"m"}), cancel)
                .await,
            Err(SteveError::Cancelled)
        ));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(Notify::new());
        let accepted_signal = accepted.clone();
        let hold = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            accepted_signal.notify_one();
            std::future::pending::<()>().await;
        });
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(5)).unwrap();
        let cancel = CancelToken::new();
        let wait_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            client
                .chat_completion_stream(&json!({"model":"m"}), wait_cancel)
                .await
        });
        accepted.notified().await;
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(SteveError::Cancelled)));
        hold.abort();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut buf).await.unwrap();
                if count == 0 {
                    return Ok::<(), String>(());
                }
                request.extend_from_slice(&buf[..count]);
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let content_length = String::from_utf8_lossy(&request[..header_end])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then_some(value)
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("request must include a valid Content-Length");
            let mut remaining = content_length.saturating_sub(request.len() - header_end);
            while remaining > 0 {
                let count = socket.read(&mut buf).await.unwrap();
                if count == 0 {
                    return Ok(());
                }
                remaining = remaining.saturating_sub(count);
            }
            socket.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 1024\r\nconnection: close\r\n\r\ndata: first\n\n",
            ).await.unwrap();
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => Ok(()),
                Ok(count) => Err(format!("received {count} bytes after cancellation")),
            }
        });
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let cancel = CancelToken::new();
        let mut stream = client
            .chat_completion_stream(&json!({"model":"m"}), cancel.clone())
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        cancel.cancel();
        assert!(matches!(
            stream.next().await,
            Some(Err(SteveError::Cancelled))
        ));
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("upstream socket was not dropped")
            .unwrap()
            .expect("upstream socket received bytes instead of closing");
    }

    #[tokio::test]
    async fn sse_header_wait_is_bounded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(Notify::new());
        let accepted_signal = accepted.clone();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            accepted_signal.notify_one();
            std::future::pending::<()>().await;
        });
        let timeout = Duration::from_millis(100);
        let client = OpenAiUpstream::new(format!("http://{addr}"), timeout).unwrap();
        let task = tokio::spawn(async move {
            client
                .chat_completion_stream(&json!({"model":"m"}), CancelToken::new())
                .await
        });
        accepted.notified().await;
        let err = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("header timeout did not finish")
            .unwrap()
            .expect_err("missing headers should time out");
        assert!(matches!(err, SteveError::Timeout { timeout_ms: 100 }));
        server.abort();
    }

    #[tokio::test]
    async fn validates_sse_response_and_pumps_without_replay() {
        let (status_addr, status_server) = spawn(Router::new().route(
            "/v1/chat/completions",
            post(|| async { StatusCode::BAD_GATEWAY }),
        ))
        .await;
        let client =
            OpenAiUpstream::new(format!("http://{status_addr}"), Duration::from_secs(1)).unwrap();
        assert!(matches!(
            client
                .chat_completion_stream(&json!({"model":"m"}), CancelToken::new())
                .await,
            Err(SteveError::UpstreamStatus { status: 502 })
        ));
        status_server.abort();
        let (json_addr, json_server) = spawn(Router::new().route(
            "/v1/chat/completions",
            post(|| async { ([("content-type", "application/json")], "{}") }),
        ))
        .await;
        let client =
            OpenAiUpstream::new(format!("http://{json_addr}"), Duration::from_secs(1)).unwrap();
        assert!(matches!(
            client
                .chat_completion_stream(&json!({"model":"m"}), CancelToken::new())
                .await,
            Err(SteveError::UnexpectedContentType { .. })
        ));
        json_server.abort();

        let (addr, server) = spawn(test_upstream::router()).await;
        let client = OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(1)).unwrap();
        let cancel = CancelToken::new();
        let gate = ReplayGate::new();
        let stream = client
            .chat_completion_stream(&json!({"model":"steve-test-model"}), cancel.clone())
            .await
            .unwrap();
        let body = pump_upstream(&gate, cancel, stream)
            .unwrap()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            expected_chat_sse()
        );
        assert!(gate.output_begun());
        assert_eq!(
            pump_upstream(
                &gate,
                CancelToken::new(),
                tokio_stream::empty::<Result<Bytes, SteveError>>()
            )
            .err(),
            Some(ReplayForbidden)
        );
        server.abort();
    }

    #[tokio::test]
    async fn configured_timeout_maps_to_error_stub() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let _hold = socket;
            std::future::pending::<()>().await;
        });

        let timeout = Duration::from_millis(200);
        let client = OpenAiUpstream::new(format!("http://{addr}"), timeout).expect("client");
        assert_eq!(client.timeout(), timeout);

        let err = tokio::time::timeout(
            Duration::from_secs(3),
            client.chat_completion(&json!({"model": "steve-test-model", "messages": []})),
        )
        .await
        .expect("client timeout did not fire")
        .expect_err("hung upstream should fail");

        let message = err.to_string();
        match err {
            SteveError::Timeout { timeout_ms } => {
                assert_eq!(timeout_ms, 200);
                assert!(message.contains("timed out"));
            }
            other => panic!("expected timeout, got {other}"),
        }
    }

    #[tokio::test]
    async fn maps_transport_status_and_invalid_json() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let closed = listener.local_addr().expect("local addr");
        drop(listener);

        let client = OpenAiUpstream::new(format!("http://{closed}"), Duration::from_secs(1))
            .expect("client");
        let err = client
            .chat_completion(&json!({"model": "m"}))
            .await
            .expect_err("refused");
        match err {
            SteveError::Transport { message } => assert!(!message.is_empty()),
            other => panic!("expected transport error, got {other}"),
        }

        let (status_addr, status_server) = spawn(Router::new().route(
            "/v1/chat/completions",
            post(|| async { StatusCode::BAD_GATEWAY }),
        ))
        .await;
        let client = OpenAiUpstream::new(format!("http://{status_addr}"), Duration::from_secs(1))
            .expect("client");
        let err = client
            .chat_completion(&json!({"model": "m"}))
            .await
            .expect_err("status");
        match err {
            SteveError::UpstreamStatus { status } => assert_eq!(status, 502),
            other => panic!("expected status error, got {other}"),
        }
        status_server.abort();

        let (json_addr, json_server) = spawn(Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    "not-json",
                )
            }),
        ))
        .await;
        let client = OpenAiUpstream::new(format!("http://{json_addr}"), Duration::from_secs(1))
            .expect("client");
        let err = client
            .chat_completion(&json!({"model": "m"}))
            .await
            .expect_err("json");
        match err {
            SteveError::InvalidJson { message } => assert!(!message.is_empty()),
            other => panic!("expected invalid json, got {other}"),
        }
        json_server.abort();
    }

    #[tokio::test]
    async fn rejects_streaming_requests_before_dialing() {
        let client =
            OpenAiUpstream::new("http://127.0.0.1:9", Duration::from_secs(1)).expect("client");
        let err = client
            .chat_completion(&json!({"model": "m", "stream": true}))
            .await
            .expect_err("stream");
        assert!(matches!(err, SteveError::StreamingNotSupported));
    }

    fn assert_chat_fixture(body: &Value) {
        assert_eq!(body["id"], "chatcmpl-steve-test");
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["model"], "steve-test-model");
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "steve-test-response"
        );
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["prompt_tokens"], 10);
        assert_eq!(body["usage"]["completion_tokens"], 3);
        assert_eq!(body["usage"]["total_tokens"], 13);
    }

    async fn spawn(app: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (addr, handle)
    }

    async fn collect_stream(mut stream: super::OpenAiEventStream) -> String {
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(body).unwrap()
    }

    fn expected_chat_sse() -> String {
        let mut body = String::new();
        for (index, content) in ["steve-test-", "response"].iter().enumerate() {
            let mut delta = json!({"content": content});
            if index == 0 {
                delta["role"] = json!("assistant");
            }
            let finish_reason = if index == 1 {
                json!("stop")
            } else {
                Value::Null
            };
            body.push_str(&format!("data: {}\n\n", json!({"id":"chatcmpl-steve-test","object":"chat.completion.chunk","created":0,"model":"steve-test-model","choices":[{"index":0,"delta":delta,"finish_reason":finish_reason}]})));
        }
        body.push_str("data: [DONE]\n\n");
        body
    }
}
