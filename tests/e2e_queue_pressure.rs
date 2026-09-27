#![allow(dead_code)]

#[path = "support/object_store.rs"]
mod object_store;
#[path = "support/process.rs"]
mod process;

use object_store::HeldObjectStoreEndpoint;
use process::SteveProcess;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

const RESPONSE_DEADLINE: Duration = Duration::from_secs(3);

#[tokio::test]
async fn history_pressure_keeps_management_responsive() {
    let mut endpoint = HeldObjectStoreEndpoint::start()
        .await
        .expect("start held object-store endpoint");
    let mut steve = SteveProcess::start_with_object_store(&endpoint.url(), 1)
        .expect("start Steve with a capacity-one history queue");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build HTTP client");
    let inference = format!("http://{}", listeners.inference);
    let management = format!("http://{}", listeners.management);

    let first = echo(&client, &inference).await;
    let held_put = endpoint
        .wait_for_put(Duration::from_secs(5))
        .await
        .expect("first echo reached the held PutObject");
    assert_eq!(held_put.method, reqwest::Method::PUT);

    // The history worker is blocked on this first write. The second echo then
    // occupies the only queued slot, so the third echo exercises overflow.
    let fixture_pending = endpoint
        .wait_for_completion(Duration::from_millis(100))
        .await;
    let second = echo(&client, &inference).await;
    let third = tokio::time::timeout(RESPONSE_DEADLINE, echo(&client, &inference))
        .await
        .map_err(|_| "third echo did not respond before releasing the held PutObject".to_owned())
        .and_then(|result| result);

    // Probe all management routes concurrently while the object-store request
    // is still blocked, and keep each complete response (including its body)
    // inside a bounded deadline.
    let (live, version, status) = tokio::join!(
        get_json(&client, format!("{management}/health/live")),
        get_json(&client, format!("{management}/api/v1/system/version")),
        get_json(&client, format!("{management}/api/v1/system/status")),
    );

    let release = endpoint.release();
    let completion = endpoint.wait_for_completion(Duration::from_secs(5)).await;
    let shutdown = endpoint.shutdown().await;

    assert_eq!(
        fixture_pending,
        Err("timed out waiting for PutObject completion".to_owned()),
        "fixture must confirm the first PutObject is pending before release"
    );
    release.expect("release the held PutObject");
    completion.expect("held PutObject completed successfully");
    shutdown.expect("stop object-store listener");

    let request = first.expect("first echo response");
    assert!(request["value"]["run"].as_str().is_some());
    second.expect("second echo response");
    third.expect("third echo must respond before releasing the held PutObject");

    assert_eq!(live.expect("management live response")["status"], "ok");
    assert_eq!(
        version.expect("management version response")["name"],
        "steve"
    );
    let status = status.expect("management status response");
    assert_eq!(status["queues"]["history_dropped"], 1);
}

async fn echo(client: &Client, inference: &str) -> Result<Value, String> {
    let request = json!({"value": {"run": uuid::Uuid::now_v7().to_string()}});
    client
        .post(format!("{inference}/api/v1/test/echo"))
        .json(&request)
        .send()
        .await
        .map_err(|err| format!("request echo: {err}"))?
        .error_for_status()
        .map_err(|err| format!("echo status: {err}"))?
        .json()
        .await
        .map_err(|err| format!("parse echo response: {err}"))
}

async fn get_json(client: &Client, url: String) -> Result<Value, String> {
    tokio::time::timeout(RESPONSE_DEADLINE, async {
        client
            .get(url)
            .send()
            .await
            .map_err(|err| format!("request management endpoint: {err}"))?
            .error_for_status()
            .map_err(|err| format!("management status: {err}"))?
            .json()
            .await
            .map_err(|err| format!("parse management response: {err}"))
    })
    .await
    .map_err(|_| "timed out waiting for management response".to_owned())?
}
