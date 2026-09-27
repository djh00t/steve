#[path = "support/process.rs"]
mod process;
#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/upstream.rs"]
mod upstream;

use process::SteveProcess;
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, sqlite::SqlitePoolOptions};
use std::time::Duration;
use tokio::time::Instant;

#[tokio::test]
async fn database_backend_contract() {
    run_database_backend_contract(None).await;
    if let Some(database_url) = std::env::var_os("STEVE_TEST_POSTGRES_URL") {
        let database_url = database_url
            .into_string()
            .expect("STEVE_TEST_POSTGRES_URL must be valid UTF-8");
        run_database_backend_contract(Some(&database_url)).await;
    }
}

type EventRow = (String, String, String, String);

async fn run_database_backend_contract(postgres_url: Option<&str>) {
    let mut steve = match postgres_url {
        Some(url) => {
            SteveProcess::start_with_database_url(url).expect("start Steve process with PostgreSQL")
        }
        None => SteveProcess::start().expect("start Steve process with SQLite"),
    };
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

    let pool = match postgres_url {
        Some(url) => EventPool::Postgres(
            tokio::time::timeout(
                Duration::from_secs(2),
                PgPoolOptions::new().max_connections(1).connect(url),
            )
            .await
            .expect("timed out connecting to configured Steve PostgreSQL database")
            .expect("connect to configured Steve PostgreSQL database"),
        ),
        None => {
            let db_url = format!("sqlite://{}", steve.database_path().display());
            EventPool::Sqlite(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    SqlitePoolOptions::new().max_connections(1).connect(&db_url),
                )
                .await
                .expect("timed out connecting to Steve SQLite database")
                .expect("connect to Steve SQLite database"),
            )
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = tokio::time::timeout(Duration::from_secs(2), pool.events(&run, &barrier_run))
            .await
            .expect("timed out querying deferred events")
            .expect("query deferred events");
        let matching: Vec<_> = rows
            .iter()
            .filter(|(_, _, payload, _)| {
                serde_json::from_str::<Value>(payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/value/run")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some(run.as_str())
            })
            .collect();
        let barrier: Vec<_> = rows
            .iter()
            .filter(|(_, _, payload, _)| {
                serde_json::from_str::<Value>(payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/value/run")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some(barrier_run.as_str())
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
            let row = matching[0];
            assert_eq!(row.1, "test.echo");
            let persisted_payload: Value =
                serde_json::from_str(&row.2).expect("parse persisted payload");
            assert_eq!(persisted_payload, request);
            uuid::Uuid::parse_str(&row.0).expect("persisted event id is a UUID");
            chrono::DateTime::parse_from_rfc3339(&row.3)
                .expect("persisted event timestamp is RFC 3339");
            let by_id = tokio::time::timeout(Duration::from_secs(2), pool.event_by_id(&row.0))
                .await
                .expect("timed out retrieving persisted event by id")
                .expect("retrieve persisted event by id");
            assert_eq!(
                &by_id, row,
                "event fields remain stable when retrieved by id"
            );
            let barrier_payload: Value =
                serde_json::from_str(&barrier[0].2).expect("parse barrier payload");
            assert_eq!(barrier[0].1, "test.echo");
            assert_eq!(barrier_payload, barrier_request);
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

enum EventPool {
    Sqlite(sqlx::SqlitePool),
    Postgres(sqlx::PgPool),
}

impl EventPool {
    async fn events(&self, run: &str, barrier: &str) -> Result<Vec<EventRow>, sqlx::Error> {
        let run_pattern = format!("%{run}%");
        let barrier_pattern = format!("%{barrier}%");
        match self {
            Self::Sqlite(pool) => {
                sqlx::query_as(
                    "SELECT id, kind, payload, created_at FROM steve_background_events
                 WHERE kind = ? AND (payload LIKE ? OR payload LIKE ?) ORDER BY id LIMIT 3",
                )
                .bind("test.echo")
                .bind(run_pattern)
                .bind(barrier_pattern)
                .fetch_all(pool)
                .await
            }
            Self::Postgres(pool) => {
                sqlx::query_as(
                    "SELECT id, kind, payload, created_at FROM steve_background_events
                 WHERE kind = $1 AND (payload LIKE $2 OR payload LIKE $3) ORDER BY id LIMIT 3",
                )
                .bind("test.echo")
                .bind(run_pattern)
                .bind(barrier_pattern)
                .fetch_all(pool)
                .await
            }
        }
    }

    async fn event_by_id(&self, id: &str) -> Result<EventRow, sqlx::Error> {
        match self {
            Self::Sqlite(pool) => sqlx::query_as(
                "SELECT id, kind, payload, created_at FROM steve_background_events WHERE id = ?",
            )
            .bind(id)
            .fetch_one(pool)
            .await,
            Self::Postgres(pool) => sqlx::query_as(
                "SELECT id, kind, payload, created_at FROM steve_background_events WHERE id = $1",
            )
            .bind(id)
            .fetch_one(pool)
            .await,
        }
    }

    async fn close(self) {
        match self {
            Self::Sqlite(pool) => pool.close().await,
            Self::Postgres(pool) => pool.close().await,
        }
    }
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
