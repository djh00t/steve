#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;
#[path = "support/upstream.rs"]
mod upstream;

use process::SteveProcess;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_stream::StreamExt;
use upstream::{ControlledUpstream, Tail};

#[tokio::test]
async fn responses_disconnect_cancels_upstream() {
    let first = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"steve-test-model\"}}\n\n";
    let tail = b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
    let mut upstream = ControlledUpstream::start(
        "/v1/responses",
        first.as_slice(),
        Tail::Bytes(tail.as_slice().into()),
    )
    .await
    .expect("start controlled upstream");
    let upstream_url = upstream.url();
    let mut steve = SteveProcess::start_with_upstream_urls(Some(&upstream_url), None)
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
    let request = json!({"model":"steve-test-model","input":"hi","stream":true});
    let response = client
        .post(format!("http://{}/v1/responses", listeners.inference))
        .json(&request)
        .send()
        .await
        .expect("request Responses stream");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let mut response = response.bytes_stream();

    let captured: Value = upstream
        .wait_for_request(Duration::from_secs(5))
        .await
        .expect("upstream request timeout");
    assert_eq!(captured, request);

    let mut received = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !received.contains("event: response.created\n") || !received.contains("\n\n") {
            let chunk = response
                .next()
                .await
                .expect("stream ended before response.created")
                .expect("read first SSE bytes");
            received.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await
    .unwrap_or_else(|err| {
        panic!("response.created was not forwarded: {err}; received: {received:?}")
    });
    assert_eq!(received.as_bytes(), first);
    assert!(!upstream.tail_was_sent());

    drop(response);
    upstream
        .wait_for_body_drop(Duration::from_secs(5))
        .await
        .expect("upstream response body was not dropped after the client disconnected");
    assert_eq!(upstream.request_count(), 1);
}
