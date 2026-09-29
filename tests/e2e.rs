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
use tempfile::tempdir;
use tokio::time::Instant;

#[derive(Clone, Copy)]
enum ExtraColumn {
    None,
    Stored(&'static str),
    Generated,
}

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

#[tokio::test]
async fn sqlite_rejects_migration_ledger_drift() {
    for (case, rows, should_ready, expected_error) in [
        ("valid v1", vec![(1_i64, "m0_foundation")], true, ""),
        (
            "name drift",
            vec![(1_i64, "m0_changed")],
            false,
            "SQLite migration ledger is not the known v1 prefix",
        ),
        (
            "gap and future version",
            vec![(1_i64, "m0_foundation"), (3_i64, "v0003_future")],
            false,
            "SQLite migration ledger is not the known v1 prefix",
        ),
    ] {
        run_sqlite_migration_case(
            case,
            "version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL",
            &rows,
            should_ready,
            expected_error,
            ExtraColumn::None,
        )
        .await;
    }
}

#[tokio::test]
async fn sqlite_rejects_migration_ledger_column_drift() {
    for (case, ledger_schema, rows, should_ready, expected_error, extra) in [
        (
            "valid v1",
            "version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL",
            vec![(1_i64, "m0_foundation")],
            true,
            "",
            ExtraColumn::None,
        ),
        (
            "nullable name",
            "version INTEGER PRIMARY KEY, name TEXT, applied_at TEXT NOT NULL",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::None,
        ),
        (
            "extra column",
            "version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL, extra TEXT",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::Stored("ordinary-extra-sentinel"),
        ),
        (
            "generated extra column",
            "version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL, extra TEXT GENERATED ALWAYS AS (name || '-generated') VIRTUAL",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::Generated,
        ),
        (
            "wrong type",
            "version TEXT PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::None,
        ),
        (
            "default",
            "version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT 'now'",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::None,
        ),
        (
            "wrong primary key",
            "version INTEGER, name TEXT NOT NULL PRIMARY KEY, applied_at TEXT NOT NULL",
            vec![(1_i64, "m0_foundation")],
            false,
            "column shape",
            ExtraColumn::None,
        ),
        (
            "descending ordinary primary key",
            "version INTEGER PRIMARY KEY DESC, name TEXT NOT NULL, applied_at TEXT NOT NULL",
            vec![],
            false,
            "column shape",
            ExtraColumn::None,
        ),
    ] {
        run_sqlite_migration_case(case, ledger_schema, &rows, should_ready, expected_error, extra)
            .await;
    }
}

#[tokio::test]
async fn sqlite_rejects_foundation_column_drift() {
    for (case, table_schema, should_ready, extra) in [
        (
            "valid v1",
            "id TEXT PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL",
            true,
            ExtraColumn::None,
        ),
        (
            "nullable kind",
            "id TEXT PRIMARY KEY, kind TEXT, payload TEXT NOT NULL, created_at TEXT NOT NULL",
            false,
            ExtraColumn::None,
        ),
        (
            "generated extra",
            "id TEXT PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL, extra TEXT GENERATED ALWAYS AS (kind || '-generated') VIRTUAL",
            false,
            ExtraColumn::Generated,
        ),
        (
            "ordinary extra",
            "id TEXT PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL, extra TEXT",
            false,
            ExtraColumn::Stored("extra-sentinel"),
        ),
        (
            "wrong type",
            "id BLOB PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL",
            false,
            ExtraColumn::None,
        ),
        (
            "wrong primary key",
            "id TEXT, kind TEXT NOT NULL PRIMARY KEY, payload TEXT NOT NULL, created_at TEXT NOT NULL",
            false,
            ExtraColumn::None,
        ),
        (
            "default",
            "id TEXT PRIMARY KEY, kind TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT 'now'",
            false,
            ExtraColumn::None,
        ),
    ] {
        let temp = tempdir().expect("create SQLite temp directory");
        let database_path = temp.path().join("seeded.db");
        let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect to seeded SQLite database");
        sqlx::query(
            "CREATE TABLE steve_schema_migrations (
                version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create migration ledger");
        sqlx::query(&format!("CREATE TABLE steve_background_events ({table_schema})"))
            .execute(&pool)
            .await
            .expect("create application table");
        sqlx::query(
            "INSERT INTO steve_schema_migrations(version, name, applied_at)
             VALUES (1, 'm0_foundation', '2026-09-28T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("seed migration ledger");
        sqlx::query(
            "INSERT INTO steve_background_events(id, kind, payload, created_at)
             VALUES ('preserve-me', 'fixture', 'payload', '2026-09-28T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("seed application row");
        if let ExtraColumn::Stored(value) = extra {
            sqlx::query("UPDATE steve_background_events SET extra = ? WHERE id = 'preserve-me'")
                .bind(value)
                .execute(&pool)
                .await
                .expect("seed extra column");
        }
        pool.close().await;

        let mut steve =
            SteveProcess::start_with_database_url(&database_url).expect("start Steve process");
        let ready = steve.wait_ready(Duration::from_secs(15)).await;
        if should_ready {
            assert!(
                ready.is_ok(),
                "{case}: valid v1 must reach readiness: {}",
                ready.err().unwrap_or_default()
            );
            drop(steve);
        } else {
            let error = match ready {
                Ok(_) => panic!("{case}: drifted application table must block readiness"),
                Err(error) => error,
            };
            assert!(error.contains("Steve exited"), "{case}: {error}");
            assert!(error.contains("application column shape"), "{case}: {error}");
        }

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("reconnect to seeded SQLite database");
        let ledger: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT version, name, applied_at FROM steve_schema_migrations ORDER BY version",
        )
        .fetch_all(&pool)
        .await
        .expect("query migration ledger");
        assert_eq!(
            ledger,
            vec![(
                1,
                "m0_foundation".to_owned(),
                "2026-09-28T00:00:00Z".to_owned(),
            )],
            "{case}"
        );
        let events: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT id, kind, payload, created_at FROM steve_background_events ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .expect("query application rows");
        assert_eq!(
            events,
            vec![(
                "preserve-me".to_owned(),
                "fixture".to_owned(),
                "payload".to_owned(),
                "2026-09-28T00:00:00Z".to_owned(),
            )],
            "{case}"
        );
        if !matches!(extra, ExtraColumn::None) {
            let value: String = sqlx::query_scalar(
                "SELECT extra FROM steve_background_events WHERE id = 'preserve-me'",
            )
            .fetch_one(&pool)
            .await
            .expect("query extra application column");
            let expected = match extra {
                ExtraColumn::Stored(value) => value,
                ExtraColumn::Generated => "fixture-generated",
                ExtraColumn::None => unreachable!(),
            };
            assert_eq!(value, expected, "{case}");
        }
        pool.close().await;
    }
}

async fn run_sqlite_migration_case(
    case: &str,
    ledger_schema: &str,
    rows: &[(i64, &str)],
    should_ready: bool,
    expected_error: &str,
    extra: ExtraColumn,
) {
    let temp = tempdir().expect("create SQLite temp directory");
    let database_path = temp.path().join("seeded.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect to seeded SQLite database");
    sqlx::query(&format!(
        "CREATE TABLE steve_schema_migrations ({ledger_schema})"
    ))
    .execute(&pool)
    .await
    .expect("create migration ledger");
    sqlx::query(
        "CREATE TABLE steve_background_events (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
    )
    .execute(&pool)
    .await
    .expect("create application table");
    for &(version, name) in rows {
        match extra {
            ExtraColumn::Stored(value) => {
                sqlx::query(
                    "INSERT INTO steve_schema_migrations(version, name, applied_at, extra)
                         VALUES (?, ?, ?, ?)",
                )
                .bind(version)
                .bind(name)
                .bind("2026-09-28T00:00:00Z")
                .bind(value)
                .execute(&pool)
                .await
                .expect("seed migration row");
            }
            ExtraColumn::None | ExtraColumn::Generated => {
                sqlx::query(
                    "INSERT INTO steve_schema_migrations(version, name, applied_at)
                         VALUES (?, ?, ?)",
                )
                .bind(version)
                .bind(name)
                .bind("2026-09-28T00:00:00Z")
                .execute(&pool)
                .await
                .expect("seed migration row");
            }
        }
    }
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at)
             VALUES (?, ?, ?, ?)",
    )
    .bind("preserve-me")
    .bind("fixture")
    .bind("payload")
    .bind("2026-09-28T00:00:00Z")
    .execute(&pool)
    .await
    .expect("seed application data");
    pool.close().await;

    let mut steve =
        SteveProcess::start_with_database_url(&database_url).expect("start Steve process");
    let ready = steve.wait_ready(Duration::from_secs(15)).await;
    if should_ready {
        assert!(ready.is_ok(), "valid v1 must reach readiness");
        drop(steve);
    } else {
        let error = match ready {
            Ok(_) => panic!("drifted migration ledger must block readiness"),
            Err(error) => error,
        };
        assert!(error.contains("Steve exited"), "{case}: {error}");
        assert!(error.contains(expected_error), "{case}: {error}");
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("reconnect to seeded SQLite database");
    let actual_rows = match extra {
        ExtraColumn::None => {
            let rows: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT CAST(version AS TEXT), name, applied_at
                     FROM steve_schema_migrations ORDER BY version",
            )
            .fetch_all(&pool)
            .await
            .expect("query migration ledger");
            rows.into_iter()
                .map(|(version, name, applied_at)| (version, name, applied_at, None))
                .collect::<Vec<_>>()
        }
        ExtraColumn::Stored(_) | ExtraColumn::Generated => {
            let rows: Vec<(String, String, String, String)> = sqlx::query_as(
                "SELECT CAST(version AS TEXT), name, applied_at, extra
                     FROM steve_schema_migrations ORDER BY version",
            )
            .fetch_all(&pool)
            .await
            .expect("query migration ledger");
            rows.into_iter()
                .map(|(version, name, applied_at, extra)| (version, name, applied_at, Some(extra)))
                .collect::<Vec<_>>()
        }
    };
    let actual_events: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, kind, payload, created_at FROM steve_background_events
             ORDER BY id, kind, payload, created_at",
    )
    .fetch_all(&pool)
    .await
    .expect("query application data");
    let expected_rows: Vec<_> = rows
        .iter()
        .map(|(version, name)| {
            (
                version.to_string(),
                (*name).to_owned(),
                "2026-09-28T00:00:00Z".to_owned(),
                match extra {
                    ExtraColumn::None => None,
                    ExtraColumn::Stored(value) => Some(value.to_owned()),
                    ExtraColumn::Generated => Some(format!("{name}-generated")),
                },
            )
        })
        .collect();
    assert_eq!(actual_rows, expected_rows);
    assert_eq!(
        actual_events,
        vec![(
            "preserve-me".to_owned(),
            "fixture".to_owned(),
            "payload".to_owned(),
            "2026-09-28T00:00:00Z".to_owned(),
        )]
    );
    pool.close().await;
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

#[cfg(unix)]
#[tokio::test]
async fn inference_saturation_keeps_management_live() {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(60), async {
        let first = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"steve-test-model\"}}\n\n";
        let tail = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"completed\"}}\n\n";
        let mut upstream = ControlledUpstream::start(
            "/v1/responses",
            first.as_slice(),
            Tail::Bytes(tail.as_slice().into()),
        )
        .await
        .expect("start controlled upstream");
        let upstream_url = upstream.url();
        let mut steve = SteveProcess::start_with_upstream_urls(Some(&upstream_url), None)
            .expect("start Steve process with controlled upstream");
        let listeners = steve
            .wait_ready(Duration::from_secs(15))
            .await
            .expect("Steve listeners become ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("build HTTP client");
        let inference = format!("http://{}/v1/responses", listeners.inference);
        let mut requests = tokio::task::JoinSet::new();
        for index in 0..32 {
            let client = client.clone();
            let inference = inference.clone();
            requests.spawn(async move {
                let response = client
                    .post(inference)
                    // Held bodies must outlive every bounded management probe.
                    .timeout(Duration::from_secs(65))
                    .json(&json!({"model":"steve-test-model","input":format!("held {index}"),"stream":true}))
                    .send()
                    .await
                    .expect("send held inference request");
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                let mut response = response.bytes_stream();
                let mut received = Vec::with_capacity(first.len());
                while !received.ends_with(b"\n\n") {
                    let chunk = response
                        .next()
                        .await
                        .expect("stream ended before first SSE event")
                        .expect("read first SSE event");
                    received.extend_from_slice(&chunk);
                }
                assert_eq!(received, first);
                (received, response)
            });
        }

        let mut responses = Vec::with_capacity(32);
        while let Some(result) = requests.join_next().await {
            responses.push(result.expect("held inference request task failed"));
        }
        let captured = upstream
            .wait_for_requests(32, Duration::from_secs(5))
            .await
            .expect("upstream did not receive 32 inference requests");
        assert_eq!(captured.len(), 32);
        assert_eq!(upstream.request_count(), 32);
        assert_eq!(upstream.tail_count(), 0);

        let rejected = client
            .post(&inference)
            .json(&json!({"model":"steve-test-model","input":"overflow","stream":true}))
            .send()
            .await
            .expect("send inference request beyond capacity");
        assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(rejected.headers()[reqwest::header::CONTENT_TYPE], "application/json");
        assert_eq!(rejected.headers()[reqwest::header::CACHE_CONTROL], "no-store");
        assert_eq!(rejected.headers()[reqwest::header::RETRY_AFTER], "1");
        assert_eq!(
            rejected.bytes().await.expect("read overload response").as_ref(),
            br#"{"error":{"type":"overloaded","code":"admission_limit","message":"inference capacity exhausted"}}"#
        );
        assert_eq!(upstream.request_count(), 32, "rejected request reached upstream");

        let management = format!("http://{}", listeners.management);
        let live: Value = client
            .get(format!("{management}/health/live"))
            .send()
            .await
            .expect("request management liveness while saturated")
            .error_for_status()
            .expect("management liveness status")
            .json()
            .await
            .expect("parse management liveness");
        assert_eq!(live["status"], "ok");
        assert_eq!(live["admission"]["inference"]["active"], 32);
        assert_eq!(live["admission"]["inference"]["rejected_total"], 1);

        let ready: Value = client
            .get(format!("{management}/health/ready"))
            .send()
            .await
            .expect("request management readiness while saturated")
            .error_for_status()
            .expect("management readiness status")
            .json()
            .await
            .expect("parse management readiness");
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["admission"]["inference"]["active"], 32);
        assert_eq!(ready["admission"]["inference"]["rejected_total"], 1);

        let status: Value = client
            .get(format!("{management}/api/v1/system/status"))
            .send()
            .await
            .expect("request management status while saturated")
            .error_for_status()
            .expect("management status response")
            .json()
            .await
            .expect("parse management status");
        assert_eq!(status["admission"]["inference"]["limit"], 32);
        assert_eq!(status["admission"]["inference"]["active"], 32);
        assert_eq!(status["admission"]["inference"]["rejected_total"], 1);
        assert_eq!(status["admission"]["management"]["active"], 1);
        assert_eq!(status["active_inference_requests"], 32);
        assert_eq!(status["active_management_requests"], 1);

        let version: Value = client
            .get(format!("{management}/api/v1/system/version"))
            .send()
            .await
            .expect("request management version while saturated")
            .error_for_status()
            .expect("management version response")
            .json()
            .await
            .expect("parse management version");
        assert_eq!(version["name"], "steve");

        upstream.release_tail();
        for (received, response) in &mut responses {
            let mut complete = received.clone();
            while let Some(chunk) = response.next().await {
                complete.extend_from_slice(&chunk.expect("read released inference stream"));
            }
            assert_eq!(complete, [first.as_slice(), tail.as_slice()].concat());
        }
        assert_eq!(upstream.tail_count(), 32);

        let recovered = client
            .post(&inference)
            .json(&json!({"model":"steve-test-model","input":"recovered","stream":true}))
            .send()
            .await
            .expect("send inference request after releasing permits");
        assert_eq!(recovered.status(), reqwest::StatusCode::OK);
        let mut recovered = recovered.bytes_stream();
        let mut complete = Vec::new();
        while let Some(chunk) = recovered.next().await {
            complete.extend_from_slice(&chunk.expect("read recovered inference stream"));
        }
        assert_eq!(complete, [first.as_slice(), tail.as_slice()].concat());
        assert_eq!(upstream.request_count(), 33);
        assert_eq!(upstream.tail_count(), 33);
    })
    .await
    .expect("inference saturation scenario exceeded 60 seconds");
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
