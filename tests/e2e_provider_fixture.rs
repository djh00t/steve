#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;
#[path = "support/upstream.rs"]
mod upstream;

use process::SteveProcess;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_stream::{Stream, StreamExt};
use upstream::ControlledUpstream;

#[tokio::test]
async fn provider_fixture_controls() {
    let first = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"steve-test-model\"}}\n\n";
    let tail = b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n\
event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fixture\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"steve-test-model\",\"output\":[]}}\n\n";
    let mut upstream = ControlledUpstream::start(
        "/v1/responses",
        first.as_slice(),
        upstream::Tail::Bytes(tail.as_slice().into()),
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
    let mut response = client
        .post(format!("http://{}/v1/responses", listeners.inference))
        .json(&request)
        .send()
        .await
        .expect("request Responses stream")
        .error_for_status()
        .expect("Responses status")
        .bytes_stream();

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
    assert!(!upstream.tail_was_sent());
    assert_eq!(upstream.request_count(), 1);

    upstream.release_tail();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = response.next().await {
            let chunk = chunk.expect("read terminal SSE bytes");
            received.push_str(&String::from_utf8_lossy(&chunk));
        }
    })
    .await
    .expect("terminal SSE events were not forwarded");
    assert_eq!(
        received.as_bytes(),
        [first.as_slice(), tail.as_slice()].concat()
    );
    assert!(upstream.tail_was_sent());
    drop(response);
    upstream
        .wait_for_body_drop(Duration::from_secs(5))
        .await
        .expect("upstream body was not dropped");
}

#[tokio::test]
async fn controlled_upstream_holds_32_response_bodies() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let first = b"first chunk";
        let tail = b" and tail";
        let mut upstream = ControlledUpstream::start(
            "/held",
            first.as_slice(),
            upstream::Tail::Bytes(tail.as_slice().into()),
        )
        .await
        .expect("start controlled upstream");
        assert!(upstream
            .wait_for_requests(33, Duration::ZERO)
            .await
            .expect_err("capture limit should be enforced")
            .contains("maximum is 32"));
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build HTTP client");
        let mut requests = tokio::task::JoinSet::new();
        for index in 0..32 {
            let client = client.clone();
            let url = format!("{}/held", upstream.url());
            requests.spawn(async move {
                let mut response = client
                    .post(url)
                    .json(&json!({"index": index}))
                    .send()
                    .await
                    .expect("send fixture request")
                    .error_for_status()
                    .expect("fixture response status")
                    .bytes_stream();
                let mut first_chunk = Vec::with_capacity(first.len());
                while first_chunk.len() < first.len() {
                    let chunk = response
                        .next()
                        .await
                        .expect("response ended before all first bytes")
                        .expect("read first response bytes");
                    first_chunk.extend_from_slice(&chunk);
                }
                assert_eq!(first_chunk, first);
                (index, first_chunk, response)
            });
        }

        let mut responses = Vec::with_capacity(32);
        while let Some(result) = requests.join_next().await {
            responses.push(result.expect("request task failed"));
        }
        let captured = upstream
            .wait_for_requests(32, Duration::from_secs(5))
            .await
            .expect("upstream request timeout");
        let mut captured_indices: Vec<_> = captured
            .into_iter()
            .map(|request| request["index"].as_u64().expect("request index"))
            .collect();
        captured_indices.sort_unstable();
        assert_eq!(captured_indices, (0..32).collect::<Vec<_>>());
        assert_eq!(upstream.request_count(), 32);
        assert!(!upstream.tail_was_sent());
        assert_eq!(upstream.tail_count(), 0);

        for (_, _, response) in &mut responses {
            let next = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(std::pin::Pin::new(&mut *response).poll_next(cx))
            })
            .await;
            assert!(next.is_pending(), "response ended before tail release");
        }

        upstream.release_tail();
        for (_, first_chunk, response) in &mut responses {
            let mut received = first_chunk.to_vec();
            while let Some(chunk) = response.next().await {
                received.extend_from_slice(&chunk.expect("read response tail"));
            }
            assert_eq!(received, [first.as_slice(), tail.as_slice()].concat());
        }
        assert_eq!(upstream.tail_count(), 32);

        for index in 0..33 {
            client
                .post(format!("{}/held", upstream.url()))
                .json(&json!({"overflow": index}))
                .send()
                .await
                .expect("send overflow request")
                .error_for_status()
                .expect("overflow response status")
                .bytes()
                .await
                .expect("drain overflow response");
        }
        assert!(upstream
            .wait_for_requests(1, Duration::from_secs(1))
            .await
            .expect_err("bounded capture overflow should be reported")
            .contains("exceeded its 32-request capacity"));
    })
    .await
    .expect("32-response fixture qualification timed out");
}
