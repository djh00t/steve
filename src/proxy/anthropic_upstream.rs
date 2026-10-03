//! Anthropic-compatible Messages client (JSON and SSE).
//!
//! Posts JSON to `{base_url}/v1/messages` (or `{base_url}/messages` when the
//! base URL already ends in `/v1`). Non-streaming calls parse the JSON body.
//! Streaming calls return the SSE bytes as they arrive and do not buffer the
//! upstream body. The base URL must be absolute `https` or numeric-loopback
//! `http`.

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
use tokio_stream::Stream;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const EVENT_STREAM: &str = "text/event-stream";

/// Stub of Steve's structured error model.
///
/// Only failures this client can observe are represented. Later proxy stages
/// extend the same model.
#[derive(Debug, thiserror::Error)]
pub enum AnthropicError {
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

    #[error("streaming messages are not supported by the non-streaming client")]
    StreamingNotSupported,

    #[error("upstream response content-type is not text/event-stream: {message}")]
    UnexpectedContentType { message: String },

    #[error("upstream request was cancelled")]
    Cancelled,
}

/// HTTP client for one Anthropic-compatible Messages origin.
#[derive(Clone, Debug)]
pub struct AnthropicUpstream {
    base_url: String,
    timeout: Duration,
    http: reqwest::Client,
}

impl AnthropicUpstream {
    /// `base_url` is an absolute `https` or numeric-loopback `http` origin,
    /// with an optional Anthropic-style `/v1` root and no embedded credentials.
    /// `timeout` bounds the non-streaming call and, for SSE, the wait for
    /// response headers. SSE body reads are not cut off by that timeout.
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self, AnthropicError> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // A cancelled SSE body must not be returned to the pool.
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|err| AnthropicError::Config {
                message: format!("http client: {err}"),
            })?;
        Self::with_client(base_url, timeout, http)
    }

    pub(crate) fn with_client(
        base_url: impl Into<String>,
        timeout: Duration,
        http: reqwest::Client,
    ) -> Result<Self, AnthropicError> {
        let base_url = normalize_base_url(base_url.into())?;
        if timeout.is_zero() {
            return Err(AnthropicError::Config {
                message: "timeout must be greater than zero".into(),
            });
        }

        Ok(Self {
            base_url,
            timeout,
            http,
        })
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// POST a non-streaming Messages request and return the parsed JSON body.
    ///
    /// `stream: true` is rejected before a connection is opened.
    pub async fn create_message(&self, request: &impl Serialize) -> Result<Value, AnthropicError> {
        let mut headers = reqwest::header::HeaderMap::new();
        self.create_message_with_headers(request, &mut headers)
            .await
    }

    pub(crate) async fn create_message_with_headers(
        &self,
        request: &impl Serialize,
        headers: &mut reqwest::header::HeaderMap,
    ) -> Result<Value, AnthropicError> {
        headers.clear();
        let body = prepare_body(request, false)?;
        let response = self
            .post_json(&body, "application/json")
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|err| map_http_error(err, self.timeout))?;

        *headers = super::safe_cache_headers(response.headers());
        let status = response.status();
        if !status.is_success() {
            return Err(AnthropicError::UpstreamStatus {
                status: status.as_u16(),
            });
        }

        response
            .json()
            .await
            .map_err(|err| map_http_error(err, self.timeout))
    }

    /// POST a streaming Messages request and return the SSE body as bytes.
    ///
    /// The returned stream yields each chunk reqwest has already read. It does
    /// not assemble or buffer the full event stream. `cancel` aborts the header
    /// wait and, once the body is open, drops the upstream body on the next poll.
    /// The same token is what [`super::stream::pump_upstream`] signals when the
    /// client disconnects.
    pub async fn create_message_stream(
        &self,
        request: &impl Serialize,
        cancel: CancelToken,
    ) -> Result<AnthropicEventStream, AnthropicError> {
        let mut headers = reqwest::header::HeaderMap::new();
        self.create_message_stream_with_headers(request, cancel, &mut headers)
            .await
    }

    pub(crate) async fn create_message_stream_with_headers(
        &self,
        request: &impl Serialize,
        cancel: CancelToken,
        headers: &mut reqwest::header::HeaderMap,
    ) -> Result<AnthropicEventStream, AnthropicError> {
        headers.clear();
        if cancel.is_cancelled() {
            return Err(AnthropicError::Cancelled);
        }

        let body = prepare_body(request, true)?;
        let send = self.post_json(&body, EVENT_STREAM).send();
        let cancel_for_wait = cancel.clone();
        let response = tokio::select! {
            biased;
            () = cancel_for_wait.cancelled() => return Err(AnthropicError::Cancelled),
            response = tokio::time::timeout(self.timeout, send) => {
                match response {
                    Ok(result) => result.map_err(|err| map_http_error(err, self.timeout))?,
                    Err(_) => {
                        return Err(AnthropicError::Timeout {
                            timeout_ms: duration_millis(self.timeout),
                        });
                    }
                }
            }
        };

        *headers = super::safe_cache_headers(response.headers());
        let status = response.status();
        if !status.is_success() {
            return Err(AnthropicError::UpstreamStatus {
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
            return Err(AnthropicError::UnexpectedContentType { message });
        }

        Ok(AnthropicEventStream {
            inner: Some(Box::pin(response.bytes_stream())),
            cancel_fut: Box::pin(cancel.cancelled_future()),
            _cancel: cancel,
            finished: false,
            timeout: self.timeout,
        })
    }

    fn post_json(&self, body: &Value, accept: &'static str) -> reqwest::RequestBuilder {
        self.http
            .post(messages_url(&self.base_url))
            .header(reqwest::header::ACCEPT, accept)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(body)
    }
}

type UpstreamByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Unbuffered Anthropic SSE body. Dropping it aborts the upstream response.
pub struct AnthropicEventStream {
    inner: Option<UpstreamByteStream>,
    cancel_fut: Pin<Box<dyn Future<Output = ()> + Send>>,
    _cancel: CancelToken,
    finished: bool,
    timeout: Duration,
}

impl AnthropicEventStream {
    fn shutdown(&mut self) {
        self.finished = true;
        self.inner = None;
    }
}

impl std::fmt::Debug for AnthropicEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnthropicEventStream")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Stream for AnthropicEventStream {
    type Item = Result<Bytes, AnthropicError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        if this.cancel_fut.as_mut().poll(cx).is_ready() {
            this.shutdown();
            tracing::debug!(
                event = "anthropic_upstream_cancelled",
                "aborting upstream messages stream"
            );
            return Poll::Ready(Some(Err(AnthropicError::Cancelled)));
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

fn prepare_body(request: &impl Serialize, streaming: bool) -> Result<Value, AnthropicError> {
    let mut body = serde_json::to_value(request).map_err(|err| AnthropicError::Config {
        message: format!("messages request is not valid JSON: {err}"),
    })?;
    let stream_flag = body.get("stream").and_then(Value::as_bool);
    if !streaming && stream_flag == Some(true) {
        return Err(AnthropicError::StreamingNotSupported);
    }
    if streaming && stream_flag == Some(false) {
        return Err(AnthropicError::Config {
            message: "SSE messages request must not set stream to false".into(),
        });
    }
    if streaming {
        let object = body.as_object_mut().ok_or_else(|| AnthropicError::Config {
            message: "messages request must be a JSON object".into(),
        })?;
        object.insert("stream".to_string(), Value::Bool(true));
    }
    Ok(body)
}

fn normalize_base_url(raw: String) -> Result<String, AnthropicError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(AnthropicError::Config {
            message: "base URL is empty".into(),
        });
    }

    let url = reqwest::Url::parse(&trimmed).map_err(|err| AnthropicError::Config {
        message: format!("base URL is invalid: {err}"),
    })?;
    validate_upstream_url(&trimmed, &url).map_err(|message| AnthropicError::Config {
        message: message.into(),
    })?;

    Ok(trimmed)
}

fn messages_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(EVENT_STREAM))
}

fn map_http_error(err: reqwest::Error, timeout: Duration) -> AnthropicError {
    if err.is_timeout() {
        AnthropicError::Timeout {
            timeout_ms: duration_millis(timeout),
        }
    } else if err.is_decode() {
        AnthropicError::InvalidJson {
            message: err.to_string(),
        }
    } else {
        AnthropicError::Transport {
            message: err.to_string(),
        }
    }
}

fn duration_millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::messages_url;
    use crate::net::bind_listener;
    use crate::proxy::stream::{pump_upstream, CancelToken, ReplayForbidden, ReplayGate};
    use crate::proxy::{AnthropicError, AnthropicEventStream, AnthropicUpstream};
    use crate::test_upstream;
    use axum::{http::StatusCode, routing::post, Router};
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use std::{
        net::SocketAddr,
        pin::Pin,
        task::{Context, Poll},
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::oneshot,
    };
    use tokio_stream::Stream;
    use tokio_stream::StreamExt;

    const MESSAGE_ID: &str = "msg_steve_test";
    const MESSAGE_TEXT: &str = "steve-test-response";
    const FIRST_EVENT: &str = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n";
    const REST_EVENT: &str = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    #[test]
    fn base_url_joins_messages_path() {
        assert_eq!(
            messages_url("http://127.0.0.1:18080"),
            "http://127.0.0.1:18080/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:18080/"),
            "http://127.0.0.1:18080/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:18080/v1"),
            "http://127.0.0.1:18080/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:18080/v1/"),
            "http://127.0.0.1:18080/v1/messages"
        );
    }

    #[test]
    fn rejects_invalid_base_url_and_zero_timeout() {
        for base_url in ["", "   ", "/v1", "127.0.0.1:18080", "ftp://127.0.0.1/v1"] {
            let err = AnthropicUpstream::new(base_url, Duration::from_secs(1)).expect_err(base_url);
            match err {
                AnthropicError::Config { message } => assert!(!message.is_empty()),
                other => panic!("expected config error for {base_url}, got {other}"),
            }
        }

        let err = AnthropicUpstream::new("http://127.0.0.1:9", Duration::ZERO).expect_err("zero");
        match err {
            AnthropicError::Config { message } => assert!(message.contains("timeout")),
            other => panic!("expected config error, got {other}"),
        }
    }

    #[tokio::test]
    async fn round_trip_json_and_sse_against_test_upstream() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let server = AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .expect("serve test upstream");
        }));

        let request = json!({
            "model": "claude-fixture",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}]
        });
        for base_url in [format!("http://{addr}"), format!("http://{addr}/v1")] {
            let client =
                AnthropicUpstream::new(base_url.as_str(), Duration::from_secs(2)).expect("client");
            assert_eq!(client.timeout(), Duration::from_secs(2));

            let body = client
                .create_message(&request)
                .await
                .unwrap_or_else(|err| panic!("json round trip {base_url}: {err}"));
            assert_message_fixture(&body, "claude-fixture");

            let mut sse_request = request.clone();
            sse_request["stream"] = json!(true);
            let stream = client
                .create_message_stream(&sse_request, CancelToken::new())
                .await
                .unwrap_or_else(|err| panic!("sse round trip {base_url}: {err}"));
            let sse_body = collect_stream(stream).await;
            assert_message_sse(&sse_body, "claude-fixture");
        }

        drop(server);
    }

    #[tokio::test]
    async fn sse_does_not_buffer_the_rest_of_the_body() {
        let (addr, release_tx, server) = spawn_chunked_sse(ChunkHold::Release).await;
        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2))
            .expect("client");
        let mut stream = client
            .create_message_stream(
                &json!({"model": "claude-fixture", "max_tokens": 16, "messages": []}),
                CancelToken::new(),
            )
            .await
            .expect("headers before the body finishes");

        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !String::from_utf8_lossy(&got).contains("message_start") {
                let chunk = stream
                    .next()
                    .await
                    .expect("first sse bytes")
                    .expect("first chunk");
                got.extend_from_slice(&chunk);
            }
        })
        .await
        .expect("first event must arrive before the rest of the body is sent");
        let partial = String::from_utf8(got).expect("utf-8");
        assert!(partial.contains("message_start"));
        assert!(
            !partial.contains("message_stop"),
            "client buffered the unread tail: {partial}"
        );

        release_tx
            .expect("release sender")
            .send(())
            .expect("release server");
        let mut rest = partial.into_bytes();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(chunk) = stream.next().await {
                rest.extend_from_slice(&chunk.expect("rest chunk"));
            }
        })
        .await
        .expect("remainder of the sse body");
        let body = String::from_utf8(rest).expect("utf-8");
        assert!(body.contains("message_stop"));
        server.await.expect("server").expect("chunked sse");
    }

    #[tokio::test]
    async fn cancel_token_aborts_sse_upstream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let cancel = CancelToken::new();
        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(5))
            .expect("client");

        let pre_cancelled = cancel.clone();
        pre_cancelled.cancel();
        let err = client
            .create_message_stream(&json!({"model": "m"}), pre_cancelled)
            .await
            .expect_err("already cancelled");
        assert!(matches!(err, AnthropicError::Cancelled));
        assert_eq!(err.to_string(), "upstream request was cancelled");
        let accepted = tokio::time::timeout(Duration::from_millis(150), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "a cancelled request must not open a connection"
        );

        let (addr, _release, server) = spawn_chunked_sse(ChunkHold::UntilDisconnect).await;
        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(5))
            .expect("client");
        let cancel = CancelToken::new();
        let mut stream = client
            .create_message_stream(&json!({"model": "m", "messages": []}), cancel.clone())
            .await
            .expect("open sse");
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first chunk timeout")
            .expect("first chunk")
            .expect("first chunk bytes");
        assert!(String::from_utf8_lossy(&first).contains("message_start"));

        cancel.cancel();
        let cancelled = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("cancel did not wake the stream")
            .expect("cancel item");
        assert!(matches!(cancelled, Err(AnthropicError::Cancelled)));
        drop(stream);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("dropping the stream did not abort the upstream")
            .expect("server task")
            .expect("upstream disconnect");
    }

    #[tokio::test]
    async fn pump_upstream_forwards_sse_bytes() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let server = AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, test_upstream::router())
                .await
                .expect("serve test upstream");
        }));

        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2))
            .expect("client");
        let cancel = CancelToken::new();
        let gate = ReplayGate::new();
        let stream = client
            .create_message_stream(
                &json!({
                    "model": "claude-fixture",
                    "max_tokens": 16,
                    "stream": true,
                    "messages": [{"role": "user", "content": "ping"}]
                }),
                cancel.clone(),
            )
            .await
            .expect("sse");
        let body = pump_upstream(&gate, cancel, stream).expect("pump");
        let collected = tokio::time::timeout(Duration::from_secs(2), body.collect())
            .await
            .expect("pump collect")
            .expect("body");
        assert_message_sse(
            std::str::from_utf8(&collected.to_bytes()).expect("utf-8"),
            "claude-fixture",
        );
        assert!(gate.output_begun());
        assert!(gate.ensure_can_attempt().is_err());

        let forbidden = pump_upstream(&gate, CancelToken::new(), UnpolledUpstream);
        assert_eq!(forbidden.err(), Some(ReplayForbidden));
        drop(server);
    }

    #[tokio::test]
    async fn configured_timeout_maps_to_error_stub() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let hold = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let _hold = socket;
                    std::future::pending::<()>().await;
                });
            }
        });

        let timeout = Duration::from_millis(200);
        let client = AnthropicUpstream::new(format!("http://{addr}"), timeout).expect("client");
        assert_eq!(client.timeout(), timeout);

        let err = tokio::time::timeout(
            Duration::from_secs(3),
            client.create_message(&json!({"model": "claude-fixture", "messages": []})),
        )
        .await
        .expect("client timeout did not fire")
        .expect_err("hung upstream should fail");
        assert_timeout(err, 200);

        let err = tokio::time::timeout(
            Duration::from_secs(3),
            client.create_message_stream(
                &json!({"model": "claude-fixture", "messages": []}),
                CancelToken::new(),
            ),
        )
        .await
        .expect("sse header timeout did not fire")
        .expect_err("hung sse upstream should fail");
        assert_timeout(err, 200);
        hold.abort();
    }

    #[tokio::test]
    async fn maps_transport_status_invalid_json_and_content_type() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let closed = listener.local_addr().expect("local addr");
        drop(listener);

        let client = AnthropicUpstream::new(format!("http://{closed}"), Duration::from_secs(1))
            .expect("client");
        let err = client
            .create_message(&json!({"model": "m"}))
            .await
            .expect_err("refused");
        match err {
            AnthropicError::Transport { message } => assert!(!message.is_empty()),
            other => panic!("expected transport error, got {other}"),
        }

        let (status_addr, status_server) =
            spawn(Router::new().route("/v1/messages", post(|| async { StatusCode::BAD_GATEWAY })))
                .await;
        let client =
            AnthropicUpstream::new(format!("http://{status_addr}"), Duration::from_secs(1))
                .expect("client");
        let err = client
            .create_message(&json!({"model": "m"}))
            .await
            .expect_err("status");
        match err {
            AnthropicError::UpstreamStatus { status } => assert_eq!(status, 502),
            other => panic!("expected status error, got {other}"),
        }
        let err = client
            .create_message_stream(&json!({"model": "m"}), CancelToken::new())
            .await
            .expect_err("sse status");
        match err {
            AnthropicError::UpstreamStatus { status } => assert_eq!(status, 502),
            other => panic!("expected sse status error, got {other}"),
        }
        status_server.abort();

        let (json_addr, json_server) = spawn(Router::new().route(
            "/v1/messages",
            post(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    "not-json",
                )
            }),
        ))
        .await;
        let client = AnthropicUpstream::new(format!("http://{json_addr}"), Duration::from_secs(1))
            .expect("client");
        let err = client
            .create_message(&json!({"model": "m"}))
            .await
            .expect_err("json");
        match err {
            AnthropicError::InvalidJson { message } => assert!(!message.is_empty()),
            other => panic!("expected invalid json, got {other}"),
        }
        let err = client
            .create_message_stream(&json!({"model": "m"}), CancelToken::new())
            .await
            .expect_err("content type");
        match err {
            AnthropicError::UnexpectedContentType { message } => {
                assert!(message.to_ascii_lowercase().contains("application/json"));
            }
            other => panic!("expected content-type error, got {other}"),
        }
        json_server.abort();
    }

    #[tokio::test]
    async fn rejects_stream_flag_mismatch_before_dialing() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(1))
            .expect("client");

        let err = client
            .create_message(&json!({"model": "m", "stream": true}))
            .await
            .expect_err("stream");
        assert!(matches!(err, AnthropicError::StreamingNotSupported));

        let err = client
            .create_message_stream(&json!({"model": "m", "stream": false}), CancelToken::new())
            .await
            .expect_err("not stream");
        match err {
            AnthropicError::Config { message } => assert!(message.contains("stream")),
            other => panic!("expected config error, got {other}"),
        }

        let accepted = tokio::time::timeout(Duration::from_millis(150), listener.accept()).await;
        assert!(accepted.is_err(), "rejected requests must not connect");
    }

    #[tokio::test]
    async fn cancel_during_header_wait_maps_to_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let hold = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let _hold = socket;
            std::future::pending::<()>().await;
        });

        let client = AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(5))
            .expect("client");
        let cancel = CancelToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            client
                .create_message_stream(&json!({"model": "m"}), task_cancel)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        let err = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("header cancel did not finish")
            .expect("task")
            .expect_err("cancelled");
        assert!(matches!(err, AnthropicError::Cancelled));
        hold.abort();
    }

    fn assert_timeout(err: AnthropicError, timeout_ms: u64) {
        let message = err.to_string();
        match err {
            AnthropicError::Timeout { timeout_ms: actual } => {
                assert_eq!(actual, timeout_ms);
                assert!(message.contains("timed out"));
            }
            other => panic!("expected timeout, got {other}"),
        }
    }

    fn assert_message_fixture(body: &Value, model: &str) {
        assert_eq!(body["id"], MESSAGE_ID);
        assert_eq!(body["type"], "message");
        assert_eq!(body["role"], "assistant");
        assert_eq!(body["model"], model);
        assert_eq!(body["content"][0]["type"], "text");
        assert_eq!(body["content"][0]["text"], MESSAGE_TEXT);
        assert_eq!(body["stop_reason"], "end_turn");
        assert_eq!(body["usage"]["input_tokens"], 10);
        assert_eq!(body["usage"]["output_tokens"], 3);
    }

    fn assert_message_sse(body: &str, model: &str) {
        let events = sse_events(body);
        assert_eq!(events.len(), 6, "{body}");
        assert_eq!(events[0].0, "message_start");
        assert_eq!(events[0].1["message"]["id"], MESSAGE_ID);
        assert_eq!(events[0].1["message"]["model"], model);
        assert_eq!(events[1].0, "content_block_start");
        assert_eq!(events[2].0, "content_block_delta");
        assert_eq!(events[2].1["delta"]["text"], MESSAGE_TEXT);
        assert_eq!(events[3].0, "content_block_stop");
        assert_eq!(events[4].0, "message_delta");
        assert_eq!(events[4].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(events[5].0, "message_stop");
        assert_eq!(events[5].1["type"], "message_stop");
    }

    fn sse_events(body: &str) -> Vec<(String, Value)> {
        body.split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .map(|block| {
                let mut event = String::new();
                let mut data = String::new();
                for line in block.split('\n') {
                    let line = line.trim_end_matches('\r');
                    if let Some(rest) = line.strip_prefix("event: ") {
                        event = rest.to_string();
                    } else if let Some(rest) = line.strip_prefix("data: ") {
                        data = rest.to_string();
                    }
                }
                let parsed = serde_json::from_str(&data)
                    .unwrap_or_else(|err| panic!("sse data for {event}: {err}; block={block}"));
                (event, parsed)
            })
            .collect()
    }

    async fn collect_stream(mut stream: AnthropicEventStream) -> String {
        let mut body = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(chunk) = stream.next().await {
                body.extend_from_slice(&chunk.expect("sse chunk"));
            }
        })
        .await
        .expect("sse collect");
        String::from_utf8(body).expect("utf-8 sse")
    }

    async fn spawn(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
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

    enum ChunkHold {
        Release,
        UntilDisconnect,
    }

    async fn spawn_chunked_sse(
        hold: ChunkHold,
    ) -> (
        SocketAddr,
        Option<oneshot::Sender<()>>,
        tokio::task::JoinHandle<Result<(), String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        assert!(addr.ip().is_loopback());
        let (release_tx, release_rx) = oneshot::channel();
        let release_tx = match hold {
            ChunkHold::Release => Some(release_tx),
            ChunkHold::UntilDisconnect => None,
        };
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.map_err(|err| err.to_string())?;
            socket.set_nodelay(true).map_err(|err| err.to_string())?;
            let headers = read_http_request(&mut socket)
                .await
                .map_err(|err| err.to_string())?;
            let header_text = String::from_utf8_lossy(&headers);
            if !header_text.starts_with("POST /v1/messages ") {
                return Err(format!("unexpected request line: {header_text}"));
            }
            if !header_text
                .to_ascii_lowercase()
                .contains("anthropic-version: 2023-06-01")
            {
                return Err(format!("missing anthropic-version: {header_text}"));
            }
            if !header_text
                .to_ascii_lowercase()
                .contains("accept: text/event-stream")
            {
                return Err(format!("missing sse accept: {header_text}"));
            }
            if !header_text.contains("\"stream\":true") {
                return Err(format!("stream flag was not sent: {header_text}"));
            }

            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: text/event-stream\r\n\
                      cache-control: no-cache\r\n\
                      transfer-encoding: chunked\r\n\
                      connection: close\r\n\
                      \r\n",
                )
                .await
                .map_err(|err| err.to_string())?;
            write_chunk(&mut socket, FIRST_EVENT)
                .await
                .map_err(|err| err.to_string())?;

            match hold {
                ChunkHold::Release => {
                    if release_rx.await.is_ok() {
                        write_chunk(&mut socket, REST_EVENT)
                            .await
                            .map_err(|err| err.to_string())?;
                        socket
                            .write_all(b"0\r\n\r\n")
                            .await
                            .map_err(|err| err.to_string())?;
                        socket.flush().await.map_err(|err| err.to_string())?;
                    }
                    Ok(())
                }
                ChunkHold::UntilDisconnect => {
                    let mut buf = [0u8; 32];
                    loop {
                        match tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf))
                            .await
                        {
                            Ok(Ok(0)) | Ok(Err(_)) => return Ok(()),
                            Ok(Ok(_)) => continue,
                            Err(_) => return Err("upstream connection was not aborted".to_string()),
                        }
                    }
                }
            }
        });
        (addr, release_tx, server)
    }

    async fn read_http_request(socket: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof before headers",
                ));
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                break pos + 4;
            }
            if buf.len() > 65_536 {
                return Err(std::io::Error::other("request headers too large"));
            }
        };
        let length = content_length(&buf[..header_end]);
        while buf.len() < header_end + length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof before body",
                ));
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        Ok(buf)
    }

    fn content_length(headers: &[u8]) -> usize {
        let headers = String::from_utf8_lossy(headers);
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    async fn write_chunk(socket: &mut TcpStream, data: &str) -> std::io::Result<()> {
        let header = format!("{:x}\r\n", data.len());
        socket.write_all(header.as_bytes()).await?;
        socket.write_all(data.as_bytes()).await?;
        socket.write_all(b"\r\n").await?;
        socket.flush().await
    }

    struct AbortOnDrop(tokio::task::JoinHandle<()>);

    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    struct UnpolledUpstream;

    impl Stream for UnpolledUpstream {
        type Item = Result<Bytes, AnthropicError>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            panic!("replay must not poll a new upstream");
        }
    }
}
