#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;

use process::{steve_command, SteveProcess};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

fn run_accounting(args: &[&str], paths: &[&Path]) -> std::process::Output {
    let mut command = steve_command();
    command.arg("accounting");
    for arg in args {
        command.arg(arg);
    }
    for path in paths {
        command.arg(path);
    }
    command.output().expect("run Steve accounting command")
}

fn provision(root: &Path) -> std::process::Output {
    run_accounting(&["provision", "--root"], &[root])
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
    let incident = fs::read(root.join("incident.json")).expect("incident evidence");
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
    assert!(!incident.is_empty());

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
    let event = b"{\"id\":\"0199-test\",\"kind\":\"test\",\"payload\":{\"value\":42},\"created_at\":\"2026-09-29T00:00:00Z\"}\n";
    fs::write(&source, event).expect("legacy source");

    let locked = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .expect("open legacy source");
    locked.lock().expect("lock legacy source");
    let busy = run_accounting(
        &["adopt-legacy", "--source"],
        &[&source, Path::new("--root"), &root],
    );
    File::unlock(&locked).expect("unlock legacy source");
    assert!(!busy.status.success(), "live legacy source was adopted");
    assert!(
        !root.exists(),
        "failed offline check created destination state"
    );

    let imported = run_accounting(
        &["adopt-legacy", "--source"],
        &[&source, Path::new("--root"), &root],
    );
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
    let backup = PathBuf::from(adoption["backup"].as_str().expect("backup path"));
    assert_eq!(fs::read(&backup).expect("durable backup"), event);
    let journals = generation_journals(&root);
    assert_eq!(journals.len(), 1);
    assert_eq!(fs::read(&journals[0]).expect("imported journal"), event);

    let resumed = run_accounting(
        &["adopt-legacy", "--source"],
        &[&source, Path::new("--root"), &root],
    );
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
