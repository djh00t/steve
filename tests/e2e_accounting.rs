#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;

use process::{acquire_accounting_startup_lock, steve_command, SteveProcess};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

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
    let event = b"{\"id\":\"0199-test\",\"kind\":\"test\",\"payload\":{\"value\":42},\"created_at\":\"2026-09-29T00:00:00Z\"}\n";
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
    let error = process
        .wait_ready(Duration::from_secs(5))
        .await
        .expect_err("pending adoption must fail closed");
    assert!(
        error.contains("pending reconciliation"),
        "unexpected error: {error}"
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
