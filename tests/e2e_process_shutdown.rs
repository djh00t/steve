#![cfg(unix)]

#[allow(dead_code)]
#[path = "support/process.rs"]
mod process;

use process::SteveProcess;
use std::time::Duration;

#[tokio::test]
async fn process_sigterm_exits_cleanly() {
    let mut steve = SteveProcess::start_with_drain_timeout(None, None, 5)
        .expect("start Steve process with a five-second drain timeout");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .expect("Steve listeners become ready");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build HTTP client");
    let live = client
        .get(format!("http://{}/health/live", listeners.management))
        .send()
        .await
        .expect("request liveness");
    assert_eq!(live.status(), reqwest::StatusCode::OK);

    steve.send_sigterm().expect("send SIGTERM to Steve");
    let status = steve
        .wait_for_exit(Duration::from_secs(10))
        .await
        .expect("Steve exits within the caller's timeout");
    assert!(status.success(), "Steve exited unsuccessfully: {status}");
}
