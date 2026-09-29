#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;

use process::{acquire_accounting_startup_lock, steve_command, SteveProcess};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, sqlite::SqlitePoolOptions};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const REPLAY_EVENT_ONE: &[u8] = b"{\"id\":\"01995200-0000-7000-8000-000000000101\",\"kind\":\"test.replay.v1\",\"payload\":{\"value\":1},\"created_at\":\"2026-09-29T00:00:01Z\"}\n";
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

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

    process.send_sigterm().expect("stop recovery Steve");
    let status = process
        .wait_for_exit(Duration::from_secs(5))
        .await
        .expect("recovery Steve exits");
    assert!(status.success(), "recovery Steve exited with {status}");
    sqlx::query("COMMIT")
        .execute(&mut *lock)
        .await
        .expect("release SQLite write lock after shutdown");
    drop(lock);

    let mut restarted = SteveProcess::start_with_database_and_accounting(
        &database_url,
        &root,
        operation_timeout.as_millis() as u64,
        retry_deadline.as_millis() as u64,
        retry_interval,
    )
    .expect("restart recovery Steve");
    let restart_error = restarted
        .wait_ready(Duration::from_secs(10))
        .await
        .expect_err("unowned active generation must remain fail closed");
    assert!(
        restart_error.contains("prior active accounting generation"),
        "unexpected restart error: {restart_error}"
    );

    assert_eq!(
        fs::read(&journal).expect("retained prior journal"),
        journal_bytes
    );
    let retained_state = fs::read(&state_path).expect("retained generation state");
    let retained_state_value: Value =
        serde_json::from_slice(&retained_state).expect("valid retained generation state");
    assert_eq!(retained_state_value["phase"], "active");
    assert_eq!(
        retained_state_value["journal_evidence_digest"],
        sha256(&journal_bytes)
    );
    assert_eq!(
        fs::read(format!("{}.sha256", state_path.display())).expect("state checksum"),
        sha256(&retained_state).as_bytes()
    );
    assert!(retained_state_value["replay_manifest"].is_null());
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
    process
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("conflicting duplicate must prevent readiness");

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
