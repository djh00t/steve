#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;
#[path = "support/upstream.rs"]
mod upstream;

use process::SteveProcess;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_stream::StreamExt;
use upstream::ControlledUpstream;

#[tokio::test]
async fn messages_disconnect_cancels_upstream() {
    let first = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_steve_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"steve-test-model\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n";
    let tail = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n";
    let mut upstream = ControlledUpstream::start(
        "/v1/messages",
        first.as_slice(),
        upstream::Tail::Bytes(tail.as_slice().into()),
    )
    .await
    .expect("start controlled upstream");
    let upstream_url = upstream.url();
    let mut steve = SteveProcess::start_with_upstream_urls(None, Some(&upstream_url))
        .expect("start Steve process");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build HTTP client");
    let request = json!({
        "model": "steve-test-model",
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true
    });
    let response = client
        .post(format!("http://{}/v1/messages", listeners.inference))
        .json(&request)
        .send()
        .await
        .expect("request Messages stream");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let mut body = response.bytes_stream();

    let captured: Value = upstream
        .wait_for_request(Duration::from_secs(5))
        .await
        .expect("upstream request timeout");
    assert_eq!(captured, request);
    let mut received = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !received.contains("event: message_start\n") || !received.contains("\n\n") {
            let chunk = body
                .next()
                .await
                .expect("stream ended before message_start")
                .expect("read first SSE bytes");
            received.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await
    .unwrap_or_else(|err| {
        panic!("message_start was not forwarded before timeout: {err}; received: {received:?}")
    });
    assert_eq!(received.as_bytes(), first);
    assert!(!upstream.tail_was_sent());
    assert_eq!(upstream.request_count(), 1);

    drop(body);
    let body_drop = upstream.wait_for_body_drop(Duration::from_secs(5)).await;
    upstream.release_tail();
    body_drop.expect("upstream body was not dropped after client disconnect");
    assert_eq!(upstream.request_count(), 1);
}
