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
//!
//! Non-streaming Anthropic Messages (`stream` omitted or not `true`):
//!
//! ```text
//! curl -sS http://127.0.0.1:18080/v1/messages \
//!   -H 'content-type: application/json' \
//!   -d '{"model":"steve-test-model","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}'
//! ```
//!
//! Streaming Anthropic Messages (`stream` set to `true`). The body is
//! `text/event-stream` with a complete text block lifecycle: `message_start`,
//! `content_block_start`, `content_block_delta`, `content_block_stop`,
//! `message_delta`, `message_stop`.
//!
//! ```text
//! curl -N http://127.0.0.1:18080/v1/messages \
//!   -H 'content-type: application/json' \
//!   -d '{"model":"steve-test-model","max_tokens":16,"stream":true,"messages":[{"role":"user","content":"hi"}]}'
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
const MESSAGE_ID: &str = "msg_steve_test";
const MESSAGE_TEXT: &str = "steve-test-response";
const RESPONSE_ID: &str = "resp_steve_test";
const RESPONSE_TEXT: &str = "steve-test-response";
const RESPONSE_TEXT_CHUNKS: [&str; 2] = ["steve-test-", "response"];

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

/// Routes for the deterministic test upstream. Tests serve this on loopback.
pub(crate) fn router() -> Router {
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

async fn responses(Json(request): Json<Value>) -> Response {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL);

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        return responses_stream(model);
    }

    Json(response_json(model)).into_response()
}

fn response_json(model: &str) -> Value {
    json!({
        "id": RESPONSE_ID,
        "object": "response",
        "created_at": 0,
        "status": "completed",
        "model": model,
        "output": [{
            "id": "msg_response_fixture",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": RESPONSE_TEXT,
                "annotations": []
            }]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3,
            "total_tokens": 13
        }
    })
}

fn responses_stream(model: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    let completed = response_json(model);
    let mut created = completed.clone();
    created["status"] = json!("in_progress");
    created["output"] = json!([]);
    created["usage"] = Value::Null;
    let item = completed["output"][0].clone();
    let part = item["content"][0].clone();
    let item_id = item["id"].clone();
    let mut events = vec![
        json!({"type": "response.created", "response": created}),
        json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"id": item_id, "type": "message", "role": "assistant",
                "status": "in_progress", "content": []}}),
        json!({"type": "response.content_part.added", "item_id": item_id,
            "output_index": 0, "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []}}),
    ];
    for chunk in RESPONSE_TEXT_CHUNKS {
        events.push(
            json!({"type": "response.output_text.delta", "item_id": item_id,
            "output_index": 0, "content_index": 0, "delta": chunk, "logprobs": []}),
        );
    }
    events.extend([
        json!({"type": "response.output_text.done", "item_id": item_id,
            "output_index": 0, "content_index": 0, "text": RESPONSE_TEXT, "logprobs": []}),
        json!({"type": "response.content_part.done", "item_id": item_id,
            "output_index": 0, "content_index": 0, "part": part}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
        json!({"type": "response.completed", "response": completed}),
    ]);
    let mut body = String::new();
    for (sequence, mut event) in events.into_iter().enumerate() {
        event["sequence_number"] = json!(sequence);
        body.push_str(&sse_event(event["type"].as_str().unwrap(), &event));
    }
    (headers, body).into_response()
}

async fn anthropic_messages(Json(request): Json<Value>) -> Response {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL);

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        return anthropic_message_stream(model);
    }

    Json(anthropic_message_json(model)).into_response()
}

fn anthropic_message_json(model: &str) -> Value {
    json!({
        "id": MESSAGE_ID,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{
            "type": "text",
            "text": MESSAGE_TEXT
        }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3
        }
    })
}

fn anthropic_message_stream(model: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, anthropic_message_sse(model)).into_response()
}

fn anthropic_message_sse(model: &str) -> String {
    let mut body = String::new();
    body.push_str(&sse_event(
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": MESSAGE_ID,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 0
                }
            }
        }),
    ));
    body.push_str(&sse_event(
        "content_block_start",
        &json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
    ));
    body.push_str(&sse_event(
        "content_block_delta",
        &json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": MESSAGE_TEXT
            }
        }),
    ));
    body.push_str(&sse_event(
        "content_block_stop",
        &json!({"type": "content_block_stop", "index": 0}),
    ));
    body.push_str(&sse_event(
        "message_delta",
        &json!({"type": "message_delta", "delta": {
            "stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 3}}),
    ));
    body.push_str(&sse_event("message_stop", &json!({"type": "message_stop"})));
    body
}

fn sse_event(event: &str, data: &Value) -> String {
    let data = serde_json::to_string(data).expect("fixture json serializes");
    format!("event: {event}\ndata: {data}\n\n")
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

        let (sse_head, sse_body) = post_raw(
            addr,
            "/v1/chat/completions",
            r#"{"model":"steve-test-model","stream":true}"#,
        )
        .await;
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

    #[tokio::test]
    async fn anthropic_messages_json_and_minimal_sse_on_bound_addr() {
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

        let omitted = post_messages(
            addr,
            r#"{"model":"claude-fixture","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await;
        assert_json_message(&omitted, "claude-fixture");

        let disabled = post_messages(addr, r#"{"stream":false,"messages":[]}"#).await;
        assert_json_message(&disabled, DEFAULT_MODEL);

        let not_bool = post_messages(addr, r#"{"stream":"true"}"#).await;
        assert_json_message(&not_bool, DEFAULT_MODEL);

        let (sse_head, sse_body) = post_raw(
            addr,
            "/v1/messages",
            r#"{"model":"claude-fixture","stream":true,"messages":[{"role":"user","content":"ping"}]}"#,
        )
        .await;
        assert!(sse_head.starts_with("HTTP/1.1 200"), "{sse_head}");
        assert!(
            sse_head
                .to_ascii_lowercase()
                .contains("content-type: text/event-stream"),
            "sse content-type: {sse_head}"
        );
        assert_eq!(sse_body, anthropic_message_sse("claude-fixture"));
        let events: Vec<&str> = sse_body
            .lines()
            .filter_map(|line| line.strip_prefix("event: "))
            .collect();
        assert_eq!(
            events,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert!(sse_body.contains(MESSAGE_TEXT));
        assert!(sse_body.contains("claude-fixture"));

        server.abort();
    }

    #[tokio::test]
    async fn responses_json_and_sse_on_bound_addr() {
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

        let (json_head, json_body) = post_raw(
            addr,
            "/v1/responses",
            r#"{"model":"responses-fixture","input":"hi"}"#,
        )
        .await;
        assert!(json_head.starts_with("HTTP/1.1 200"), "{json_head}");
        assert!(
            json_head
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "json content-type: {json_head}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&json_body).expect("response json"),
            response_json("responses-fixture")
        );

        let (sse_head, sse_body) = post_raw(
            addr,
            "/v1/responses",
            r#"{"model":"responses-fixture","stream":true,"input":"hi"}"#,
        )
        .await;
        assert!(sse_head.starts_with("HTTP/1.1 200"), "{sse_head}");
        assert!(
            sse_head
                .to_ascii_lowercase()
                .contains("content-type: text/event-stream"),
            "sse content-type: {sse_head}"
        );
        let event_types: Vec<&str> = sse_body
            .lines()
            .filter_map(|line| line.strip_prefix("event: "))
            .collect();
        assert_eq!(
            event_types,
            [
                "response.created",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed"
            ]
        );
        let deltas: Vec<String> = sse_body
            .split("\n\n")
            .filter_map(|block| {
                let event = block
                    .lines()
                    .find_map(|line| line.strip_prefix("event: "))?;
                if event != "response.output_text.delta" {
                    return None;
                }
                let data = block.lines().find_map(|line| line.strip_prefix("data: "))?;
                Some(
                    serde_json::from_str::<Value>(data).expect("delta json")["delta"]
                        .as_str()
                        .expect("delta text")
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            deltas,
            RESPONSE_TEXT_CHUNKS
                .iter()
                .map(|chunk| (*chunk).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(deltas.concat(), RESPONSE_TEXT);
        assert!(sse_body.contains(&response_json("responses-fixture").to_string()));

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

    fn assert_json_message(response: &(String, Value), model: &str) {
        let (head, body) = response;
        assert!(
            head.to_ascii_lowercase()
                .contains("content-type: application/json"),
            "json content-type: {head}"
        );
        assert!(!head.to_ascii_lowercase().contains("text/event-stream"));
        assert_eq!(body, &anthropic_message_json(model));
        assert_eq!(body["id"], MESSAGE_ID);
        assert_eq!(body["type"], "message");
        assert_eq!(body["role"], "assistant");
        assert_eq!(body["content"][0]["text"], MESSAGE_TEXT);
        assert_eq!(body["stop_reason"], "end_turn");
        assert_eq!(body["usage"]["input_tokens"], 10);
        assert_eq!(body["usage"]["output_tokens"], 3);
    }

    fn sse_data(body: &str) -> Vec<&str> {
        body.lines()
            .map(str::trim)
            .filter(|line| line.starts_with("data:"))
            .collect()
    }

    async fn post_chat(addr: SocketAddr, json_body: &str) -> (String, Value) {
        post_json(addr, "/v1/chat/completions", json_body).await
    }

    async fn post_messages(addr: SocketAddr, json_body: &str) -> (String, Value) {
        post_json(addr, "/v1/messages", json_body).await
    }

    async fn post_json(addr: SocketAddr, path: &str, json_body: &str) -> (String, Value) {
        let (head, body) = post_raw(addr, path, json_body).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let parsed = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("json body: {error}: {body}");
        });
        (head, parsed)
    }

    async fn post_raw(addr: SocketAddr, path: &str, json_body: &str) -> (String, String) {
        let mut stream = TcpStream::connect(addr).await.expect("connect loopback");
        let len = json_body.len();
        let request = format!(
            "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{json_body}"
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
