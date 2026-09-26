//! Deterministic local upstream for development and tests. It never dials an
//! external network.
//!
//! Non-streaming chat completion (`stream` omitted or not `true`):
//!
//! ```text
//! curl -sS http://127.0.0.1:18080/v1/chat/completions \
//!   -H 'content-type: application/json' \
//!   -d '{"model":"steve-test-model","messages":[{"role":"user","content":"hi"}]}'
//! ```
//!
//! Streaming chat completion (`stream` set to `true`). The body is
//! `text/event-stream` with at least two `data:` chunks, then `data: [DONE]`.
//!
//! ```text
//! curl -N http://127.0.0.1:18080/v1/chat/completions \
//!   -H 'content-type: application/json' \
//!   -d '{"model":"steve-test-model","stream":true,"messages":[{"role":"user","content":"hi"}]}'
//! ```

use crate::{net::bind_listener, server::shutdown_signal};
use anyhow::Result;
use axum::{
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::net::SocketAddr;

const CHAT_ID: &str = "chatcmpl-steve-test";
const CHAT_CONTENT: &str = "steve-test-response";
const DEFAULT_MODEL: &str = "steve-test-model";
const CHAT_CONTENT_CHUNKS: [&str; 2] = ["steve-test-", "response"];

pub async fn run(listen: SocketAddr) -> Result<()> {
    let listener = bind_listener(listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(event = "test_upstream_ready", %local_addr, "deterministic test upstream ready");

    axum::serve(listener, router())
        .with_graceful_shutdown(async {
            let _ = shutdown_signal().await;
        })
        .await?;

    Ok(())
}

fn router() -> Router {
    Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(anthropic_messages))
}

async fn chat_completions(Json(request): Json<Value>) -> Response {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL);

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        return chat_completion_stream(model);
    }

    Json(chat_completion_json(model)).into_response()
}

fn chat_completion_json(model: &str) -> Value {
    json!({
        "id": CHAT_ID,
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": CHAT_CONTENT
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 3,
            "total_tokens": 13
        }
    })
}

fn chat_completion_stream(model: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, chat_sse_payload(model)).into_response()
}

fn chat_sse_payload(model: &str) -> String {
    let mut body = String::new();
    for (index, content) in CHAT_CONTENT_CHUNKS.iter().enumerate() {
        let mut delta = json!({ "content": content });
        if index == 0 {
            delta["role"] = json!("assistant");
        }
        let finish_reason = if index + 1 == CHAT_CONTENT_CHUNKS.len() {
            json!("stop")
        } else {
            Value::Null
        };
        let chunk = json!({
            "id": CHAT_ID,
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason
            }]
        });
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

async fn responses(Json(request): Json<Value>) -> Json<Value> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("steve-test-model");

    Json(json!({
        "id": "resp_steve_test",
        "object": "response",
        "status": "completed",
        "model": model,
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "steve-test-response"
            }]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3,
            "total_tokens": 13
        }
    }))
}

async fn anthropic_messages(Json(request): Json<Value>) -> Json<Value> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("steve-test-model");

    Json(json!({
        "id": "msg_steve_test",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{
            "type": "text",
            "text": "steve-test-response"
        }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        time::{timeout, Duration},
    };

    #[tokio::test]
    async fn chat_completions_json_and_sse_on_bound_addr() {
        assert_eq!(
            CHAT_CONTENT_CHUNKS.concat(),
            CHAT_CONTENT,
            "sse chunks must concatenate to the fixed completion"
        );

        let listener = bind_listener("127.0.0.1:0".parse().unwrap())
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        assert!(
            addr.ip().is_loopback(),
            "test dials only the bound loopback"
        );

        let server = tokio::spawn(async move {
            axum::serve(listener, router()).await.expect("serve");
        });

        let omitted = post_chat(addr, r#"{"model":"steve-test-model"}"#).await;
        assert_json_completion(&omitted);

        let disabled = post_chat(addr, r#"{"model":"other-model","stream":false}"#).await;
        assert_json_completion(&disabled);
        assert_eq!(disabled.1["model"], "other-model");

        let not_bool = post_chat(addr, r#"{"stream":"true"}"#).await;
        assert_json_completion(&not_bool);
        assert_eq!(not_bool.1["model"], DEFAULT_MODEL);

        let (sse_head, sse_body) =
            post_raw(addr, r#"{"model":"steve-test-model","stream":true}"#).await;
        assert!(sse_head.starts_with("HTTP/1.1 200"), "{sse_head}");
        assert!(
            sse_head
                .to_ascii_lowercase()
                .contains("content-type: text/event-stream"),
            "sse content-type: {sse_head}"
        );
        let events = sse_data(&sse_body);
        assert!(
            events.len() >= 3,
            "expected >=2 data chunks plus [DONE], got {events:?}"
        );
        assert_eq!(events.last().copied(), Some("data: [DONE]"));

        let mut content = String::new();
        for event in &events[..events.len() - 1] {
            let json = event
                .strip_prefix("data: ")
                .expect("data prefix")
                .parse::<Value>()
                .expect("chunk json");
            assert_eq!(json["id"], CHAT_ID);
            assert_eq!(json["object"], "chat.completion.chunk");
            assert_eq!(json["created"], 0);
            assert_eq!(json["model"], "steve-test-model");
            if let Some(text) = json["choices"][0]["delta"]["content"].as_str() {
                content.push_str(text);
            }
        }
        assert_eq!(content, CHAT_CONTENT);
        assert_eq!(
            events[events.len() - 2]
                .strip_prefix("data: ")
                .unwrap()
                .parse::<Value>()
                .unwrap()["choices"][0]["finish_reason"],
            "stop"
        );

        server.abort();
    }

    fn assert_json_completion(response: &(String, Value)) {
        let (head, body) = response;
        assert!(
            head.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "json content-type: {head}"
        );
        assert!(!head.to_ascii_lowercase().contains("text/event-stream"));
        assert_eq!(body["id"], CHAT_ID);
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["created"], 0);
        assert_eq!(body["choices"][0]["message"]["role"], "assistant");
        assert_eq!(body["choices"][0]["message"]["content"], CHAT_CONTENT);
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["prompt_tokens"], 10);
        assert_eq!(body["usage"]["completion_tokens"], 3);
        assert_eq!(body["usage"]["total_tokens"], 13);
    }

    fn sse_data(body: &str) -> Vec<&str> {
        body.lines()
            .map(str::trim)
            .filter(|line| line.starts_with("data:"))
            .collect()
    }

    async fn post_chat(addr: SocketAddr, json_body: &str) -> (String, Value) {
        let (head, body) = post_raw(addr, json_body).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let parsed = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("json body: {error}: {body}");
        });
        (head, parsed)
    }

    async fn post_raw(addr: SocketAddr, json_body: &str) -> (String, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect loopback");
        let len = json_body.len();
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{json_body}"
        );
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut raw = Vec::new();
        timeout(Duration::from_secs(2), stream.read_to_end(&mut raw))
            .await
            .expect("response timed out")
            .expect("read");
        let raw = String::from_utf8(raw).expect("utf8");
        let (head, body) = raw.split_once("\r\n\r\n").expect("http headers");
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            decode_chunked(body)
        } else {
            body.to_string()
        };
        (head.to_string(), body)
    }

    fn decode_chunked(mut body: &str) -> String {
        let mut out = String::new();
        loop {
            let (size_line, rest) = body.split_once("\r\n").expect("chunk size");
            let size = usize::from_str_radix(size_line.trim(), 16).expect("chunk len");
            if size == 0 {
                break;
            }
            out.push_str(&rest[..size]);
            body = &rest[size..];
            if let Some(stripped) = body.strip_prefix("\r\n") {
                body = stripped;
            }
        }
        out
    }
}
