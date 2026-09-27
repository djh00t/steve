#[path = "support/process.rs"]
mod process;
#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/upstream.rs"]
mod upstream;

use process::SteveProcess;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
async fn process_management_and_deferred_event() {
    let mut steve = SteveProcess::start().expect("start Steve process");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    assert!(listeners.inference.ip().is_loopback());
    assert!(listeners.management.ip().is_loopback());
    assert_ne!(listeners.inference.port(), 0);
    assert_ne!(listeners.management.port(), 0);
    assert_ne!(listeners.inference, listeners.management);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build HTTP client");
    let management = format!("http://{}", listeners.management);
    let inference = format!("http://{}", listeners.inference);

    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = client
                .get(format!("{management}/health/ready"))
                .send()
                .await
            {
                if response.status().is_success() {
                    let health: Value = response.json().await.expect("parse readiness response");
                    if health["status"] == "ready" {
                        break health;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("management listener did not become ready");
    assert_eq!(ready["status"], "ready");

    let version: Value = client
        .get(format!("{management}/api/v1/system/version"))
        .send()
        .await
        .expect("request version")
        .error_for_status()
        .expect("version status")
        .json()
        .await
        .expect("parse version");
    assert_eq!(version["name"], "steve");
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));

    let run = uuid::Uuid::now_v7().to_string();
    let request = json!({"value": {"run": run}});
    let reply: Value = client
        .post(format!("{inference}/api/v1/test/echo"))
        .json(&request)
        .send()
        .await
        .expect("request echo")
        .error_for_status()
        .expect("echo status")
        .json()
        .await
        .expect("parse echo response");
    assert_eq!(reply, request);

    let barrier_run = uuid::Uuid::now_v7().to_string();
    let barrier_request = json!({"value": {"run": barrier_run}});
    let barrier_reply: Value = client
        .post(format!("{inference}/api/v1/test/echo"))
        .json(&barrier_request)
        .send()
        .await
        .expect("request echo barrier")
        .error_for_status()
        .expect("echo barrier status")
        .json()
        .await
        .expect("parse echo barrier response");
    assert_eq!(barrier_reply, barrier_request);

    let db_url = format!("sqlite://{}", steve.database_path().display());
    let pool = tokio::time::timeout(
        Duration::from_secs(2),
        SqlitePoolOptions::new().max_connections(1).connect(&db_url),
    )
    .await
    .expect("timed out connecting to Steve SQLite database")
    .expect("connect to Steve SQLite database");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let payloads = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar::<_, String>(
                "SELECT payload FROM steve_background_events WHERE kind = ?",
            )
            .bind("test.echo")
            .fetch_all(&pool),
        )
        .await
        .expect("timed out querying deferred events")
        .expect("query deferred events");
        let events: Vec<Value> = payloads
            .into_iter()
            .filter_map(|payload| serde_json::from_str(&payload).ok())
            .collect();
        let matching: Vec<_> = events
            .iter()
            .filter(|payload| {
                payload.pointer("/value/run").and_then(Value::as_str) == Some(run.as_str())
            })
            .collect();
        let barrier: Vec<_> = events
            .iter()
            .filter(|payload| {
                payload.pointer("/value/run").and_then(Value::as_str) == Some(barrier_run.as_str())
            })
            .collect();
        assert!(
            barrier.len() <= 1,
            "duplicate echo barrier for run {barrier_run}"
        );
        if !barrier.is_empty() {
            assert_eq!(
                matching.len(),
                1,
                "expected exactly one deferred event for run {run}"
            );
            assert_eq!(matching[0], &request);
            assert_eq!(barrier[0], &barrier_request);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "missing persisted test.echo event for run {run}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    pool.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn drain_active_stream() {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(30), async {
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
        let mut steve = SteveProcess::start_with_drain_timeout(Some(&upstream_url), None, 5)
            .expect("start Steve process with a five-second drain timeout");
        let listeners = steve
            .wait_ready(Duration::from_secs(15))
            .await
            .expect("Steve listeners become ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
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
        let mut response = response.bytes_stream();

        let captured: Value = upstream
            .wait_for_request(Duration::from_secs(5))
            .await
            .expect("upstream request timeout");
        assert_eq!(captured, request);

        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !received.ends_with(b"\n\n") {
                let chunk = response
                    .next()
                    .await
                    .expect("stream ended before the first SSE event")
                    .expect("read first SSE event");
                received.extend_from_slice(&chunk);
            }
        })
        .await
        .expect("first SSE event was not forwarded");
        assert_eq!(received, first);
        assert!(!upstream.tail_was_sent());

        let drain: Value = client
            .post(format!("http://{}/api/v1/system/drain", listeners.management))
            .send()
            .await
            .expect("request management drain")
            .error_for_status()
            .expect("management drain status")
            .json()
            .await
            .expect("parse management drain response");
        assert_eq!(drain["phase"], "draining");

        let readiness = client
            .get(format!("http://{}/health/ready", listeners.management))
            .send()
            .await
            .expect("request readiness during drain");
        assert_eq!(readiness.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let readiness: Value = readiness.json().await.expect("parse readiness response");
        assert_eq!(readiness["status"], "not_ready");
        assert_eq!(readiness["phase"], "draining");
        assert_eq!(readiness["inflight"], 1, "held stream keeps its lifecycle guard");

        let live = client
            .get(format!("http://{}/health/live", listeners.management))
            .send()
            .await
            .expect("request liveness during drain");
        assert_eq!(live.status(), reqwest::StatusCode::OK);

        let shutdown_started = Instant::now();
        steve.send_sigterm().expect("send SIGTERM to Steve");
        upstream.release_tail();
        while let Some(chunk) = response.next().await {
            received.extend_from_slice(&chunk.expect("read remaining Responses stream"));
        }
        let mut expected = first.to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(received, expected);
        assert!(upstream.tail_was_sent());

        let status = steve
            .wait_for_exit(Duration::from_secs(8))
            .await
            .expect("Steve exits within the caller's timeout");
        assert!(status.success(), "Steve exited unsuccessfully: {status}");
        assert!(
            shutdown_started.elapsed() < Duration::from_secs(5),
            "Steve did not exit before its configured drain deadline"
        );
    })
    .await
    .expect("active stream drain scenario exceeded 30 seconds");
}

#[cfg(unix)]
#[tokio::test]
async fn chat_completions_stream_forwards_first_event_before_tail() {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(30), async {
        let first = b"data: {\"id\":\"chatcmpl_fixture\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}] }\n\n";
        let tail = b"data: {\"id\":\"chatcmpl_fixture\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"index\":0}]}\n\ndata: [DONE]\n\n";
        let mut upstream = ControlledUpstream::start(
            "/v1/chat/completions",
            first.as_slice(),
            Tail::Bytes(tail.as_slice().into()),
        )
        .await
        .expect("start controlled upstream");
        let upstream_url = upstream.url();
        let mut steve = SteveProcess::start_with_upstream_urls(Some(&upstream_url), None)
            .expect("start Steve process with OpenAI upstream");
        let listeners = steve
            .wait_ready(Duration::from_secs(15))
            .await
            .expect("Steve listeners become ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .expect("build HTTP client");
        let request = json!({
            "model": "steve-test-model",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "temperature": 0.25,
            "metadata": {"trace": "preserve-me"}
        });
        let response = client
            .post(format!("http://{}/v1/chat/completions", listeners.inference))
            .json(&request)
            .send()
            .await
            .expect("request Chat Completions stream");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
        let mut response = response.bytes_stream();

        let captured: Value = upstream
            .wait_for_request(Duration::from_secs(5))
            .await
            .expect("upstream request timeout");
        assert_eq!(captured, request, "forward the complete request unchanged");

        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !received.ends_with(b"\n\n") {
                let chunk = response
                    .next()
                    .await
                    .expect("stream ended before the first SSE event")
                    .expect("read first SSE event");
                received.extend_from_slice(&chunk);
            }
        })
        .await
        .expect("first SSE event was not forwarded");
        assert_eq!(received, first);
        assert!(!upstream.tail_was_sent());

        let management = format!("http://{}", listeners.management);
        let status: Value = client
            .get(format!("{management}/api/v1/system/status"))
            .send()
            .await
            .expect("request system status while stream is held")
            .error_for_status()
            .expect("system status response")
            .json()
            .await
            .expect("parse system status");
        assert_eq!(status["inflight"], 1, "held stream keeps its lifecycle guard");
        assert_eq!(
            status["active_inference_requests"], 1,
            "held stream keeps its request counter"
        );

        upstream.release_tail();
        while let Some(chunk) = response.next().await {
            received.extend_from_slice(&chunk.expect("read remaining Chat Completions stream"));
        }
        let mut expected = first.to_vec();
        expected.extend_from_slice(tail);
        assert_eq!(received, expected);
        assert!(upstream.tail_was_sent());

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let status: Value = client
                    .get(format!("{management}/api/v1/system/status"))
                    .send()
                    .await
                    .expect("request system status after stream EOF")
                    .error_for_status()
                    .expect("system status response")
                    .json()
                    .await
                    .expect("parse system status");
                if status["inflight"] == 0 && status["active_inference_requests"] == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("stream completion did not release request guards");
    })
    .await
    .expect("Chat Completions stream scenario exceeded 30 seconds");
}

#[cfg(unix)]
#[tokio::test]
async fn chat_disconnect_no_replay() {
    use upstream::Tail;

    for (tail, disconnect) in [
        (Tail::Bytes(b"unreleased tail".as_slice().into()), true),
        (Tail::Error("failure after first output".into()), false),
    ] {
        tokio::time::timeout(
            Duration::from_secs(45),
            run_chat_disconnect_case(tail, disconnect),
        )
        .await
        .expect("Chat Completions disconnect case exceeded 45 seconds");
    }
}

#[cfg(unix)]
async fn run_chat_disconnect_case(tail: upstream::Tail, disconnect: bool) {
    use tokio_stream::StreamExt;
    use upstream::ControlledUpstream;

    let first = b"data: {\"id\":\"chatcmpl_fixture\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n";
    let mut upstream = ControlledUpstream::start("/v1/chat/completions", first.as_slice(), tail)
        .await
        .expect("start controlled upstream");
    let upstream_url = upstream.url();
    let mut steve = SteveProcess::start_with_upstream_urls(Some(&upstream_url), None)
        .expect("start Steve process with OpenAI upstream");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .expect("Steve listeners become ready");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(8))
        .build()
        .expect("build HTTP client");
    let request = json!({
        "model": "steve-test-model",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true
    });
    let response = client
        .post(format!(
            "http://{}/v1/chat/completions",
            listeners.inference
        ))
        .json(&request)
        .send()
        .await
        .expect("request Chat Completions stream");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut response = response.bytes_stream();

    let captured: Value = upstream
        .wait_for_request(Duration::from_secs(5))
        .await
        .expect("upstream request timeout");
    assert_eq!(captured, request);

    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !received.ends_with(b"\n\n") {
            let chunk = response
                .next()
                .await
                .expect("stream ended before the first SSE event")
                .expect("read first SSE event");
            received.extend_from_slice(&chunk);
        }
    })
    .await
    .expect("first SSE event was not forwarded");
    assert_eq!(received, first, "observe output before disconnect or fault");
    assert!(!upstream.tail_was_sent(), "hold tail until first output");

    if disconnect {
        drop(response);
        upstream
            .wait_for_body_drop(Duration::from_secs(5))
            .await
            .expect("upstream response body was not dropped after client disconnect");
        assert!(
            !upstream.tail_was_sent(),
            "unreleased tail must be cancelled"
        );
    } else {
        upstream.release_tail();
        let error = tokio::time::timeout(Duration::from_secs(5), response.next())
            .await
            .expect("post-output upstream failure was not observed")
            .expect("stream ended without surfacing upstream failure");
        assert!(
            error.is_err(),
            "post-output upstream failure must reach client"
        );
    }

    let management = format!("http://{}", listeners.management);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status: Value = client
                .get(format!("{management}/api/v1/system/status"))
                .send()
                .await
                .expect("request system status after stream end")
                .error_for_status()
                .expect("system status response")
                .json()
                .await
                .expect("parse system status");
            if status["inflight"] == 0 && status["active_inference_requests"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("stream end did not release request guards");

    assert_eq!(upstream.request_count(), 1, "stream must never replay");
    assert!(
        upstream
            .wait_for_requests(1, Duration::from_millis(100))
            .await
            .is_err(),
        "no sentinel second request may reach upstream"
    );
}
