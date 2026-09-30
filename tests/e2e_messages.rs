#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;
#[path = "support/upstream.rs"]
mod upstream;
#[path = "support/upstream_tls.rs"]
mod upstream_tls;

use process::SteveProcess;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_stream::StreamExt;
use upstream::ControlledUpstream;

#[tokio::test]
async fn stv_prov_36_acceptance() {
    use axum::{routing::post, Router};

    tokio::time::timeout(Duration::from_secs(30), async {
        let request = json!({
            "model": "steve-test-model",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "secure hello"}]
        });
        let upstream_response = json!({
            "id": "msg_tls_fixture",
            "type": "message",
            "role": "assistant",
            "model": "steve-test-model",
            "content": [{"type": "text", "text": "secure response"}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 3}
        });
        let (request_tx, mut request_rx) = tokio::sync::mpsc::channel(1);
        let response_for_route = upstream_response.clone();
        let fixture = upstream_tls::TlsFixture::start(Router::new().route(
            "/v1/messages",
            post(
                move |method: axum::http::Method,
                      uri: axum::http::Uri,
                      headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<Value>| {
                    let request_tx = request_tx.clone();
                    let response = response_for_route.clone();
                    async move {
                        request_tx
                            .send((
                                method,
                                uri.path().to_owned(),
                                headers.get("anthropic-version").cloned(),
                                body,
                            ))
                            .await
                            .expect("capture HTTPS request");
                        axum::Json(response)
                    }
                },
            ),
        ))
        .await
        .expect("start HTTPS Anthropic fixture");
        let upstream_url = fixture.url();
        let mut steve = SteveProcess::start_with_upstream_ca_bundle(
            None,
            Some(&upstream_url),
            Some(std::path::Path::new("tests/fixtures/tls/ca.pem")),
        )
        .expect("start Steve with HTTPS Anthropic upstream");
        let listeners = steve
            .wait_ready(Duration::from_secs(15))
            .await
            .expect("Steve listeners become ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .expect("build ingress client");
        let response = client
            .post(format!("http://{}/v1/messages", listeners.inference))
            .json(&request)
            .send()
            .await
            .expect("request Messages through HTTPS upstream");
        let status = response.status();
        let body: Value = response.json().await.expect("HTTPS response JSON");
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        assert_eq!(body, upstream_response);
        let (method, path, version, captured) =
            request_rx.recv().await.expect("captured HTTPS request");
        assert_eq!(method, axum::http::Method::POST);
        assert_eq!(path, "/v1/messages");
        assert_eq!(
            version.as_ref().and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
        assert_eq!(captured, request);
        drop(steve);
        fixture.shutdown().await.expect("HTTPS fixture shutdown");

        let response_bytes = serde_json::to_vec(&upstream_response).expect("encode HTTP response");
        let mut http_fixture = ControlledUpstream::start(
            "/v1/messages",
            response_bytes,
            upstream::Tail::Bytes(bytes::Bytes::new()),
        )
        .await
        .expect("start HTTP Anthropic fixture");
        http_fixture.release_tail();
        let mut steve = SteveProcess::start_with_upstream_urls(None, Some(&http_fixture.url()))
            .expect("start Steve with HTTP Anthropic upstream");
        let listeners = steve
            .wait_ready(Duration::from_secs(15))
            .await
            .expect("Steve listeners become ready for HTTP");
        let response = client
            .post(format!("http://{}/v1/messages", listeners.inference))
            .json(&request)
            .send()
            .await
            .expect("request Messages through HTTP upstream");
        let status = response.status();
        let body: Value = response.json().await.expect("HTTP response JSON");
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        assert_eq!(body, upstream_response);
        assert_eq!(
            http_fixture
                .wait_for_request(Duration::from_secs(5))
                .await
                .expect("captured HTTP request"),
            request
        );
    })
    .await
    .expect("STV-PROV-36 acceptance exceeded 30 seconds");
}

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
