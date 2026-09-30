#![allow(dead_code)]

#[path = "support/object_store.rs"]
mod object_store;
#[path = "support/process.rs"]
mod process;
#[path = "support/upstream.rs"]
mod upstream;

use object_store::HeldObjectStoreEndpoint;
use process::{acquire_accounting_startup_lock, steve_command, SteveProcess};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, sqlite::SqlitePoolOptions};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const REPLAY_EVENT_ONE: &[u8] = b"{\"id\":\"01995200-0000-7000-8000-000000000101\",\"kind\":\"test.replay.v1\",\"payload\":{\"value\":1},\"created_at\":\"2026-09-29T00:00:01Z\"}\n";
const REPLAY_EVENT_ONE_DIFFERENT: &[u8] = b"{\"id\":\"01995200-0000-7000-8000-000000000101\",\"kind\":\"test.replay.v1\",\"payload\":{\"value\":111},\"created_at\":\"2026-09-29T00:00:01Z\"}\n";
const REPLAY_EVENT_TWO: &[u8] = b"{\"id\":\"01995200-0000-7000-8000-000000000102\",\"kind\":\"test.replay.v1\",\"payload\":{\"value\":2},\"created_at\":\"2026-09-29T00:00:02Z\"}\n";
const TORN_TAIL: &[u8] = b"{\"id\":\"01995200-0000-7000-8000-000000000103\"";

fn run_accounting(args: &[&str], paths: &[&Path]) -> std::process::Output {
    let startup_lock = acquire_accounting_startup_lock().expect("serialize accounting command");
    let mut command = steve_command();
    command.arg("accounting");
    for arg in args {
        command.arg(arg);
    }
    for path in paths {
        command.arg(path);
    }
    let output = command.output().expect("run Steve accounting command");
    File::unlock(&startup_lock).expect("release accounting command lock");
    output
}

fn provision(root: &Path) -> std::process::Output {
    run_accounting(&["provision", "--root"], &[root])
}

fn adopt(source: &Path, root: &Path, maintenance_assertion: &Path) -> std::process::Output {
    let startup_lock = acquire_accounting_startup_lock().expect("serialize adoption command");
    let output = steve_command()
        .args(["accounting", "adopt-legacy", "--source"])
        .arg(source)
        .arg("--root")
        .arg(root)
        .arg("--maintenance-assertion")
        .arg(maintenance_assertion)
        .output()
        .expect("run Steve legacy adoption");
    File::unlock(&startup_lock).expect("release adoption command lock");
    output
}

fn generation_journals(root: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(root.join("generations"))
        .expect("read generations")
        .map(|entry| entry.expect("generation entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("journal"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn journal_events(root: &Path) -> Vec<Value> {
    generation_journals(root)
        .into_iter()
        .flat_map(|path| {
            fs::read(path)
                .expect("read generation journal")
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice(line).expect("valid journal event"))
                .collect::<Vec<Value>>()
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_checked_fixture(path: &Path, value: &Value) {
    let bytes = serde_json::to_vec_pretty(value).expect("serialize checked fixture");
    fs::write(path, &bytes).expect("write checked fixture");
    fs::write(format!("{}.sha256", path.display()), sha256(&bytes))
        .expect("write checked fixture checksum");
}

fn accounting_root_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).expect("read accounting evidence directory") {
            let path = entry.expect("accounting evidence entry").path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("accounting evidence below root")
                        .to_owned(),
                    fs::read(&path).expect("read accounting evidence bytes"),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn write_maintenance_assertion(source: &Path, path: &Path) {
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({
            "workload_identity": "legacy-steve-task3",
            "host": "fixture-host",
            "source": fs::canonicalize(source).expect("canonical source"),
            "stopped": true,
            "restart_disabled": true,
            "observed_at": "2026-09-29T00:00:00Z",
            "command_or_exported_status": "fixture supervisor stopped and disabled legacy-steve-task3"
        }))
        .expect("serialize maintenance assertion"),
    )
    .expect("write maintenance assertion");
}

fn adopt_fixture(source: &Path, root: &Path, assertion: &Path, bytes: &[u8]) -> Value {
    fs::write(source, bytes).expect("write legacy journal fixture");
    write_maintenance_assertion(source, assertion);
    let output = adopt(source, root, assertion);
    assert!(
        output.status.success(),
        "legacy adoption failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("adoption evidence"))
        .expect("valid adoption evidence")
}

async fn complete_adoption_fixture(
    source: &Path,
    root: &Path,
    assertion: &Path,
) -> (Value, SteveProcess) {
    adopt_fixture(source, root, assertion, REPLAY_EVENT_ONE);
    let mut process = SteveProcess::start_with_accounting_root(root).expect("start adopted Steve");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("adoption replay completes before readiness");
    let completed =
        serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("adoption evidence"))
            .expect("valid adoption evidence");
    (completed, process)
}

fn generation_id(adoption: &Value) -> &str {
    adoption["generation_id"]
        .as_str()
        .expect("adoption generation id")
}

fn replay_evidence(root: &Path, adoption: &Value) -> Value {
    let path = root
        .join("generations")
        .join(format!("{}.replay.json", generation_id(adoption)));
    serde_json::from_slice(
        &fs::read(&path)
            .unwrap_or_else(|err| panic!("read replay evidence {}: {err}", path.display())),
    )
    .expect("valid replay evidence")
}

fn replay_evidence_text(root: &Path, adoption: &Value) -> String {
    serde_json::to_string(&replay_evidence(root, adoption)).expect("serialize replay evidence")
}

fn retained_artifacts_match(root: &Path, adoption: &Value, expected: &[u8]) {
    let backup = PathBuf::from(adoption["backup"].as_str().expect("backup path"));
    assert_eq!(fs::read(backup).expect("retained backup"), expected);
    let journal = root
        .join("generations")
        .join(format!("{}.journal", generation_id(adoption)));
    assert_eq!(fs::read(journal).expect("retained journal"), expected);
}

async fn prepare_sqlite(database_url: &str) -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect(database_url)
        .await
        .expect("connect task3 SQLite fixture");
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(&pool)
        .await
        .expect("enable SQLite WAL");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS steve_schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create SQLite migration ledger");
    sqlx::query(
        "INSERT OR IGNORE INTO steve_schema_migrations(version, name, applied_at)
         VALUES (1, 'm0_foundation', '2026-09-29T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("seed SQLite migration ledger");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS steve_background_events (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create SQLite event table");
    pool
}

async fn sqlite_events(database_url: &str) -> Vec<(String, String, String, String)> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .expect("open SQLite event evidence");
    let rows = sqlx::query_as(
        "SELECT id, kind, payload, created_at
         FROM steve_background_events ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("read SQLite event evidence");
    pool.close().await;
    rows
}

fn incident(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("incident.json")).expect("incident evidence"))
        .expect("valid incident evidence")
}

fn write_offline_config(path: &Path, database_url: &str, accounting_root: &Path) {
    let object_root = path.parent().expect("config parent").join("objects");
    fs::create_dir_all(&object_root).expect("object root");
    fs::write(
        path,
        format!(
            "[database]\nurl = {}\n[object_storage]\nkind = \"fs\"\nroot = {}\n[queues]\naccounting_journal = {}\n",
            toml::Value::String(database_url.to_string()),
            toml::Value::String(object_root.display().to_string()),
            toml::Value::String(accounting_root.display().to_string()),
        ),
    )
    .expect("offline config");
}

fn configured_accounting(
    config: &Path,
    command: &str,
    arguments: &[(&str, &str)],
) -> std::process::Output {
    let startup_lock = acquire_accounting_startup_lock().expect("serialize accounting command");
    let mut process = steve_command();
    process
        .arg("--config")
        .arg(config)
        .args(["accounting", command]);
    for (name, value) in arguments {
        process.args([name, value]);
    }
    let output = process
        .env("STEVE_OPERATOR", "forged-operator")
        .env("USER", "forged-user")
        .env("USERNAME", "forged-username")
        .env("PATH", "/nonexistent")
        .output()
        .expect("run configured accounting command");
    File::unlock(&startup_lock).expect("release accounting command lock");
    output
}

async fn wait_for_incident(
    client: &reqwest::Client,
    management: std::net::SocketAddr,
    state: &str,
) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = client
                .get(format!("http://{management}/api/v1/system/status"))
                .send()
                .await
            {
                if response.status().is_success() {
                    let body: Value = response.json().await.expect("status JSON");
                    if body["accounting_incident"]["state"] == state {
                        break body;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("incident state was not observable")
}

async fn wait_for_accounting_settle(root: &Path, database_url: &str, admitted: u64) {
    let started = Instant::now();
    let mut stable: Option<((u64, u64, u64, String), Instant)> = None;
    loop {
        let coordination_bytes =
            fs::read(root.join("coordination.json")).expect("read accounting coordination");
        let coordination: Value =
            serde_json::from_slice(&coordination_bytes).expect("valid accounting coordination");
        let mut journal_records = 0_u64;
        let mut evidence_matches = true;
        for journal in generation_journals(root) {
            let bytes = fs::read(&journal).expect("read generation journal");
            journal_records += bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count() as u64;
            let generation_id = journal
                .file_stem()
                .and_then(|name| name.to_str())
                .expect("generation id");
            let state_path = root
                .join("generations")
                .join(format!("{generation_id}.state.json"));
            let state_bytes = fs::read(&state_path).expect("read generation state");
            let state: Value =
                serde_json::from_slice(&state_bytes).expect("valid generation state");
            let covered = coordination["coverage"]
                .as_array()
                .expect("coordination coverage")
                .iter()
                .any(|entry| {
                    entry["generation_id"] == generation_id
                        && entry["generation_state_revision"] == state["revision"]
                        && entry["journal_evidence_digest"] == state["journal_evidence_digest"]
                });
            evidence_matches &= state["journal_length"] == bytes.len() as u64
                && state["journal_evidence_digest"] == sha256(&bytes)
                && fs::read(format!("{}.sha256", state_path.display()))
                    .expect("generation state checksum")
                    == sha256(&state_bytes).as_bytes()
                && covered;
        }
        let lost = incident(root)["payloads"]["outcome_totals"]["unrecoverable_lost"]
            .as_u64()
            .expect("known lost count");
        let database_records = sqlite_events(database_url).await.len() as u64;
        let accounted = database_records + journal_records + lost;
        assert!(
            accounted <= admitted,
            "accounting outcomes exceed admitted requests: admitted={admitted} database={database_records} journal={journal_records} lost={lost}"
        );
        let snapshot = (
            database_records,
            journal_records,
            lost,
            sha256(&coordination_bytes),
        );
        if evidence_matches {
            match &stable {
                Some((prior, since))
                    if prior == &snapshot && since.elapsed() >= Duration::from_millis(500) =>
                {
                    return;
                }
                Some((prior, _)) if prior == &snapshot => {}
                _ => stable = Some((snapshot, Instant::now())),
            }
        } else {
            stable = None;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "admitted accounting outcomes did not settle: admitted={admitted} database={database_records} journal={journal_records} lost={lost} evidence_matches={evidence_matches}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn latest_generation_state(root: &Path) -> Value {
    let mut states = fs::read_dir(root.join("generations"))
        .expect("read generation states")
        .map(|entry| entry.expect("generation state entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".state.json"))
        })
        .collect::<Vec<_>>();
    states.sort();
    let path = states.last().expect("latest generation state");
    serde_json::from_slice(&fs::read(path).expect("read latest generation state"))
        .expect("valid latest generation state")
}

async fn wait_for_generation_phase(root: &Path, phase: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = latest_generation_state(root);
            if state["phase"] == phase {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("generation did not reach {phase}"))
}

#[derive(Clone, Copy)]
enum ShutdownTrigger {
    Management,
    Signal,
}

async fn trigger_shutdown(
    process: &mut SteveProcess,
    management: std::net::SocketAddr,
    trigger: ShutdownTrigger,
) {
    match trigger {
        ShutdownTrigger::Management => {
            let response = reqwest::Client::new()
                .post(format!("http://{management}/api/v1/system/drain"))
                .send()
                .await
                .expect("request management drain");
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
        ShutdownTrigger::Signal => process.send_sigterm().expect("send SIGTERM"),
    }
}

#[tokio::test]
async fn accounting_shutdown_barriers_complete() {
    for trigger in [ShutdownTrigger::Management, ShutdownTrigger::Signal] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("shutdown.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let mut process =
            SteveProcess::start_with_shutdown_fixture(&database_url, &root, 3, 100, 500, 25)
                .expect("start shutdown fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("shutdown fixture ready");
        let payload = json!({"value": {"shutdown": "barrier"}});
        let response: Value = reqwest::Client::new()
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&payload)
            .send()
            .await
            .expect("send accounting event")
            .error_for_status()
            .expect("echo status")
            .json()
            .await
            .expect("echo JSON");
        assert_eq!(response, payload);

        trigger_shutdown(&mut process, listeners.management, trigger).await;
        let state = wait_for_generation_phase(&root, "reconciled").await;
        assert_eq!(state["admitted_count"], 1);
        assert_eq!(state["worker_completed_count"], 1);
        assert_eq!(state["database_committed_count"], 1);

        match trigger {
            ShutdownTrigger::Management => {
                assert!(process.is_running().expect("inspect drained process"));
                let ready = reqwest::get(format!("http://{}/health/ready", listeners.management))
                    .await
                    .expect("read drained readiness");
                assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
                let live = reqwest::get(format!("http://{}/health/live", listeners.management))
                    .await
                    .expect("read drained liveness");
                assert_eq!(live.status(), reqwest::StatusCode::OK);
                process.send_sigterm().expect("stop drained process");
            }
            ShutdownTrigger::Signal => {}
        }
        let status = process
            .wait_for_exit(Duration::from_secs(5))
            .await
            .expect("shutdown process exits");
        assert!(status.success(), "clean shutdown failed: {status}");
        assert_eq!(sqlite_events(&database_url).await.len(), 1);
        pool.close().await;
    }
}

#[tokio::test]
async fn accounting_shutdown_timeout_survives_restart() {
    for trigger in [ShutdownTrigger::Management, ShutdownTrigger::Signal] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("timeout.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let mut lock = pool.acquire().await.expect("acquire SQLite writer");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *lock)
            .await
            .expect("hold SQLite writer");

        let mut process =
            SteveProcess::start_with_shutdown_fixture(&database_url, &root, 3, 100, 5_000, 25)
                .expect("start timeout fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("timeout fixture ready");
        reqwest::Client::new()
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&json!({"value": {"shutdown": "timeout"}}))
            .send()
            .await
            .expect("send held accounting event")
            .error_for_status()
            .expect("held event response");

        trigger_shutdown(&mut process, listeners.management, trigger).await;
        wait_for_generation_phase(&root, "unclean").await;
        assert!(
            process
                .is_running()
                .expect("predecessor is live before successor startup"),
            "predecessor exited before the live-ownership check"
        );

        let mut successor =
            SteveProcess::start_with_shutdown_fixture(&database_url, &root, 1, 100, 200, 25)
                .expect("start successor while predecessor owns generation");
        let successor_status = successor
            .wait_for_exit(Duration::from_millis(500))
            .await
            .expect("live predecessor journal lock rejects successor");
        assert!(
            !successor_status.success(),
            "successor acquired accounting ownership while predecessor writer was live"
        );
        assert!(
            successor.log_output().contains("busy generation")
                || successor.log_output().contains("still serving")
                || successor
                    .log_output()
                    .contains("timed out acquiring accounting file lock"),
            "successor failed for a reason other than live accounting ownership:\n{}",
            successor.log_output()
        );
        drop(successor);
        assert!(process
            .is_running()
            .expect("inspect predecessor before deadline"));

        let unresolved = wait_for_generation_phase(&root, "unclean").await;
        assert_eq!(unresolved["admitted_count"], 1);
        assert_eq!(unresolved["worker_completed_count"], 0);
        assert_eq!(unresolved["database_committed_count"], 0);
        assert_eq!(unresolved["journal_synced_count"], 0);

        match trigger {
            ShutdownTrigger::Management => {
                assert!(process.is_running().expect("management drain stays alive"));
                let ready = reqwest::get(format!("http://{}/health/ready", listeners.management))
                    .await
                    .expect("timed-out management readiness");
                assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
                let live = reqwest::get(format!("http://{}/health/live", listeners.management))
                    .await
                    .expect("timed-out management liveness");
                assert_eq!(live.status(), reqwest::StatusCode::OK);
                process
                    .send_sigterm()
                    .expect("terminate timed-out management drain");
            }
            ShutdownTrigger::Signal => {}
        }
        let status = process
            .wait_for_exit(Duration::from_secs(5))
            .await
            .expect("timed-out signal exits within the shared bound");
        assert!(
            !status.success(),
            "timeout must be a non-success drain result"
        );
        let mut restarted =
            SteveProcess::start_with_shutdown_fixture(&database_url, &root, 1, 100, 200, 25)
                .expect("restart after predecessor termination");
        let restarted_listeners = restarted
            .wait_ready(Duration::from_secs(3))
            .await
            .expect("restart exposes fail-closed management surface");
        let ready = reqwest::get(format!(
            "http://{}/health/ready",
            restarted_listeners.management
        ))
        .await
        .expect("restart readiness");
        assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let ready: Value = ready.json().await.expect("restart readiness JSON");
        assert_eq!(ready["accounting_incident"]["state"], "unreconciled");
        restarted.send_sigterm().expect("stop fail-closed restart");
        restarted
            .wait_for_exit(Duration::from_secs(3))
            .await
            .expect("fail-closed restart exits");

        sqlx::query("ROLLBACK")
            .execute(&mut *lock)
            .await
            .expect("release SQLite writer");
        drop(lock);
        pool.close().await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn management_drain_timeout_cancels_chat_body_before_accounting_closes() {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(20), async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("stream-timeout.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let first = b"data: {\"id\":\"chatcmpl_timeout\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n";
        let mut upstream = ControlledUpstream::start(
            "/v1/chat/completions",
            first.as_slice(),
            Tail::Bytes(b"data: [DONE]\n\n".as_slice().into()),
        )
        .await
        .expect("start held Chat upstream");
        let mut process = SteveProcess::start_with_stream_shutdown_fixture(
            &upstream.url(),
            &database_url,
            &root,
            1,
            100,
            500,
            25,
        )
        .expect("start held-stream shutdown fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("held-stream fixture ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("held-stream client");
        let response = client
            .post(format!(
                "http://{}/v1/chat/completions",
                listeners.inference
            ))
            .json(&json!({
                "model":"steve-test-model",
                "messages":[{"role":"user","content":"hold until drain deadline"}],
                "stream":true
            }))
            .send()
            .await
            .expect("request held Chat stream");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        upstream
            .wait_for_request(Duration::from_secs(3))
            .await
            .expect("Chat request reached held upstream");
        let mut response = response.bytes_stream();
        assert_eq!(
            response
                .next()
                .await
                .expect("stream ended before first Chat event")
                .expect("read first Chat event"),
            first.as_slice()
        );

        let drain_started = Instant::now();
        trigger_shutdown(
            &mut process,
            listeners.management,
            ShutdownTrigger::Management,
        )
        .await;
        upstream
            .wait_for_body_drop(Duration::from_secs(2))
            .await
            .expect("held upstream body was not cancelled by the drain deadline");
        assert!(
            drain_started.elapsed() < Duration::from_secs(2),
            "held upstream body outlived the one-second drain deadline"
        );
        assert!(!upstream.tail_was_sent());
        let response_end = tokio::time::timeout(Duration::from_secs(2), response.next())
            .await
            .expect("held client response body was not cancelled by the drain deadline");
        assert!(
            response_end.is_none() || response_end.is_some_and(|chunk| chunk.is_err()),
            "held client response emitted data after drain cancellation"
        );
        let terminal = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = sqlite_events(&database_url).await;
                let terminal = rows
                    .into_iter()
                    .filter(|row| row.1 == "chat.attempt.terminal.v1")
                    .collect::<Vec<_>>();
                if terminal.len() == 1 {
                    break terminal.into_iter().next().expect("one terminal event");
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("cancelled Chat terminal accounting was not persisted");
        assert_eq!(
            serde_json::from_str::<Value>(&terminal.2).expect("terminal payload")["status"],
            "cancelled"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            sqlite_events(&database_url)
                .await
                .iter()
                .filter(|row| row.1 == "chat.attempt.terminal.v1")
                .count(),
            1,
            "cancelled Chat terminal accounting must be exact-once"
        );

        assert!(process.is_running().expect("management timeout stays alive"));
        let ready = client
            .get(format!("http://{}/health/ready", listeners.management))
            .send()
            .await
            .expect("timed-out management readiness");
        assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let ready: Value = ready.json().await.expect("timed-out readiness JSON");
        assert_eq!(ready["status"], "not_ready");
        assert_eq!(ready["phase"], "draining");
        assert_eq!(
            latest_generation_state(&root)["phase"],
            "unclean",
            "deadline expiry must retain unresolved generation evidence"
        );

        process.send_sigterm().expect("stop timed-out management drain");
        let status = process
            .wait_for_exit(Duration::from_secs(5))
            .await
            .expect("timed-out management process exits without an orphan");
        assert!(!status.success(), "drain timeout must remain non-success");
        assert!(
            process
                .log_output()
                .contains("drain deadline reached with 1 request(s) in flight"),
            "timeout lost the inflight count captured at the deadline:\n{}",
            process.log_output()
        );
        pool.close().await;
    })
    .await
    .expect("held-stream management drain scenario exceeded 20 seconds");
}

#[cfg(unix)]
#[tokio::test]
async fn management_drain_timeout_cancels_backpressured_chat_without_downstream_poll() {
    use tokio::io::AsyncWriteExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(20), async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("backpressured-stream-timeout.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let first = bytes::Bytes::from(vec![b'x'; 64 * 1024 * 1024]);
        let mut upstream = ControlledUpstream::start(
            "/v1/chat/completions",
            first,
            Tail::Bytes(b"data: [DONE]\n\n".as_slice().into()),
        )
        .await
        .expect("start backpressured Chat upstream");
        let mut process = SteveProcess::start_with_stream_shutdown_fixture(
            &upstream.url(),
            &database_url,
            &root,
            1,
            100,
            500,
            25,
        )
        .expect("start backpressured-stream shutdown fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("backpressured-stream fixture ready");
        let socket = tokio::net::TcpSocket::new_v4().expect("create downstream socket");
        socket
            .set_recv_buffer_size(1024)
            .expect("limit downstream receive buffer");
        let mut downstream = socket
            .connect(listeners.inference)
            .await
            .expect("connect non-reading downstream");
        let body = json!({
            "model":"steve-test-model",
            "messages":[{"role":"user","content":"backpressure until drain deadline"}],
            "stream":true
        })
        .to_string();
        downstream
            .write_all(
                format!(
                    "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                    listeners.inference,
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .expect("write non-reading Chat request");
        upstream
            .wait_for_request(Duration::from_secs(3))
            .await
            .expect("backpressured Chat request reached upstream");
        tokio::time::sleep(Duration::from_millis(250)).await;

        let management = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("management client");
        let drain_started = Instant::now();
        trigger_shutdown(
            &mut process,
            listeners.management,
            ShutdownTrigger::Management,
        )
        .await;
        upstream
            .wait_for_body_drop(Duration::from_secs(2))
            .await
            .expect("backpressured upstream body was not cancelled by the shared deadline");
        assert!(
            drain_started.elapsed() < Duration::from_secs(2),
            "backpressured body outlived the one-second drain deadline"
        );
        assert!(!upstream.tail_was_sent());

        let terminal = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let terminal = sqlite_events(&database_url)
                    .await
                    .into_iter()
                    .filter(|row| row.1 == "chat.attempt.terminal.v1")
                    .collect::<Vec<_>>();
                if terminal.len() == 1 {
                    break terminal.into_iter().next().expect("one terminal event");
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("backpressured Chat terminal accounting was not persisted");
        assert_eq!(
            serde_json::from_str::<Value>(&terminal.2).expect("terminal payload")["status"],
            "cancelled"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            sqlite_events(&database_url)
                .await
                .iter()
                .filter(|row| row.1 == "chat.attempt.terminal.v1")
                .count(),
            1,
            "backpressured Chat terminal accounting must be exact-once"
        );

        assert!(process.is_running().expect("management timeout stays alive"));
        let ready = management
            .get(format!("http://{}/health/ready", listeners.management))
            .send()
            .await
            .expect("backpressured timeout readiness");
        assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        let ready: Value = ready.json().await.expect("backpressured readiness JSON");
        assert_eq!(ready["status"], "not_ready");
        assert_eq!(ready["phase"], "draining");
        assert_eq!(latest_generation_state(&root)["phase"], "unclean");

        let signal_started = Instant::now();
        process
            .send_sigterm()
            .expect("stop backpressured management drain");
        let status = process
            .wait_for_exit(Duration::from_secs(3))
            .await
            .expect("backpressured management process exits without an orphan");
        assert!(
            signal_started.elapsed() < Duration::from_secs(2),
            "shutdown coordinator did not complete within the shared bound"
        );
        assert!(!status.success(), "drain timeout must remain non-success");
        drop(downstream);
        pool.close().await;
    })
    .await
    .expect("backpressured management drain scenario exceeded 20 seconds");
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_preparation_failure_cancels_held_chat_before_accounting_closes() {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(20), async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("prepare-failure.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let first = b"data: {\"id\":\"chatcmpl_prepare_failure\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n";
        let mut upstream = ControlledUpstream::start(
            "/v1/chat/completions",
            first.as_slice(),
            Tail::Bytes(b"data: [DONE]\n\n".as_slice().into()),
        )
        .await
        .expect("start preparation-failure upstream");
        let mut process = SteveProcess::start_with_stream_shutdown_fixture(
            &upstream.url(),
            &database_url,
            &root,
            1,
            100,
            500,
            25,
        )
        .expect("start preparation-failure fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("preparation-failure fixture ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("preparation-failure client");
        let response = client
            .post(format!(
                "http://{}/v1/chat/completions",
                listeners.inference
            ))
            .json(&json!({
                "model":"steve-test-model",
                "messages":[{"role":"user","content":"hold through preparation failure"}],
                "stream":true
            }))
            .send()
            .await
            .expect("request held Chat stream");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        upstream
            .wait_for_request(Duration::from_secs(3))
            .await
            .expect("Chat request reached preparation-failure upstream");
        let mut response = response.bytes_stream();
        assert_eq!(
            response
                .next()
                .await
                .expect("stream ended before first Chat event")
                .expect("read first Chat event"),
            first.as_slice()
        );
        let coordination_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("coordination.lock"))
            .expect("open accounting coordination lock");
        coordination_lock
            .lock()
            .expect("hold accounting coordination lock");

        trigger_shutdown(
            &mut process,
            listeners.management,
            ShutdownTrigger::Management,
        )
        .await;
        upstream
            .wait_for_body_drop(Duration::from_secs(2))
            .await
            .expect("preparation failure left the held upstream body alive");
        assert!(!upstream.tail_was_sent());
        let response_end = tokio::time::timeout(Duration::from_secs(2), response.next())
            .await
            .expect("preparation failure left the client response body alive");
        assert!(response_end.is_none() || response_end.is_some_and(|chunk| chunk.is_err()));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let terminal = sqlite_events(&database_url)
                    .await
                    .into_iter()
                    .filter(|row| row.1 == "chat.attempt.terminal.v1")
                    .collect::<Vec<_>>();
                if terminal.len() == 1 {
                    assert_eq!(
                        serde_json::from_str::<Value>(&terminal[0].2)
                            .expect("terminal payload")["status"],
                        "cancelled"
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("preparation-failure terminal accounting was not persisted once");
        assert!(process
            .is_running()
            .expect("preparation-failure management drain stays alive"));
        let ready = client
            .get(format!("http://{}/health/ready", listeners.management))
            .send()
            .await
            .expect("preparation-failure readiness");
        assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);

        File::unlock(&coordination_lock).expect("release accounting coordination lock");
        process
            .send_sigterm()
            .expect("stop preparation-failure management drain");
        let status = process
            .wait_for_exit(Duration::from_secs(5))
            .await
            .expect("preparation-failure process exits without an orphan");
        assert!(!status.success(), "preparation failure must remain non-success");
        let logs = process.log_output();
        assert!(
            logs.contains("timed out preparing accounting shutdown")
                || logs.contains("timed out acquiring accounting file lock"),
            "shutdown replaced the original preparation error:\n{}",
            logs
        );
        pool.close().await;
    })
    .await
    .expect("preparation-failure cancellation scenario exceeded 20 seconds");
}

#[tokio::test]
async fn accounting_shutdown_bounds_held_nonstream_body() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        temp.path().join("held-body.db").display()
    );
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let _pool = prepare_sqlite(&database_url).await;
    let mut process =
        SteveProcess::start_with_shutdown_fixture(&database_url, &root, 1, 100, 500, 25)
            .expect("start held-body fixture");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("held-body fixture ready");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("held-body client");
    let response = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({
            "value": {"shutdown": "held-nonstream-body"},
            "hold_response_ms": 5_000
        }))
        .send()
        .await
        .expect("held nonstream response headers");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            listeners.management
        ))
        .send()
        .await
        .expect("held-body status")
        .error_for_status()
        .expect("held-body status code")
        .json()
        .await
        .expect("held-body status JSON");
    assert_eq!(status["active_inference_requests"], 1);

    process.send_sigterm().expect("send held-body SIGTERM");
    let exit = process
        .wait_for_exit(Duration::from_secs(3))
        .await
        .expect("held nonstream response is cancelled within shutdown bound");
    assert!(
        !exit.success(),
        "held response deadline must report non-success"
    );
    let state = wait_for_generation_phase(&root, "unclean").await;
    assert_eq!(state["admitted_count"], 1);
    assert!(state["worker_completed_count"].as_u64().is_some());
    drop(response);
}

#[tokio::test]
async fn accounting_shutdown_replay_timeout_stays_unclean() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        temp.path().join("replay-timeout.db").display()
    );
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let pool = prepare_sqlite(&database_url).await;
    let mut lock = pool.acquire().await.expect("acquire SQLite writer");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .expect("hold SQLite writer");
    let mut process =
        SteveProcess::start_with_shutdown_fixture(&database_url, &root, 1, 50, 150, 25)
            .expect("start replay-timeout fixture");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("replay-timeout fixture ready");
    reqwest::Client::new()
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value": {"shutdown": "replay-timeout"}}))
        .send()
        .await
        .expect("send replay-timeout event")
        .error_for_status()
        .expect("replay-timeout response");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if generation_journals(&root)
                .iter()
                .any(|journal| fs::metadata(journal).is_ok_and(|metadata| metadata.len() > 0))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("event spills to the synced journal before shutdown");

    process.send_sigterm().expect("send replay-timeout SIGTERM");
    let exit = process
        .wait_for_exit(Duration::from_secs(3))
        .await
        .expect("replay timeout exits within shutdown bound");
    assert!(!exit.success(), "replay timeout must report non-success");
    let state = wait_for_generation_phase(&root, "unclean").await;
    assert_eq!(state["admitted_count"], 1);
    assert_eq!(state["worker_completed_count"], 1);
    assert_eq!(state["database_committed_count"], 0);
    assert_eq!(state["journal_synced_count"], 1);

    sqlx::query("ROLLBACK")
        .execute(&mut *lock)
        .await
        .expect("release SQLite writer");
}

#[tokio::test]
async fn accounting_counts_refresh_before_later_worker_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        temp.path().join("history-timeout.db").display()
    );
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let pool = prepare_sqlite(&database_url).await;
    let mut endpoint = HeldObjectStoreEndpoint::start()
        .await
        .expect("start held object store");
    let mut process = SteveProcess::start_with_shutdown_and_object_store(
        &database_url,
        &root,
        &endpoint.url(),
        1,
        100,
        500,
        25,
    )
    .expect("start later-worker-timeout fixture");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("later-worker-timeout fixture ready");
    reqwest::Client::new()
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value": {"shutdown": "history-timeout"}}))
        .send()
        .await
        .expect("send history-timeout event")
        .error_for_status()
        .expect("history-timeout response");
    endpoint
        .wait_for_put(Duration::from_secs(3))
        .await
        .expect("history worker reaches held PutObject");
    tokio::time::timeout(Duration::from_secs(3), async {
        while sqlite_events(&database_url).await.len() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("accounting worker commits before shutdown");

    process
        .send_sigterm()
        .expect("send history-timeout SIGTERM");
    let exit = process
        .wait_for_exit(Duration::from_secs(3))
        .await
        .expect("later worker timeout exits within shutdown bound");
    assert!(
        !exit.success(),
        "later worker timeout must report non-success"
    );
    let state = wait_for_generation_phase(&root, "unclean").await;
    assert_eq!(state["admitted_count"], 1);
    assert_eq!(state["worker_completed_count"], 1);
    assert_eq!(state["database_committed_count"], 1);
    assert_eq!(state["journal_synced_count"], 0);

    let _ = endpoint.release();
    endpoint.shutdown().await.expect("stop held object store");
    pool.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn chat_accounting_does_not_delay_response() {
    assert_chat_accounting_nonblocking(false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn chat_accounting_preserves_provisional_unknown_after_journal_timeout() {
    assert_chat_accounting_nonblocking(true).await;
}

#[cfg(unix)]
async fn assert_chat_accounting_nonblocking(force_journal_timeout: bool) {
    use tokio_stream::StreamExt;
    use upstream::{ControlledUpstream, Tail};

    tokio::time::timeout(Duration::from_secs(45), async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting-root");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("chat-saturation.db").display()
        );
        assert!(
            provision(&root).status.success(),
            "provision accounting root"
        );
        let pool = prepare_sqlite(&database_url).await;
        let first = b"data: {\"id\":\"chatcmpl_accounting\",\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}]}\n\n";
        let tail = b"data: {\"id\":\"chatcmpl_accounting\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"index\":0}]}\n\ndata: [DONE]\n\n";
        let mut upstream = ControlledUpstream::start(
            "/v1/chat/completions",
            first.as_slice(),
            Tail::Bytes(tail.as_slice().into()),
        )
        .await
        .expect("start controlled Chat upstream");
        let mut process = SteveProcess::start_with_accounting_fault_fixture(
            Some(&upstream.url()),
            &database_url,
            &root,
            1,
            1,
            100,
            5_000,
            25,
        )
        .expect("start Chat accounting fixture");
        let listeners = process
            .wait_ready(Duration::from_secs(10))
            .await
            .expect("Chat accounting fixture ready");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("HTTP client");
        let mut database_lock = pool.acquire().await.expect("SQLite lock connection");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *database_lock)
            .await
            .expect("hold SQLite writer");
        let coordination_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("coordination.lock"))
            .expect("open accounting coordination lock");
        coordination_lock
            .lock()
            .expect("hold accounting coordination lock");

        let mut admitted = 0_u64;
        for sequence in 0..16 {
            let response = client
                .post(format!("http://{}/api/v1/test/echo", listeners.inference))
                .json(&json!({"value":{"saturation":sequence}}))
                .send()
                .await
                .expect("send saturation accounting event");
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            admitted += 1;
            let status: Value = client
                .get(format!(
                    "http://{}/api/v1/system/status",
                    listeners.management
                ))
                .send()
                .await
                .expect("read queue status")
                .error_for_status()
                .expect("queue status response")
                .json()
                .await
                .expect("queue status JSON");
            if status["queues"]["accounting_spilled"] == 1 {
                break;
            }
        }
        let saturated: Value = client
            .get(format!(
                "http://{}/api/v1/system/status",
                listeners.management
            ))
            .send()
            .await
            .expect("read saturated queue status")
            .error_for_status()
            .expect("saturated queue status response")
            .json()
            .await
            .expect("saturated queue status JSON");
        assert_eq!(
            saturated["queues"]["accounting_spilled"], 1,
            "primary accounting queue did not saturate"
        );

        let spilled_echo_id = force_journal_timeout.then(|| {
            process.log_output().lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|event| event["fields"]["message"] == "accounting event queued for durable journal")
                .expect("spilled echo queue acknowledgement")["fields"]["event_id"]
                .as_str().expect("spilled echo event ID").to_owned()
        });

        let response = client
            .post(format!(
                "http://{}/v1/chat/completions",
                listeners.inference
            ))
            .json(&json!({
                "model":"steve-test-model",
                "messages":[{"role":"user","content":"prove nonblocking accounting"}],
                "stream":true
            }))
            .send()
            .await
            .expect("request Chat stream while accounting is blocked");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let captured = upstream
            .wait_for_request(Duration::from_secs(3))
            .await
            .expect("Chat request reached upstream");
        assert_eq!(captured["model"], "steve-test-model");
        let mut response = response.bytes_stream();
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !received.ends_with(b"\n\n") {
                received.extend_from_slice(
                    &response
                        .next()
                        .await
                        .expect("stream ended before first SSE event")
                        .expect("read first SSE event"),
                );
            }
        })
        .await
        .expect("first SSE progress waited for accounting or upstream tail");
        assert_eq!(received, first);
        assert!(!upstream.tail_was_sent());

        upstream.release_tail();
        tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(chunk) = response.next().await {
                received.extend_from_slice(&chunk.expect("read released Chat tail"));
            }
        })
        .await
        .expect("Chat response waited for accounting durability");
        assert_eq!(received, [first.as_slice(), tail.as_slice()].concat());
        admitted += 1;
        assert!(
            journal_events(&root)
                .iter()
                .all(|event| event["kind"] != "chat.attempt.terminal.v1"),
            "Chat terminal event became durable while its journal writer was locked"
        );

        let triggering = client
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&json!({"value":"fallback-unavailable"}))
            .send()
            .await
            .expect("send fallback-unavailable trigger");
        assert_eq!(
            triggering.status(),
            reqwest::StatusCode::OK,
            "already admitted work must finish forwarding"
        );
        admitted += 1;
        let observed = wait_for_incident(&client, listeners.management, "blocked").await;
        assert_eq!(
            observed["accounting_incident"]["cause"],
            "primary_and_journal_unavailable"
        );
        assert_eq!(
            observed["accounting_incident"]["payloads"]["outcome_totals"]
                ["unrecoverable_lost"],
            1
        );
        let rejected = client
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&json!({"value":"after-accounting-incident"}))
            .send()
            .await
            .expect("post-incident admission response");
        assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(rejected.headers()["cache-control"], "no-store");
        assert_eq!(rejected.headers()["retry-after"], "1");
        let rejected_body: Value = rejected.json().await.expect("incident rejection JSON");
        assert_eq!(
            rejected_body,
            json!({"error":{
                "type":"unavailable",
                "code":"accounting_incident",
                "message":"inference admission stopped by an unresolved accounting incident",
                "incident_id":observed["accounting_incident"]["incident_id"],
                "state":"blocked"
            }})
        );

        if let Some(event_id) = &spilled_echo_id {
            // Observe the first append's real lock timeout before releasing the queued Chat append.
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let failed = process.log_output().lines()
                        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                        .any(|event| event["fields"]["message"] == "accounting journal write failed"
                            && event["fields"]["event_id"] == *event_id
                            && event["fields"]["err"] == "timed out acquiring accounting file lock");
                    if failed { break; }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.expect("first spilled echo did not reach its journal lock timeout");
        }

        File::unlock(&coordination_lock).expect("release accounting coordination lock");
        sqlx::query("COMMIT")
            .execute(&mut *database_lock)
            .await
            .expect("release SQLite writer");
        drop(database_lock);
        let mut last_snapshot = None;
        let settlement = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let durable = journal_events(&root);
                let journaled = durable
                    .iter()
                    .filter(|event| event["kind"] == "chat.attempt.terminal.v1")
                    .count();
                let persisted = incident(&root);
                let lost = persisted["payloads"]["outcome_totals"]["unrecoverable_lost"]
                    .as_u64()
                    .expect("known incident loss count");
                let unknown = persisted["payloads"]["provisional"]["unknown"]
                    .as_u64()
                    .expect("known provisional unknown count");
                assert!(unknown <= 1, "only the first spilled echo may have an unknown journal outcome");
                let rows = sqlite_events(&database_url).await.len() as u64;
                last_snapshot = Some((rows, durable.len() as u64, journaled, lost, unknown));
                if journaled == 1 && lost == 1 && rows + durable.len() as u64 + lost + unknown == admitted {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        if let Err(elapsed) = settlement {
            match last_snapshot {
                Some((sqlite_rows, journal_frames, terminal_chat_frames, unrecoverable_lost, provisional_unknown)) => {
                    panic!(
                        "journal acknowledgement and accounting outcomes did not settle within {elapsed}: admitted={admitted}, sqlite_rows={sqlite_rows}, journal_frames={journal_frames}, terminal_chat_frames={terminal_chat_frames}, unrecoverable_lost={unrecoverable_lost}, provisional_unknown={provisional_unknown}"
                    );
                }
                None => panic!(
                    "journal acknowledgement and accounting outcomes did not settle within {elapsed}: admitted={admitted}, no completed settlement snapshot"
                ),
            }
        }

        let persisted = incident(&root);
        assert_eq!(persisted["state"], "blocked");
        assert_eq!(persisted["payloads"]["outcome_totals"]["unrecoverable_lost"], 1);
        if let Some(event_id) = &spilled_echo_id {
            assert_eq!(persisted["payloads"]["provisional"]["unknown"], 1);
            assert!(sqlite_events(&database_url).await.iter().all(|row| row.0 != *event_id));
            assert!(journal_events(&root).iter().all(|event| event["id"] != *event_id));
        }

        process.send_sigterm().expect("stop Chat accounting fixture");
        assert!(
            process
                .wait_for_exit(Duration::from_secs(8))
                .await
                .expect("Chat accounting fixture exits")
                .success()
        );
        let rows = sqlite_events(&database_url).await;
        let terminal_rows = rows
            .iter()
            .filter(|row| row.1 == "chat.attempt.terminal.v1")
            .collect::<Vec<_>>();
        assert_eq!(terminal_rows.len(), 1, "Chat terminal replay was not exact-once");
        assert_eq!(
            serde_json::from_str::<Value>(&terminal_rows[0].2).expect("terminal payload")["status"],
            "success"
        );
        assert_eq!(
            latest_generation_state(&root)["phase"],
            "unclean",
            "known loss must keep the generation retained instead of retiring it"
        );
        let terminal_journal_events = journal_events(&root)
            .into_iter()
            .filter(|event| event["kind"] == "chat.attempt.terminal.v1")
            .collect::<Vec<_>>();
        assert_eq!(
            terminal_journal_events.len(),
            1,
            "replayed Chat evidence must remain retained"
        );
        assert_eq!(
            terminal_journal_events[0]["id"], terminal_rows[0].0,
            "database replay must acknowledge the retained Chat event ID"
        );
        pool.close().await;
    })
    .await
    .expect("Chat accounting saturation scenario exceeded 45 seconds");
}

async fn induce_runtime_incident(
    root: &Path,
    database_url: &str,
) -> (
    SteveProcess,
    process::Listeners,
    SteveProcess,
    process::Listeners,
    Value,
) {
    assert!(
        provision(root).status.success(),
        "provision accounting root"
    );
    let mut process = SteveProcess::start_with_accounting_fault_fixture(
        None,
        database_url,
        root,
        1,
        1,
        50,
        300,
        25,
    )
    .expect("start fault fixture");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("fault fixture listeners");
    let mut sibling =
        SteveProcess::start_with_database_and_accounting(database_url, root, 50, 300, 25)
            .expect("start overlapping sibling");
    let sibling_listeners = sibling
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("overlapping sibling listeners");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .expect("open SQLite fault connection");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&pool)
        .await
        .expect("hold SQLite writer");
    let coordination = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("coordination.lock"))
        .expect("open coordination lock");
    coordination.lock().expect("hold coordination lock");
    let outage_started = Instant::now();

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTP client");
    let mut requests = Vec::new();
    for sequence in 0..12 {
        let client = client.clone();
        let url = format!("http://{}/api/v1/test/echo", listeners.inference);
        requests.push(tokio::spawn(async move {
            client
                .post(url)
                .json(&json!({"value":{"sequence":sequence}}))
                .send()
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let during_outage = tokio::time::timeout(
        Duration::from_millis(500),
        client
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&json!({"value":"during-incident-publication-outage"}))
            .send(),
    )
    .await
    .expect("admission remained bounded during incident publication outage")
    .expect("outage admission response");
    assert_eq!(
        during_outage.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    tokio::time::timeout(
        Duration::from_millis(500),
        client
            .get(format!(
                "http://{}/api/v1/system/status",
                listeners.management
            ))
            .send(),
    )
    .await
    .expect("status remained bounded during incident publication outage")
    .expect("outage status response")
    .error_for_status()
    .expect("outage status code");
    let mut admitted_responses = 0;
    tokio::time::timeout(Duration::from_secs(1), async {
        for request in requests {
            let response = request
                .await
                .expect("join admitted trigger request")
                .expect("send admitted trigger request");
            if response.status() == reqwest::StatusCode::OK {
                admitted_responses += 1;
            }
        }
    })
    .await
    .expect("triggering admitted responses remained bounded during permanent publication outage");
    assert!(
        admitted_responses > 0,
        "no already-admitted request preserved its response"
    );
    let outage_status: Value = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status: Value = client
                .get(format!(
                    "http://{}/api/v1/system/status",
                    listeners.management
                ))
                .send()
                .await
                .expect("final outage status response")
                .error_for_status()
                .expect("final outage status code")
                .json()
                .await
                .expect("final outage status JSON");
            let lost = status["accounting_incident"]["payloads"]["outcome_totals"]
                ["unrecoverable_lost"]
                .as_u64()
                .unwrap_or(0);
            let unknown = status["accounting_incident"]["payloads"]["provisional"]["unknown"]
                .as_u64()
                .unwrap_or(0);
            if lost + unknown == admitted_responses {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all admitted failures became visible during permanent publication outage");
    let outage_lost = outage_status["accounting_incident"]["payloads"]["outcome_totals"]
        ["unrecoverable_lost"]
        .as_u64()
        .expect("outage known loss count");
    let outage_unknown = outage_status["accounting_incident"]["payloads"]["provisional"]["unknown"]
        .as_u64()
        .expect("outage known uncertainty count");
    assert_eq!(
        outage_lost + outage_unknown,
        admitted_responses,
        "all admitted failures were immediately aggregated while publication was unavailable"
    );
    assert!(
        outage_lost + outage_unknown > 1,
        "multiple failures were not retained during the publication outage"
    );
    if let Some(remaining) = Duration::from_millis(1_250).checked_sub(outage_started.elapsed()) {
        tokio::time::sleep(remaining).await;
    }
    File::unlock(&coordination).expect("release coordination lock");
    let observed = wait_for_incident(&client, listeners.management, "blocked").await;
    sqlx::query("COMMIT")
        .execute(&pool)
        .await
        .expect("release SQLite writer");
    pool.close().await;
    wait_for_accounting_settle(root, database_url, admitted_responses).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if incident(root)["state"] == "blocked" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("checked incident publication");
    let durable = incident(root);
    assert_eq!(
        durable["payloads"]["outcome_totals"]["unrecoverable_lost"],
        outage_lost
    );
    assert_eq!(
        durable["payloads"]["provisional"]["unknown"],
        outage_unknown
    );
    (process, listeners, sibling, sibling_listeners, observed)
}

#[tokio::test]
async fn accounting_incident_preserves_forwarding_and_restart_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join("incident.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    write_offline_config(&config, &database_url, &root);
    let (mut process, listeners, sibling, sibling_listeners, observed) =
        induce_runtime_incident(&root, &database_url).await;
    assert_eq!(observed["accounting_incident"]["state"], "blocked");
    assert_eq!(
        observed["accounting_incident"]["cause"],
        "primary_and_journal_unavailable"
    );

    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client");
    let rejected = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value":"after-latch"}))
        .send()
        .await
        .expect("post-latch inference response");
    assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["cache-control"], "no-store");
    assert_eq!(rejected.headers()["retry-after"], "1");
    let rejected_body: Value = rejected.json().await.expect("incident rejection JSON");
    assert_eq!(
        rejected_body,
        json!({"error":{
            "type":"unavailable",
            "code":"accounting_incident",
            "message":"inference admission stopped by an unresolved accounting incident",
            "incident_id":observed["accounting_incident"]["incident_id"],
            "state":"blocked"
        }})
    );

    let ready = client
        .get(format!("http://{}/health/ready", listeners.management))
        .send()
        .await
        .expect("readiness response");
    assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let ready_body: Value = ready.json().await.expect("readiness JSON");
    assert_eq!(ready_body["status"], "not_ready");
    assert_eq!(ready_body["accounting_incident"]["state"], "blocked");
    let sibling_rejected = client
        .post(format!(
            "http://{}/api/v1/test/echo",
            sibling_listeners.inference
        ))
        .json(&json!({"value":"after-sibling-latch"}))
        .send()
        .await
        .expect("sibling incident rejection");
    assert_eq!(
        sibling_rejected.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        client
            .get(format!("http://{}/health/live", listeners.management))
            .send()
            .await
            .expect("liveness response")
            .status(),
        reqwest::StatusCode::OK
    );

    process.send_sigterm().expect("stop incident process");
    assert!(process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("incident process exits")
        .success());
    let blocked_while_live = incident(&root);
    let blocked_id = blocked_while_live["incident_id"]
        .as_str()
        .expect("incident id while sibling live");
    let blocked_revision = blocked_while_live["revision"]
        .as_u64()
        .expect("incident revision while sibling live")
        .to_string();
    let generation_inventory_before = fs::read_dir(root.join("generations"))
        .expect("generation inventory before rejected maintenance")
        .map(|entry| {
            let path = entry.expect("generation entry").path();
            let bytes = fs::read(&path).expect("generation evidence");
            (
                path.file_name().expect("generation name").to_owned(),
                sha256(&bytes),
            )
        })
        .collect::<Vec<_>>();
    let rejected_live = configured_accounting(
        &config,
        "acknowledge",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", blocked_id),
            ("--revision", &blocked_revision),
            ("--kind", "accepted_loss_and_uncertainty"),
            ("--evidence-ref", "ticket://must-remain-offline"),
        ],
    );
    assert!(
        !rejected_live.status.success(),
        "acknowledgement mutated accounting while a generation was live"
    );
    let generation_inventory_after = fs::read_dir(root.join("generations"))
        .expect("generation inventory after rejected maintenance")
        .map(|entry| {
            let path = entry.expect("generation entry").path();
            let bytes = fs::read(&path).expect("generation evidence");
            (
                path.file_name().expect("generation name").to_owned(),
                sha256(&bytes),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(generation_inventory_after, generation_inventory_before);
    drop(sibling);
    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart incident process");
    let restarted_listeners = restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("restart exposes blocked management surface");
    let restarted_status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            restarted_listeners.management
        ))
        .send()
        .await
        .expect("restart status")
        .error_for_status()
        .expect("restart status code")
        .json()
        .await
        .expect("restart status JSON");
    assert_eq!(
        restarted_status["accounting_incident"]["state"],
        "unreconciled"
    );
    assert_eq!(
        restarted_status["accounting_incident"]["incident_id"],
        observed["accounting_incident"]["incident_id"]
    );
    assert_eq!(
        restarted_status["accounting_incident"]["payloads"]["provisional"]["unknown"],
        Value::Null,
        "the second owner's abandoned Active tuple did not add uncertainty"
    );
    let restart_rejected = client
        .post(format!(
            "http://{}/api/v1/test/echo",
            restarted_listeners.inference
        ))
        .json(&json!({"value":"after-restart"}))
        .send()
        .await
        .expect("restart inference rejection");
    assert_eq!(
        restart_rejected.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn sibling_fails_closed_when_an_active_owner_crashes_before_incident_publication() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join("sibling-crash.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    write_offline_config(&config, &database_url, &root);
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let mut predecessor = SteveProcess::start_with_accounting_fault_fixture(
        None,
        &database_url,
        &root,
        1,
        1,
        50,
        300,
        25,
    )
    .expect("start predecessor");
    let predecessor_listeners = predecessor
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("predecessor ready");
    let mut sibling =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("start sibling");
    let listeners = sibling
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("sibling ready");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("open SQLite fault connection");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&pool)
        .await
        .expect("hold SQLite writer");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client");
    let durable = client
        .post(format!(
            "http://{}/api/v1/test/echo",
            predecessor_listeners.inference
        ))
        .json(&json!({"value":"durable-before-publication-crash"}))
        .send()
        .await
        .expect("durable trigger response");
    assert_eq!(durable.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if generation_journals(&root)
                .iter()
                .any(|journal| fs::metadata(journal).is_ok_and(|metadata| metadata.len() > 0))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("first failed write reached the durable journal");
    let coordination = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("coordination.lock"))
        .expect("open coordination lock");
    coordination.lock().expect("hold coordination lock");

    let triggering = client
        .post(format!(
            "http://{}/api/v1/test/echo",
            predecessor_listeners.inference
        ))
        .json(&json!({"value":"crash-before-incident-publication"}))
        .send()
        .await
        .expect("triggering response");
    assert_eq!(triggering.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status: Value = client
                .get(format!(
                    "http://{}/api/v1/system/status",
                    predecessor_listeners.management
                ))
                .send()
                .await
                .expect("predecessor status")
                .json()
                .await
                .expect("predecessor status JSON");
            if status["accounting_incident"]["state"] == "blocked" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("predecessor latched incident in memory");
    drop(predecessor);
    File::unlock(&coordination).expect("release coordination lock after crash");

    let rejected = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value":"after-owner-crash"}))
        .send()
        .await
        .expect("sibling crash admission response");
    assert_eq!(rejected.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            listeners.management
        ))
        .send()
        .await
        .expect("sibling crash status")
        .json()
        .await
        .expect("sibling crash status JSON");
    assert_eq!(status["accounting_incident"]["state"], "unreconciled");

    sibling.send_sigterm().expect("stop sibling");
    assert!(sibling
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("sibling exits")
        .success());
    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart after owner crash");
    let restarted_listeners = restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("restart exposes incident management surfaces");
    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            restarted_listeners.management
        ))
        .send()
        .await
        .expect("recovered incident status")
        .json()
        .await
        .expect("recovered incident status JSON");
    assert_eq!(status["accounting_incident"]["state"], "unreconciled");
    assert!(status["accounting_incident"]["incident_id"].is_string());
    assert_eq!(
        status["accounting_incident"]["payloads"]["provisional"]["unknown"],
        Value::Null
    );
    restarted
        .send_sigterm()
        .expect("stop recovered incident process");
    assert!(restarted
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("recovered incident process exits")
        .success());

    let protected = incident(&root);
    let incident_id = protected["incident_id"].as_str().expect("incident id");
    let revision = protected["revision"].as_u64().expect("incident revision");
    let revision = revision.to_string();
    let rejected = configured_accounting(
        &config,
        "acknowledge",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision),
            ("--kind", "accepted_uncertainty"),
            ("--evidence-ref", "ticket://crash-before-publication"),
        ],
    );
    assert!(
        !rejected.status.success(),
        "operator disposition skipped unresolved durable replay"
    );
    sqlx::query("COMMIT")
        .execute(&pool)
        .await
        .expect("release SQLite writer after rejected disposition");
    pool.close().await;
}

#[tokio::test]
async fn admission_fails_closed_on_journal_published_before_generation_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join("orphan-journal.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("start accounting process");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("accounting process ready");
    let orphan = root
        .join("generations")
        .join("019d23ff-0000-7000-8000-000000000001.journal");
    File::create(&orphan)
        .expect("publish crash-window journal")
        .sync_all()
        .expect("sync crash-window journal");

    let response = reqwest::Client::new()
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value":"after-journal-before-state-crash"}))
        .send()
        .await
        .expect("orphan journal admission response");
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn accounting_disposition_is_auditable_and_publicly_redacted() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join("disposition.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    write_offline_config(&config, &database_url, &root);
    let (mut process, _, mut sibling, _, _) = induce_runtime_incident(&root, &database_url).await;
    process.send_sigterm().expect("stop incident process");
    assert!(process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("incident process exits")
        .success());
    sibling.send_sigterm().expect("stop incident sibling");
    assert!(sibling
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("incident sibling exits")
        .success());

    let blocked = incident(&root);
    assert!(
        blocked["payloads"]["outcome_totals"]["unrecoverable_lost"]
            .as_u64()
            .is_some_and(|lost| lost > 0),
        "real queue loss did not produce a confirmed lost outcome: {blocked}"
    );
    assert!(
        blocked["payloads"]["provisional"]["unknown"]
            .as_u64()
            .is_some_and(|unknown| unknown > 0),
        "real database timeout did not produce provisional uncertainty: {blocked}"
    );
    let incident_id = blocked["incident_id"].as_str().expect("incident id");
    let revision = blocked["revision"].as_u64().expect("incident revision");
    let revision_string = revision.to_string();
    let blocked_bytes = fs::read(root.join("incident.json")).expect("blocked incident bytes");
    let blocked_checksum = fs::read(format!("{}.sha256", root.join("incident.json").display()))
        .expect("blocked incident checksum");
    let acknowledged = configured_accounting(
        &config,
        "acknowledge",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--kind", "accepted_loss_and_uncertainty"),
            ("--evidence-ref", "ticket://m1/operator-loss-review"),
        ],
    );
    assert!(
        acknowledged.status.success(),
        "acknowledge failed: {}",
        String::from_utf8_lossy(&acknowledged.stderr)
    );
    fs::write(root.join("incident.json"), &blocked_bytes)
        .expect("restore pre-publication incident evidence");
    fs::write(
        format!("{}.sha256", root.join("incident.json").display()),
        &blocked_checksum,
    )
    .expect("restore pre-publication incident checksum");
    let resumed = configured_accounting(
        &config,
        "acknowledge",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--kind", "accepted_loss_and_uncertainty"),
            ("--evidence-ref", "ticket://m1/operator-loss-review"),
        ],
    );
    assert!(
        resumed.status.success(),
        "interrupted acknowledgement did not resume: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let acknowledged_record: Value =
        serde_json::from_slice(&resumed.stdout).expect("resumed acknowledge output JSON");
    assert_eq!(
        acknowledged_record["payloads"]["provisional"]["unknown"],
        blocked["payloads"]["provisional"]["unknown"],
        "acknowledgement changed the known uncertainty count"
    );
    assert!(
        fs::read_dir(root.join("generations"))
            .expect("read generations")
            .any(|entry| {
                entry
                    .expect("generation entry")
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".replay.json"))
            }),
        "offline acknowledgement must retain replay evidence"
    );
    let acknowledged_revision = acknowledged_record["revision"]
        .as_u64()
        .expect("acknowledged revision");
    let acknowledged_revision_string = acknowledged_revision.to_string();

    let audit = configured_accounting(
        &config,
        "audit",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &acknowledged_revision_string),
        ],
    );
    assert!(
        audit.status.success(),
        "audit failed: {}",
        String::from_utf8_lossy(&audit.stderr)
    );
    let audit_record: Value = serde_json::from_slice(&audit.stdout).expect("audit output JSON");
    assert_eq!(
        audit_record["disposition"]["kind"],
        "accepted_loss_and_uncertainty"
    );
    assert_eq!(
        audit_record["disposition"]["evidence_ref"],
        "ticket://m1/operator-loss-review"
    );
    let actor = audit_record["disposition"]["actor"]
        .as_str()
        .expect("protected actor");
    assert!(
        !actor.contains("forged"),
        "environment forged actor: {actor}"
    );

    let audit_path = root.join("audit.json");
    let audit_bytes = fs::read(&audit_path).expect("protected audit bytes");
    let audit_checksum =
        fs::read(format!("{}.sha256", audit_path.display())).expect("protected audit checksum");
    let mut unsupported_audit: Value =
        serde_json::from_slice(&audit_bytes).expect("protected audit JSON");
    unsupported_audit
        .as_array_mut()
        .expect("audit records")
        .last_mut()
        .expect("acknowledgement audit")["format_version"] = json!(2);
    let unsupported_bytes =
        serde_json::to_vec_pretty(&unsupported_audit).expect("unsupported audit bytes");
    fs::write(&audit_path, &unsupported_bytes).expect("write unsupported audit");
    fs::write(
        format!("{}.sha256", audit_path.display()),
        sha256(&unsupported_bytes),
    )
    .expect("write unsupported audit checksum");
    let mut unsupported =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("start with unsupported audit");
    assert!(
        unsupported
            .wait_ready(Duration::from_secs(5))
            .await
            .is_err(),
        "unsupported checked audit evidence reopened admission"
    );
    fs::write(&audit_path, audit_bytes).expect("restore protected audit");
    fs::write(format!("{}.sha256", audit_path.display()), audit_checksum)
        .expect("restore protected audit checksum");

    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart acknowledged process");
    let listeners = restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("acknowledged restart is ready");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client");
    let ready = client
        .get(format!("http://{}/health/ready", listeners.management))
        .send()
        .await
        .expect("acknowledged readiness");
    assert_eq!(ready.status(), reqwest::StatusCode::OK);
    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            listeners.management
        ))
        .send()
        .await
        .expect("acknowledged status")
        .json()
        .await
        .expect("acknowledged status JSON");
    assert_eq!(status["accounting_incident"]["state"], "acknowledged");
    assert_eq!(
        status["accounting_incident"]["disposition"]["actor"],
        Value::Null
    );
    assert_eq!(
        status["accounting_incident"]["disposition"]["evidence_ref"],
        Value::Null
    );
    let admitted = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&json!({"value":"new-work-after-acknowledgement"}))
        .send()
        .await
        .expect("post-acknowledgement inference");
    assert_eq!(admitted.status(), reqwest::StatusCode::OK);
    drop(restarted);
    let mut uncovered_active =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart after uncovered active generation crash");
    let listeners = uncovered_active
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("uncovered active generation exposes blocked management surface");
    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            listeners.management
        ))
        .send()
        .await
        .expect("uncovered active status")
        .json()
        .await
        .expect("uncovered active status JSON");
    assert_eq!(status["accounting_incident"]["state"], "unreconciled");
}

async fn assert_ordinary_conflict_resolution(authoritative: &str) {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join(format!("ordinary-{authoritative}.db"));
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    write_offline_config(&config, &database_url, &root);
    let pool = prepare_sqlite(&database_url).await;
    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 200, 25)
            .expect("start ordinary conflict process");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("ordinary conflict process ready");
    let mut lock = pool.acquire().await.expect("acquire SQLite writer");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .expect("hold SQLite writer");
    let request = json!({"value":{"authority":authoritative,"side":"journal"}});
    let response = reqwest::Client::new()
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&request)
        .send()
        .await
        .expect("send ordinary conflict event");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let (journal, journal_bytes) = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some((journal, bytes)) =
                generation_journals(&root).into_iter().find_map(|journal| {
                    let bytes = fs::read(&journal).expect("read ordinary conflict journal");
                    (!bytes.is_empty()).then_some((journal, bytes))
                })
            {
                break (journal, bytes);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("ordinary event reached durable journal");
    let event: Value = serde_json::from_slice(
        journal_bytes
            .strip_suffix(b"\n")
            .expect("journal frame newline"),
    )
    .expect("ordinary journal event");
    let event_id = event["id"].as_str().expect("ordinary event id");
    let database_payload =
        json!({"value":{"authority":authoritative,"side":"database"}}).to_string();
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(event_id)
    .bind(event["kind"].as_str().expect("ordinary event kind"))
    .bind(&database_payload)
    .bind(
        event["created_at"]
            .as_str()
            .expect("ordinary event timestamp"),
    )
    .execute(&mut *lock)
    .await
    .expect("seed ordinary conflicting row");
    sqlx::query("COMMIT")
        .execute(&mut *lock)
        .await
        .expect("publish ordinary conflicting row");
    drop(lock);

    process
        .send_sigterm()
        .expect("stop ordinary conflict process");
    process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("ordinary conflict process exits");
    let generation_id = journal
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("ordinary generation id");
    let state_path = root
        .join("generations")
        .join(format!("{generation_id}.state.json"));
    let before_state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("ordinary generation state"))
            .expect("valid ordinary generation state");
    assert!(
        matches!(before_state["phase"].as_str(), Some("draining" | "unclean")),
        "ordinary shutdown did not publish a completion-bearing replay phase"
    );
    assert_eq!(before_state["admitted_count"], 1);
    assert_eq!(before_state["worker_completed_count"], 1);
    assert_eq!(before_state["journal_synced_count"], 1);
    assert_eq!(before_state["database_committed_count"], 0);

    let conflict = incident(&root);
    assert_eq!(conflict["state"], "blocked");
    assert_eq!(conflict["cause"], "replay_content_conflict");
    let incident_id = conflict["incident_id"].as_str().expect("incident id");
    let revision = conflict["revision"].as_u64().expect("incident revision");
    let revision_string = revision.to_string();

    let audit_bytes = fs::read(root.join("audit.json")).expect("audit evidence");
    fs::write(root.join(".audit.json.staging"), &audit_bytes)
        .expect("stage untouched audit evidence");
    fs::write(
        root.join(".audit.json.sha256.staging"),
        sha256(&audit_bytes),
    )
    .expect("stage untouched audit checksum");
    let before_audit = accounting_root_bytes(&root);
    let audited = configured_accounting(
        &config,
        "audit",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
        ],
    );
    assert!(
        audited.status.success(),
        "unresolved conflict audit failed: {}",
        String::from_utf8_lossy(&audited.stderr)
    );
    let audited: Value =
        serde_json::from_slice(&audited.stdout).expect("unresolved conflict audit JSON");
    assert_eq!(audited["operation"], "inspect_conflict");
    assert_eq!(audited["incident_id"], incident_id);
    assert_eq!(audited["revision"], revision);
    assert_eq!(
        audited["conflicts"]
            .as_array()
            .expect("audited conflicts")
            .len(),
        1
    );
    let audited_conflict = &audited["conflicts"][0];
    assert_eq!(audited_conflict["event_id"], event_id);
    assert_eq!(audited_conflict["generation_id"], generation_id);
    let snapshot_path = PathBuf::from(
        audited_conflict["conflict_snapshot_ref"]
            .as_str()
            .expect("conflict snapshot path"),
    );
    let snapshot_bytes = fs::read(&snapshot_path).expect("protected conflict snapshot");
    let snapshot: Value =
        serde_json::from_slice(&snapshot_bytes).expect("protected conflict snapshot JSON");
    for field in [
        "journal_offset",
        "journal_length",
        "journal_record_digest",
        "source",
        "existing",
        "existing_row_digest",
        "verification_id",
        "backend",
        "transaction_isolation",
        "read_evidence",
        "observed_at",
    ] {
        assert_eq!(
            audited_conflict[field], snapshot[field],
            "audit field {field}"
        );
    }
    assert_eq!(audited_conflict["source"]["id"], event["id"]);
    assert_eq!(audited_conflict["source"]["kind"], event["kind"]);
    assert_eq!(
        audited_conflict["source"]["payload"],
        event["payload"].to_string()
    );
    assert_eq!(
        audited_conflict["source"]["created_at"],
        event["created_at"]
    );
    assert_eq!(audited_conflict["existing"]["payload"], database_payload);
    assert_eq!(
        audited_conflict["conflict_snapshot_digest"],
        sha256(&snapshot_bytes)
    );
    let coordination: Value = serde_json::from_slice(
        &fs::read(root.join("coordination.json")).expect("coordination evidence"),
    )
    .expect("coordination JSON");
    assert_eq!(audited["coverage"], coordination["coverage"]);
    assert_eq!(
        accounting_root_bytes(&root),
        before_audit,
        "read-only audit changed protected root bytes"
    );

    let resolved = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--event-id", event_id),
            ("--authoritative", authoritative),
            ("--evidence-ref", "ticket://m1/ordinary-conflict-resolution"),
        ],
    );
    assert!(
        resolved.status.success(),
        "ordinary conflict resolution failed: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let resolved: Value =
        serde_json::from_slice(&resolved.stdout).expect("ordinary resolution JSON");
    assert_eq!(resolved["state"], "clear");
    let after_state: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("resolved ordinary state"))
            .expect("valid resolved ordinary state");
    assert_eq!(after_state["phase"], "reconciled");
    for field in [
        "admitted_count",
        "worker_completed_count",
        "journal_synced_count",
    ] {
        assert_eq!(after_state[field], before_state[field], "preserve {field}");
    }
    assert_eq!(after_state["database_committed_count"], 1);
    let row = sqlite_events(&database_url).await;
    assert_eq!(row.len(), 1);
    assert_eq!(
        row[0].2,
        if authoritative == "journal" {
            event["payload"].to_string()
        } else {
            database_payload
        }
    );
    let completed_revision = resolved["revision"]
        .as_u64()
        .expect("completed revision")
        .to_string();
    let completed = configured_accounting(
        &config,
        "audit",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &completed_revision),
        ],
    );
    assert!(completed.status.success());
    let completed: Value = serde_json::from_slice(&completed.stdout).expect("completed audit JSON");
    assert_eq!(completed["operation"], "resolve_conflict");
    pool.close().await;
}

#[tokio::test]
async fn ordinary_generation_conflict_resolves_with_journal_authority() {
    assert_ordinary_conflict_resolution("journal").await;
}

#[tokio::test]
async fn ordinary_generation_conflict_resolves_with_database_authority() {
    assert_ordinary_conflict_resolution("database").await;
}

#[tokio::test]
async fn failed_replay_rebinds_after_read_only_generation_on_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("failed-replay.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let database_path = temp.path().join("failed-replay.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let adoption = adopt_fixture(&source, &root, &assertion, REPLAY_EVENT_ONE);
    let pool = prepare_sqlite(&database_url).await;
    sqlx::query(
        "CREATE TRIGGER fail_replay BEFORE INSERT ON steve_background_events
         WHEN NEW.id = '01995200-0000-7000-8000-000000000101'
         BEGIN SELECT RAISE(FAIL, 'forced replay failure'); END",
    )
    .execute(&pool)
    .await
    .expect("install replay failure trigger");
    let mut blocked = incident(&root);
    blocked["revision"] = json!(blocked["revision"].as_u64().expect("incident revision") + 1);
    blocked["state"] = json!("blocked");
    blocked["incident_id"] = json!("019d2400-0000-7000-8000-000000000001");
    blocked["first_observed_at"] = json!("2026-09-30T00:00:00Z");
    blocked["cause"] = json!("primary_and_journal_unavailable");
    write_checked_fixture(&root.join("incident.json"), &blocked);

    let mut failed =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 200, 25)
            .expect("start failed replay process");
    failed
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("failed replay publishes read-only management process");
    let manifest_path = root
        .join("generations")
        .join(format!("{}.replay.json", generation_id(&adoption)));
    let failed_manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("failed replay manifest"))
            .expect("failed replay manifest JSON");
    assert!(matches!(
        failed_manifest["receipts"][0]["outcome"].as_str(),
        Some("failed" | "unknown")
    ));
    let original_source_revision = failed_manifest["source_coordination_revision"]
        .as_u64()
        .expect("failed replay source coordination revision");
    let advanced_coordination: Value = serde_json::from_slice(
        &fs::read(root.join("coordination.json")).expect("advanced coordination"),
    )
    .expect("advanced coordination JSON");
    assert!(
        advanced_coordination["revision"]
            .as_u64()
            .expect("advanced coordination revision")
            > original_source_revision,
        "read-only generation did not advance coordination"
    );
    drop(failed);
    sqlx::query("DROP TRIGGER fail_replay")
        .execute(&pool)
        .await
        .expect("recover replay database");

    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 500, 25)
            .expect("restart recovered replay process");
    restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("recovered replay restart publishes management process");
    let recovered_manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("recovered replay manifest"))
            .expect("recovered replay manifest JSON");
    assert_eq!(recovered_manifest["receipts"][0]["outcome"], "inserted");
    assert!(
        recovered_manifest["source_coordination_revision"]
            .as_u64()
            .expect("rebound coordination revision")
            > original_source_revision
    );
    let completed: Value =
        serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("completed adoption"))
            .expect("completed adoption JSON");
    assert_eq!(completed["state"], "complete");
    assert_eq!(sqlite_events(&database_url).await.len(), 1);
    restarted
        .send_sigterm()
        .expect("stop recovered replay process");
    restarted
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("recovered replay process exits");
    pool.close().await;
}

#[tokio::test]
async fn accounting_conflict_recovery_is_offline_and_verified() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("conflict.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let database_path = temp.path().join("conflict.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    let conflict_events = [REPLAY_EVENT_ONE, REPLAY_EVENT_ONE, REPLAY_EVENT_TWO].concat();
    let adoption = adopt_fixture(&source, &root, &assertion, &conflict_events);
    let pool = prepare_sqlite(&database_url).await;
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind("01995200-0000-7000-8000-000000000101")
    .bind("test.replay.v1")
    .bind("{\"value\":999}")
    .bind("2026-09-29T00:00:01Z")
    .execute(&pool)
    .await
    .expect("seed conflicting row");
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind("01995200-0000-7000-8000-000000000102")
    .bind("test.replay.v1")
    .bind("{\"value\":998}")
    .bind("2026-09-29T00:00:02Z")
    .execute(&pool)
    .await
    .expect("seed second conflicting row");
    pool.close().await;
    write_offline_config(&config, &database_url, &root);

    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("start conflicting process");
    let listeners = process
        .wait_ready(Duration::from_secs(5))
        .await
        .expect("conflict process exposes management surfaces");
    let conflict = incident(&root);
    assert_eq!(conflict["cause"], "replay_content_conflict");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("HTTP client");
    assert_eq!(
        client
            .get(format!("http://{}/health/ready", listeners.management))
            .send()
            .await
            .expect("conflict readiness")
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        client
            .get(format!("http://{}/health/live", listeners.management))
            .send()
            .await
            .expect("conflict liveness")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .post(format!("http://{}/api/v1/test/echo", listeners.inference))
            .json(&json!({"value":"blocked-by-conflict"}))
            .send()
            .await
            .expect("conflict inference rejection")
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    let incident_id = conflict["incident_id"]
        .as_str()
        .expect("conflict incident id");
    let revision = conflict["revision"].as_u64().expect("conflict revision");
    let revision_string = revision.to_string();
    process.send_sigterm().expect("stop conflict process");
    assert!(process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("conflict process exits")
        .success());

    let rejected_acknowledgement = configured_accounting(
        &config,
        "acknowledge",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--kind", "accepted_uncertainty"),
            ("--evidence-ref", "ticket://invalid-conflict-ack"),
        ],
    );
    assert!(!rejected_acknowledgement.status.success());

    let stale_revision = revision.saturating_sub(1).to_string();
    let stale = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &stale_revision),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "database"),
            ("--evidence-ref", "ticket://m1/conflict-resolution"),
        ],
    );
    assert!(!stale.status.success(), "stale conflict revision accepted");

    let replay_path = fs::read_dir(root.join("generations"))
        .expect("read conflict generations")
        .map(|entry| entry.expect("conflict generation entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".replay.json"))
        })
        .expect("conflict replay manifest");
    let pre_resolution_manifest = fs::read(&replay_path).expect("pre-resolution manifest");
    let pre_resolution_manifest_checksum =
        fs::read(format!("{}.sha256", replay_path.display())).expect("manifest checksum");
    let incident_path = root.join("incident.json");
    let pre_resolution_incident = fs::read(&incident_path).expect("pre-resolution incident");
    let pre_resolution_incident_checksum =
        fs::read(format!("{}.sha256", incident_path.display())).expect("incident checksum");

    let crash_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("open conflict crash-window database lock");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&crash_pool)
        .await
        .expect("hold conflict database mutation");
    let startup_lock = acquire_accounting_startup_lock().expect("serialize interrupted command");
    let mut interrupted = steve_command();
    interrupted
        .arg("--config")
        .arg(&config)
        .args([
            "accounting",
            "resolve-conflict",
            "--root",
            root.to_str().expect("root path"),
            "--incident",
            incident_id,
            "--revision",
            &revision_string,
            "--event-id",
            "01995200-0000-7000-8000-000000000101",
            "--authoritative",
            "journal",
            "--evidence-ref",
            "ticket://m1/conflict-resolution",
        ])
        .env("PATH", "/nonexistent");
    let mut interrupted = interrupted.spawn().expect("spawn interrupted resolution");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let audit: Value = serde_json::from_slice(
                &fs::read(root.join("audit.json")).expect("read prepared conflict audit"),
            )
            .expect("prepared conflict audit JSON");
            if audit.as_array().is_some_and(|records| {
                records.iter().any(|record| {
                    record["operation"] == "prepare_conflict_resolution"
                        && record["event_id"] == "01995200-0000-7000-8000-000000000101"
                })
            }) {
                break;
            }
            assert!(
                interrupted
                    .try_wait()
                    .expect("poll interrupted resolution")
                    .is_none(),
                "resolution exited before publishing prepared evidence"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prepared evidence precedes database mutation");
    interrupted.kill().expect("crash prepared resolution");
    interrupted.wait().expect("reap prepared resolution");
    File::unlock(&startup_lock).expect("release interrupted command lock");
    sqlx::query("COMMIT")
        .execute(&crash_pool)
        .await
        .expect("release conflict database mutation");
    crash_pool.close().await;
    assert_eq!(
        fs::read(&replay_path).expect("manifest after prepared crash"),
        pre_resolution_manifest
    );
    assert_eq!(
        fs::read(format!("{}.sha256", replay_path.display()))
            .expect("manifest checksum after prepared crash"),
        pre_resolution_manifest_checksum
    );
    assert_eq!(
        fs::read(&incident_path).expect("incident after prepared crash"),
        pre_resolution_incident
    );
    assert_eq!(
        fs::read(format!("{}.sha256", incident_path.display()))
            .expect("incident checksum after prepared crash"),
        pre_resolution_incident_checksum
    );
    let resumed = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "journal"),
            ("--evidence-ref", "ticket://m1/conflict-resolution"),
        ],
    );
    assert!(
        resumed.status.success(),
        "interrupted conflict resolution did not resume: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resolution: Value =
        serde_json::from_slice(&resumed.stdout).expect("resumed conflict resolution JSON");
    assert_eq!(resolution["state"], "blocked");
    assert_eq!(resolution["authoritative"], "journal");
    fs::write(&incident_path, &pre_resolution_incident)
        .expect("restore incident before completed resolution publication");
    fs::write(
        format!("{}.sha256", incident_path.display()),
        &pre_resolution_incident_checksum,
    )
    .expect("restore incident checksum before completed resolution publication");
    let resumed_publication = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "journal"),
            ("--evidence-ref", "ticket://m1/conflict-resolution"),
        ],
    );
    assert!(
        resumed_publication.status.success(),
        "published receipt did not resume incident completion: {}",
        String::from_utf8_lossy(&resumed_publication.stderr)
    );
    let resolution: Value = serde_json::from_slice(&resumed_publication.stdout)
        .expect("resumed incident publication JSON");
    let completed_revision = resolution["revision"]
        .as_u64()
        .expect("completed resolution revision")
        .to_string();
    let completed_audit = configured_accounting(
        &config,
        "audit",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &completed_revision),
        ],
    );
    assert!(completed_audit.status.success());
    let completed_audit: Value =
        serde_json::from_slice(&completed_audit.stdout).expect("completed conflict audit JSON");
    assert_eq!(completed_audit["operation"], "resolve_conflict");
    let repeated_revision = resolution["revision"]
        .as_u64()
        .expect("repeated conflict revision")
        .to_string();
    let repeated = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &repeated_revision),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "journal"),
            ("--evidence-ref", "ticket://m1/repeated-conflict-resolution"),
        ],
    );
    assert!(
        repeated.status.success(),
        "repeated same-id conflict remained wedged: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let repeated: Value =
        serde_json::from_slice(&repeated.stdout).expect("repeated conflict resolution JSON");
    assert_eq!(repeated["state"], "blocked");
    let second_revision = repeated["revision"]
        .as_u64()
        .expect("second event conflict revision")
        .to_string();
    let coordination_path = root.join("coordination.json");
    let pre_reconciled_coordination =
        fs::read(&coordination_path).expect("coordination before reconciled state publication");
    let pre_reconciled_coordination_checksum =
        fs::read(format!("{}.sha256", coordination_path.display()))
            .expect("coordination checksum before reconciled state publication");
    let resolved = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &second_revision),
            ("--event-id", "01995200-0000-7000-8000-000000000102"),
            ("--authoritative", "database"),
            ("--evidence-ref", "ticket://m1/conflict-resolution-2"),
        ],
    );
    assert!(
        resolved.status.success(),
        "resolve second conflict failed: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let resolution: Value =
        serde_json::from_slice(&resolved.stdout).expect("second conflict resolution JSON");
    assert_eq!(resolution["state"], "clear");
    fs::write(&coordination_path, pre_reconciled_coordination)
        .expect("restore state-before-coordination crash window");
    fs::write(
        format!("{}.sha256", coordination_path.display()),
        pre_reconciled_coordination_checksum,
    )
    .expect("restore state-before-coordination coordination checksum");

    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart resolved conflict");
    restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("resolved conflict permits readiness");
    assert_eq!(
        sqlite_events(&database_url).await,
        vec![
            (
                "01995200-0000-7000-8000-000000000101".into(),
                "test.replay.v1".into(),
                "{\"value\":1}".into(),
                "2026-09-29T00:00:01Z".into(),
            ),
            (
                "01995200-0000-7000-8000-000000000102".into(),
                "test.replay.v1".into(),
                "{\"value\":998}".into(),
                "2026-09-29T00:00:02Z".into(),
            ),
        ]
    );
    retained_artifacts_match(&root, &adoption, &conflict_events);
    restarted
        .send_sigterm()
        .expect("stop verified conflict process");
    assert!(restarted
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("verified conflict process exits")
        .success());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("open changed conflict database");
    sqlx::query("UPDATE steve_background_events SET payload = ? WHERE id = ?")
        .bind("{\"value\":997}")
        .bind("01995200-0000-7000-8000-000000000102")
        .execute(&pool)
        .await
        .expect("change resolved database row");
    pool.close().await;
    let mut changed =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("restart changed conflict evidence");
    let listeners = changed
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("changed evidence exposes blocked management surface");
    let status: Value = client
        .get(format!(
            "http://{}/api/v1/system/status",
            listeners.management
        ))
        .send()
        .await
        .expect("changed evidence status")
        .json()
        .await
        .expect("changed evidence status JSON");
    assert_eq!(status["accounting_incident"]["state"], "unreconciled");
}

#[tokio::test]
async fn distinct_same_id_conflicts_reject_journal_authority_before_mutation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("distinct-conflict.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let database_path = temp.path().join("distinct-conflict.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let config = temp.path().join("offline.toml");
    write_offline_config(&config, &database_url, &root);
    adopt_fixture(
        &source,
        &root,
        &assertion,
        &[REPLAY_EVENT_ONE, REPLAY_EVENT_ONE_DIFFERENT].concat(),
    );
    let pool = prepare_sqlite(&database_url).await;
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind("01995200-0000-7000-8000-000000000101")
    .bind("test.replay.v1")
    .bind("{\"value\":999}")
    .bind("2026-09-29T00:00:01Z")
    .execute(&pool)
    .await
    .expect("seed distinct same-id conflict");
    pool.close().await;
    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 50, 300, 25)
            .expect("start distinct conflict process");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("distinct conflict management surfaces");
    process
        .send_sigterm()
        .expect("stop distinct conflict process");
    assert!(process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("distinct conflict process exits")
        .success());
    let blocked = incident(&root);
    let incident_id = blocked["incident_id"].as_str().expect("incident id");
    let revision = blocked["revision"].as_u64().expect("incident revision");
    let revision_string = revision.to_string();
    let rejected = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "journal"),
            ("--evidence-ref", "ticket://ambiguous-journal-authority"),
        ],
    );
    assert!(!rejected.status.success());
    assert_eq!(
        sqlite_events(&database_url).await[0].2,
        "{\"value\":999}",
        "ambiguous journal authority mutated the database before rejection"
    );

    let first = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &revision_string),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "database"),
            (
                "--evidence-ref",
                "ticket://database-authority-disambiguation",
            ),
        ],
    );
    assert!(first.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).expect("first resolution JSON");
    assert_eq!(first["state"], "blocked");
    let second_revision = first["revision"]
        .as_u64()
        .expect("next revision")
        .to_string();
    let second = configured_accounting(
        &config,
        "resolve-conflict",
        &[
            ("--root", root.to_str().expect("root path")),
            ("--incident", incident_id),
            ("--revision", &second_revision),
            ("--event-id", "01995200-0000-7000-8000-000000000101"),
            ("--authoritative", "database"),
            (
                "--evidence-ref",
                "ticket://database-authority-disambiguation",
            ),
        ],
    );
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).expect("second resolution JSON");
    assert_eq!(second["state"], "clear");
    assert_eq!(sqlite_events(&database_url).await[0].2, "{\"value\":999}");
}

#[tokio::test]
async fn fresh_provision_and_missing_evidence_fail_closed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let relative = provision(Path::new("relative-accounting-root"));
    assert!(!relative.status.success(), "relative root was provisioned");
    let root = temp.path().join("accounting");
    let output = provision(&root);
    assert!(
        output.status.success(),
        "provision failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let installation = fs::read(root.join("installation.json")).expect("installation evidence");
    let incident: Value =
        serde_json::from_slice(&fs::read(root.join("incident.json")).expect("incident evidence"))
            .expect("valid incident evidence");
    let manifest: Value = serde_json::from_slice(
        &fs::read(root.join("provisioning.json")).expect("provisioning evidence"),
    )
    .expect("valid provisioning evidence");
    assert_eq!(manifest["format_version"], 1);
    assert_eq!(
        manifest["root"],
        fs::canonicalize(&root)
            .expect("canonical accounting root")
            .display()
            .to_string()
    );
    assert!(!installation.is_empty());
    assert_eq!(incident["state"], "clear");
    assert_eq!(incident["incident_id"], Value::Null);
    assert_eq!(incident["first_observed_at"], Value::Null);
    assert_eq!(incident["cause"], Value::Null);
    assert_eq!(incident["disposition"], Value::Null);
    assert_eq!(incident["payloads"]["pending_replay"]["volatile"], 0);
    assert_eq!(incident["payloads"]["pending_replay"]["durable"], 0);
    assert_eq!(incident["payloads"]["provisional"]["unknown"], 0);
    assert_eq!(incident["payloads"]["outcome_totals"]["reconciled"], 0);
    assert_eq!(
        incident["payloads"]["outcome_totals"]["unrecoverable_lost"],
        0
    );

    let repeated = provision(&root);
    assert!(
        !repeated.status.success(),
        "provision unexpectedly overwrote evidence"
    );
    assert_eq!(
        fs::read(root.join("installation.json")).expect("installation evidence after retry"),
        installation
    );

    let mut process = SteveProcess::start_with_accounting_root(&root).expect("start Steve");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("provisioned Steve ready");
    assert_eq!(generation_journals(&root).len(), 1);
    drop(process);

    let missing_root = temp.path().join("missing-accounting");
    let output = provision(&missing_root);
    assert!(output.status.success(), "second provision failed");
    fs::remove_file(missing_root.join("incident.json")).expect("remove incident evidence");
    let mut missing = SteveProcess::start_with_accounting_root(&missing_root).expect("spawn Steve");
    let error = missing
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("missing evidence must fail closed");
    assert!(error.contains("incident.json"), "unexpected error: {error}");

    let corrupt_root = temp.path().join("corrupt-accounting");
    assert!(provision(&corrupt_root).status.success());
    fs::write(corrupt_root.join("incident.json"), b"{}").expect("corrupt incident evidence");
    let mut corrupt = SteveProcess::start_with_accounting_root(&corrupt_root).expect("spawn Steve");
    let error = corrupt
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("corrupt evidence must fail closed");
    assert!(
        error.contains("checksum mismatch"),
        "unexpected error: {error}"
    );

    let original_root = temp.path().join("original-accounting");
    let moved_root = temp.path().join("moved-accounting");
    assert!(provision(&original_root).status.success());
    fs::rename(&original_root, &moved_root).expect("move accounting root");
    let mut moved = SteveProcess::start_with_accounting_root(&moved_root).expect("spawn Steve");
    let error = moved
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("moved evidence must fail closed");
    assert!(
        error.contains("moved since provisioning"),
        "unexpected error: {error}"
    );

    let absent_root = temp.path().join("never-provisioned");
    let mut absent = SteveProcess::start_with_accounting_root(&absent_root).expect("spawn Steve");
    let error = absent
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("ordinary startup must not provision");
    assert!(
        error.contains("not provisioned"),
        "unexpected error: {error}"
    );
    assert!(!absent_root.exists());
}

#[tokio::test]
async fn same_replica_replacement_preserves_accounting_ownership() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting");
    let output = provision(&root);
    assert!(output.status.success(), "provision failed");
    let abandoned = root.join("generations/abandoned.journal.staging");
    fs::write(&abandoned, b"").expect("empty abandoned staging");

    let mut old = SteveProcess::start_with_accounting_root(&root).expect("start old Steve");
    old.wait_ready(Duration::from_secs(10))
        .await
        .expect("old Steve ready");
    assert!(
        !abandoned.exists(),
        "empty unpublished staging was retained"
    );
    let old_journal = generation_journals(&root)
        .into_iter()
        .next()
        .expect("old generation journal");
    let old_bytes = fs::read(&old_journal).expect("old journal bytes");

    let mut replacement =
        SteveProcess::start_with_accounting_root(&root).expect("start replacement Steve");
    replacement
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("replacement ready before old exit");

    assert_eq!(generation_journals(&root).len(), 2);
    assert_eq!(
        fs::read(&old_journal).expect("old journal remains readable by test owner"),
        old_bytes,
        "replacement changed the live predecessor journal"
    );
}

#[tokio::test]
async fn legacy_journal_adoption_is_offline_and_resumable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("accounting-overflow.jsonl");
    let root = temp.path().join("accounting-root");
    let maintenance_assertion = temp.path().join("maintenance-assertion.json");
    let incomplete_assertion = temp.path().join("incomplete-assertion.json");
    let event = REPLAY_EVENT_ONE;
    fs::write(&source, event).expect("legacy source");
    fs::write(&incomplete_assertion, b"{}").expect("incomplete assertion");
    let missing = adopt(&source, &root, &maintenance_assertion);
    assert!(!missing.status.success(), "missing assertion was accepted");
    let incomplete = adopt(&source, &root, &incomplete_assertion);
    assert!(
        !incomplete.status.success(),
        "incomplete assertion was accepted"
    );
    assert!(
        !root.exists(),
        "invalid assertion created destination state"
    );

    fs::write(
        &maintenance_assertion,
        serde_json::to_vec_pretty(&serde_json::json!({
            "workload_identity": "legacy-steve",
            "host": "fixture-host",
            "source": fs::canonicalize(&source).expect("canonical source"),
            "stopped": true,
            "restart_disabled": true,
            "observed_at": "2026-09-29T00:00:00Z",
            "command_or_exported_status": "fixture supervisor stopped and disabled legacy-steve"
        }))
        .expect("serialize assertion"),
    )
    .expect("maintenance assertion");

    let locked = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .expect("open legacy source");
    locked
        .lock()
        .expect("cooperating legacy writer holds source lock");
    let busy = adopt(&source, &root, &maintenance_assertion);
    File::unlock(&locked).expect("cooperating legacy writer releases source lock");
    assert!(
        !busy.status.success(),
        "cooperating live writer was adopted"
    );
    assert!(
        !root.exists(),
        "failed offline check created destination state"
    );

    let imported = adopt(&source, &root, &maintenance_assertion);
    assert!(
        imported.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert!(String::from_utf8_lossy(&imported.stdout).contains("pending reconciliation"));
    assert_eq!(fs::read(&source).expect("retained source"), event);

    let adoption: Value =
        serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("adoption evidence"))
            .expect("valid adoption evidence");
    assert_eq!(adoption["state"], "pending_reconciliation");
    assert_eq!(
        adoption["maintenance_assertion"]["assertion_kind"],
        "operator_supplied_unauthenticated"
    );
    assert_eq!(adoption["maintenance_assertion"]["stopped"], true);
    assert_eq!(adoption["maintenance_assertion"]["restart_disabled"], true);
    let backup = PathBuf::from(adoption["backup"].as_str().expect("backup path"));
    assert_eq!(fs::read(&backup).expect("durable backup"), event);
    let journals = generation_journals(&root);
    assert_eq!(journals.len(), 1);
    assert_eq!(fs::read(&journals[0]).expect("imported journal"), event);

    let resumed = adopt(&source, &root, &maintenance_assertion);
    assert!(
        resumed.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(
        generation_journals(&root),
        journals,
        "resume duplicated import"
    );

    let mut process = SteveProcess::start_with_accounting_root(&root).expect("spawn Steve");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("pending adoption replays before process readiness");

    let database_url = format!("sqlite://{}", process.database_path().display());
    assert_eq!(
        sqlite_events(&database_url).await,
        vec![(
            "01995200-0000-7000-8000-000000000101".into(),
            "test.replay.v1".into(),
            "{\"value\":1}".into(),
            "2026-09-29T00:00:01Z".into(),
        )]
    );

    let completed: Value =
        serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("adoption evidence"))
            .expect("valid adoption evidence");
    assert_eq!(completed["state"], "complete");
    assert_eq!(completed["record_outcome"], "reconciled");
    assert!(!source.exists(), "reconciled source remained active");
    let retained = PathBuf::from(
        completed["retained_source"]
            .as_str()
            .expect("retained source path"),
    );
    assert_eq!(fs::read(retained).expect("retained moved source"), event);
    retained_artifacts_match(&root, &adoption, event);

    let manifest = replay_evidence_text(&root, &adoption);
    assert!(manifest.contains("inserted"));
    assert!(manifest.contains("01995200-0000-7000-8000-000000000101"));
    assert!(
        root.join("generations")
            .join(format!("{}.replay.json.sha256", generation_id(&adoption)))
            .is_file(),
        "replay manifest checksum missing"
    );
}

#[tokio::test]
async fn completed_adoption_missing_backup_fails_startup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("missing-backup.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let (completed, _live) = complete_adoption_fixture(&source, &root, &assertion).await;
    fs::remove_file(PathBuf::from(
        completed["backup"].as_str().expect("backup path"),
    ))
    .expect("remove completed adoption backup");

    let mut restarted = SteveProcess::start_with_accounting_root(&root).expect("restart Steve");
    let error = restarted
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("missing completed-adoption backup must fail startup");
    assert!(
        error.contains("completed adoption backup does not match evidence"),
        "unexpected startup error: {error}"
    );
}

#[tokio::test]
async fn completed_adoption_tampered_retained_source_fails_startup() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("tampered-retained.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let (completed, _live) = complete_adoption_fixture(&source, &root, &assertion).await;
    fs::write(
        PathBuf::from(
            completed["retained_source"]
                .as_str()
                .expect("retained source path"),
        ),
        b"tampered\n",
    )
    .expect("tamper completed adoption retained source");

    let mut restarted = SteveProcess::start_with_accounting_root(&root).expect("restart Steve");
    let error = restarted
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("tampered completed-adoption retained source must fail startup");
    assert!(
        error.contains("completed adoption retained source does not match evidence"),
        "unexpected startup error: {error}"
    );
}

#[tokio::test]
async fn empty_legacy_journal_adoption_completes_vacuously() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("empty-overflow.jsonl");
    let root = temp.path().join("accounting-root");
    let maintenance_assertion = temp.path().join("maintenance-assertion.json");
    fs::write(&source, b"").expect("empty source");
    let canonical_source = fs::canonicalize(&source).expect("canonical source");
    fs::write(
        &maintenance_assertion,
        serde_json::to_vec_pretty(&serde_json::json!({
            "workload_identity": "legacy-steve",
            "host": "fixture-host",
            "source": canonical_source,
            "stopped": true,
            "restart_disabled": true,
            "observed_at": "2026-09-29T00:00:00Z",
            "command_or_exported_status": "fixture supervisor stopped and disabled legacy-steve"
        }))
        .expect("serialize assertion"),
    )
    .expect("maintenance assertion");

    let adopted = adopt(&source, &root, &maintenance_assertion);
    assert!(
        adopted.status.success(),
        "empty adoption failed: {}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    assert!(!source.exists(), "empty source was not retained by move");
    let adoption: Value =
        serde_json::from_slice(&fs::read(root.join("adoption.json")).expect("adoption evidence"))
            .expect("valid adoption evidence");
    assert_eq!(adoption["state"], "complete");
    assert_eq!(adoption["record_outcome"], "empty_vacuous");
    assert_eq!(adoption["source_directory_synced"], true);
    let retained = PathBuf::from(
        adoption["retained_source"]
            .as_str()
            .expect("retained source path"),
    );
    assert_eq!(fs::read(retained).expect("retained empty source"), b"");
    let repeated = adopt(&source, &root, &maintenance_assertion);
    assert!(
        repeated.status.success(),
        "empty adoption retry failed: {}",
        String::from_utf8_lossy(&repeated.stderr)
    );

    let mut process = SteveProcess::start_with_accounting_root(&root).expect("start Steve");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("empty adoption permits startup");
}

#[tokio::test]
async fn journal_partial_tail_recovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("partial-tail.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let database_path = temp.path().join("partial-tail.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let mut bytes = [REPLAY_EVENT_ONE, REPLAY_EVENT_TWO].concat();
    let complete_boundary = bytes.len();
    bytes.extend_from_slice(TORN_TAIL);
    let adoption = adopt_fixture(&source, &root, &assertion, &bytes);

    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 200, 2_000, 50)
            .expect("start partial-tail Steve");
    process
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("torn tail must keep startup fail closed");

    assert_eq!(
        sqlite_events(&database_url).await,
        vec![
            (
                "01995200-0000-7000-8000-000000000101".into(),
                "test.replay.v1".into(),
                "{\"value\":1}".into(),
                "2026-09-29T00:00:01Z".into(),
            ),
            (
                "01995200-0000-7000-8000-000000000102".into(),
                "test.replay.v1".into(),
                "{\"value\":2}".into(),
                "2026-09-29T00:00:02Z".into(),
            ),
        ],
        "complete frames before the torn tail were not replayed exactly once"
    );
    assert_eq!(fs::read(&source).expect("retained source"), bytes);
    retained_artifacts_match(&root, &adoption, &bytes);
    let replay = replay_evidence(&root, &adoption);
    assert_eq!(replay["complete_boundary"], complete_boundary as u64);
    assert_eq!(replay["torn_tail_length"], TORN_TAIL.len() as u64);
    assert_eq!(replay["torn_tail_digest"], sha256(TORN_TAIL));
    let replay_text = serde_json::to_string(&replay).expect("serialize replay evidence");
    assert!(replay_text.contains("01995200-0000-7000-8000-000000000101"));
    assert!(replay_text.contains("01995200-0000-7000-8000-000000000102"));
    assert_eq!(
        serde_json::from_slice::<Value>(
            &fs::read(root.join("adoption.json")).expect("pending adoption evidence")
        )
        .expect("valid pending adoption evidence")["state"],
        "pending_reconciliation"
    );

    let mut restarted =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 200, 2_000, 50)
            .expect("restart partial-tail Steve");
    restarted
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("retained torn tail must remain fail closed after restart");
    assert_eq!(sqlite_events(&database_url).await.len(), 2);
}

#[tokio::test]
async fn accounting_reconciles_after_db_recovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("accounting-root");
    let database_path = temp.path().join("recovery.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    assert!(
        provision(&root).status.success(),
        "provision accounting root"
    );
    let pool = prepare_sqlite(&database_url).await;

    let operation_timeout = Duration::from_millis(100);
    let retry_deadline = Duration::from_millis(600);
    let retry_interval = 50;
    let mut process = SteveProcess::start_with_database_and_accounting(
        &database_url,
        &root,
        operation_timeout.as_millis() as u64,
        retry_deadline.as_millis() as u64,
        retry_interval,
    )
    .expect("start recovery Steve");
    let listeners = process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("recovery Steve ready");
    let journal = generation_journals(&root)
        .into_iter()
        .next()
        .expect("active generation journal");
    assert_eq!(fs::read(&journal).expect("empty active journal"), b"");

    let client = reqwest::Client::new();
    let mut lock = pool
        .acquire()
        .await
        .expect("acquire SQLite lock connection");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .expect("hold SQLite write lock");
    let first_payload = json!({"value": {"run": "retry-before-deadline"}});
    let first_started = Instant::now();
    let first_reply: Value = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&first_payload)
        .send()
        .await
        .expect("send first echo")
        .error_for_status()
        .expect("first echo status")
        .json()
        .await
        .expect("parse first echo");
    assert_eq!(first_reply, first_payload);
    tokio::time::sleep(operation_timeout + Duration::from_millis(100)).await;
    assert!(
        first_started.elapsed() < retry_deadline,
        "test did not release the database before the retry deadline"
    );
    assert_eq!(
        fs::read(&journal).expect("journal before live recovery"),
        b"",
        "event spilled before the configured retry deadline"
    );
    sqlx::query("COMMIT")
        .execute(&mut *lock)
        .await
        .expect("release SQLite write lock");
    let first_rows = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = sqlite_events(&database_url).await;
            if rows
                .iter()
                .any(|row| row.2.contains("retry-before-deadline"))
            {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("first event was not inserted after live database recovery");
    assert_eq!(first_rows.len(), 1);
    assert_eq!(first_rows[0].1, "test.echo");
    assert_eq!(first_rows[0].2, first_payload.to_string());
    assert_eq!(
        fs::read(&journal).expect("journal after live recovery"),
        b""
    );

    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *lock)
        .await
        .expect("hold SQLite write lock through retry deadline");
    let second_payload = json!({"value": {"run": "spill-after-deadline"}});
    let started = Instant::now();
    let second_reply: Value = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
        .json(&second_payload)
        .send()
        .await
        .expect("send second echo")
        .error_for_status()
        .expect("second echo status")
        .json()
        .await
        .expect("parse second echo");
    assert_eq!(second_reply, second_payload);

    let earliest_spill = retry_deadline - Duration::from_millis(100);
    while started.elapsed() < earliest_spill {
        assert_eq!(
            fs::read(&journal).expect("journal before retry deadline"),
            b"",
            "event spilled before the configured retry deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let journal_bytes = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let bytes = fs::read(&journal).expect("read active journal");
            if !bytes.is_empty() {
                break bytes;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("event did not spill after the configured retry deadline");
    assert!(
        started.elapsed() >= earliest_spill,
        "durable spill was observed before the retry deadline tolerance"
    );
    let lines = journal_bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "journal must contain exactly one event");
    let journal_event: Value = serde_json::from_slice(lines[0]).expect("valid journal event");
    assert_eq!(journal_event["kind"], "test.echo");
    assert_eq!(journal_event["payload"], second_payload);

    let generation_id = journal
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("generation id")
        .to_owned();
    let state_path = root
        .join("generations")
        .join(format!("{generation_id}.state.json"));
    let coordination_path = root.join("coordination.json");
    let active_state = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let bytes = fs::read(&state_path).expect("active generation state");
            let state: Value =
                serde_json::from_slice(&bytes).expect("valid active generation state");
            let checksum = fs::read(format!("{}.sha256", state_path.display()))
                .expect("active state checksum");
            let coordination_bytes =
                fs::read(&coordination_path).expect("active generation coordination");
            let coordination: Value = serde_json::from_slice(&coordination_bytes)
                .expect("valid active generation coordination");
            let coordination_checksum = fs::read(format!("{}.sha256", coordination_path.display()))
                .expect("active coordination checksum");
            let coverage_matches = coordination["coverage"]
                .as_array()
                .expect("coordination coverage")
                .iter()
                .any(|entry| {
                    entry["generation_id"] == generation_id
                        && entry["generation_state_revision"] == state["revision"]
                        && entry["journal_evidence_digest"] == state["journal_evidence_digest"]
                });
            if state["journal_length"] == journal_bytes.len() as u64
                && state["journal_evidence_digest"] == sha256(&journal_bytes)
                && checksum == sha256(&bytes).as_bytes()
                && coordination_checksum == sha256(&coordination_bytes).as_bytes()
                && coverage_matches
            {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("journal spill did not publish checked generation state");
    assert_eq!(active_state["phase"], "active");
    assert_eq!(active_state["journal_length"], journal_bytes.len() as u64);

    sqlx::query("COMMIT")
        .execute(&mut *lock)
        .await
        .expect("release SQLite write lock before shutdown reconciliation");
    drop(lock);
    process.send_sigterm().expect("stop recovery Steve");
    let status = process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("recovery Steve exits");
    assert!(status.success(), "recovery Steve exited with {status}");

    let mut restarted = SteveProcess::start_with_database_and_accounting(
        &database_url,
        &root,
        operation_timeout.as_millis() as u64,
        retry_deadline.as_millis() as u64,
        retry_interval,
    )
    .expect("restart recovery Steve");
    let restarted_listeners = restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("gracefully drained generation replays on restart");
    assert_eq!(
        reqwest::get(format!(
            "http://{}/health/ready",
            restarted_listeners.management
        ))
        .await
        .expect("restart readiness")
        .status(),
        reqwest::StatusCode::OK
    );

    assert_eq!(
        fs::read(&journal).expect("retained prior journal"),
        journal_bytes
    );
    let retained_state = fs::read(&state_path).expect("retained generation state");
    let retained_state_value: Value =
        serde_json::from_slice(&retained_state).expect("valid retained generation state");
    assert_eq!(retained_state_value["phase"], "reconciled");
    assert_eq!(
        retained_state_value["journal_evidence_digest"],
        sha256(&journal_bytes)
    );
    assert_eq!(
        fs::read(format!("{}.sha256", state_path.display())).expect("state checksum"),
        sha256(&retained_state).as_bytes()
    );
    assert!(retained_state_value["replay_manifest"].is_string());
    let second_payload = second_payload.to_string();
    assert!(sqlite_events(&database_url)
        .await
        .iter()
        .any(|event| event.1 == "test.echo" && event.2 == second_payload));
    pool.close().await;
}

#[tokio::test]
async fn journal_partial_commit_replay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("partial-commit.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let database_path = temp.path().join("partial-commit.db");
    let database_url = format!("sqlite://{}?mode=rwc", database_path.display());
    let bytes = [REPLAY_EVENT_ONE, REPLAY_EVENT_TWO].concat();
    let adoption = adopt_fixture(&source, &root, &assertion, &bytes);
    let pool = prepare_sqlite(&database_url).await;
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind("01995200-0000-7000-8000-000000000101")
    .bind("test.replay.v1")
    .bind("{\"value\":1}")
    .bind("2026-09-29T00:00:01Z")
    .execute(&pool)
    .await
    .expect("seed previously committed event");
    pool.close().await;

    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 200, 2_000, 50)
            .expect("start partial-commit Steve");
    process
        .wait_ready(Duration::from_secs(10))
        .await
        .expect("partial commit replay reconciles");

    let rows = sqlite_events(&database_url).await;
    assert_eq!(rows.len(), 2, "partial replay inserted a duplicate row");
    assert_eq!(rows[0].0, "01995200-0000-7000-8000-000000000101");
    assert_eq!(rows[1].0, "01995200-0000-7000-8000-000000000102");
    let replay = replay_evidence_text(&root, &adoption);
    assert!(replay.contains("duplicate_identical"));
    assert!(replay.contains("inserted"));
    retained_artifacts_match(&root, &adoption, &bytes);
    let completed: Value = serde_json::from_slice(
        &fs::read(root.join("adoption.json")).expect("completed adoption evidence"),
    )
    .expect("valid completed adoption evidence");
    assert_eq!(completed["state"], "complete");
    assert!(!source.exists(), "reconciled source remained active");
    let retained = PathBuf::from(
        completed["retained_source"]
            .as_str()
            .expect("retained source path"),
    );
    assert_eq!(fs::read(retained).expect("retained moved source"), bytes);
}

#[tokio::test]
async fn postgres_replay_detects_conflicting_duplicate() {
    let Some(database_url) = std::env::var_os("STEVE_TEST_POSTGRES_URL") else {
        eprintln!("STEVE_TEST_POSTGRES_URL is not set; PostgreSQL replay case not requested");
        return;
    };
    let database_url = database_url
        .into_string()
        .expect("STEVE_TEST_POSTGRES_URL must be valid UTF-8");
    let pool = tokio::time::timeout(
        Duration::from_secs(2),
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url),
    )
    .await
    .expect("timed out connecting to configured Task3 PostgreSQL database")
    .expect("connect to configured Task3 PostgreSQL database");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS steve_background_events (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .expect("create PostgreSQL event table");
    sqlx::query("DELETE FROM steve_background_events WHERE id = $1")
        .bind("01995200-0000-7000-8000-000000000101")
        .execute(&pool)
        .await
        .expect("clear prior conflict fixture");
    sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind("01995200-0000-7000-8000-000000000101")
    .bind("test.replay.v1")
    .bind("{\"value\":999}")
    .bind("2026-09-29T00:00:01Z")
    .execute(&pool)
    .await
    .expect("seed conflicting PostgreSQL event");

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("postgres-conflict.jsonl");
    let root = temp.path().join("accounting-root");
    let assertion = temp.path().join("maintenance-assertion.json");
    let adoption = adopt_fixture(&source, &root, &assertion, REPLAY_EVENT_ONE);
    let mut process =
        SteveProcess::start_with_database_and_accounting(&database_url, &root, 200, 1_000, 50)
            .expect("start PostgreSQL conflict Steve");
    let listeners = process
        .wait_ready(Duration::from_secs(5))
        .await
        .expect("conflicting duplicate keeps management available");
    let client = reqwest::Client::new();
    let conflict_incident = incident(&root);
    assert_eq!(conflict_incident["state"], "blocked");
    assert_eq!(conflict_incident["cause"], "replay_content_conflict");
    let ready = client
        .get(format!("http://{}/health/ready", listeners.management))
        .send()
        .await
        .expect("conflict readiness response");
    assert_eq!(ready.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let ready: Value = ready.json().await.expect("conflict readiness JSON");
    assert_eq!(ready["status"], "not_ready");
    assert_eq!(ready["accounting_incident"]["state"], "unreconciled");
    assert_eq!(
        ready["accounting_incident"]["cause"],
        "prior_incident_unreconciled"
    );
    assert_eq!(
        client
            .get(format!("http://{}/health/live", listeners.management))
            .send()
            .await
            .expect("conflict liveness response")
            .status(),
        reqwest::StatusCode::OK
    );

    let stored: (String, String, String, String) = sqlx::query_as(
        "SELECT id, kind, payload, created_at
         FROM steve_background_events WHERE id = $1",
    )
    .bind("01995200-0000-7000-8000-000000000101")
    .fetch_one(&pool)
    .await
    .expect("read retained conflicting PostgreSQL row");
    assert_eq!(stored.1, "test.replay.v1");
    assert_eq!(stored.2, "{\"value\":999}");
    assert_eq!(stored.3, "2026-09-29T00:00:01Z");
    assert_eq!(
        fs::read(&source).expect("retained conflict source"),
        REPLAY_EVENT_ONE
    );
    retained_artifacts_match(&root, &adoption, REPLAY_EVENT_ONE);
    let replay = replay_evidence_text(&root, &adoption);
    assert!(replay.contains("duplicate_conflict"));
    assert!(replay.contains("01995200-0000-7000-8000-000000000101"));
    let conflicts = fs::read_dir(root.join("generations"))
        .expect("read conflict evidence directory")
        .map(|entry| entry.expect("conflict evidence entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".conflict.") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 1, "protected conflict snapshot missing");
    let conflict: Value =
        serde_json::from_slice(&fs::read(&conflicts[0]).expect("protected conflict snapshot"))
            .expect("valid protected conflict snapshot");
    let conflict_text = serde_json::to_string(&conflict).expect("serialize conflict snapshot");
    assert!(conflict_text.contains("{\\\"value\\\":1}"));
    assert!(conflict_text.contains("{\\\"value\\\":999}"));
    let pending: Value = serde_json::from_slice(
        &fs::read(root.join("adoption.json")).expect("pending conflict adoption"),
    )
    .expect("valid pending conflict adoption");
    assert_eq!(pending["state"], "pending_reconciliation");

    sqlx::query("DELETE FROM steve_background_events WHERE id = $1")
        .bind("01995200-0000-7000-8000-000000000101")
        .execute(&pool)
        .await
        .expect("clean PostgreSQL conflict fixture");
    pool.close().await;
}
