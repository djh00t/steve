#![allow(dead_code)]

#[path = "support/object_store.rs"]
mod object_store;
#[path = "support/process.rs"]
mod process;

use object_store::HeldObjectStoreEndpoint;
use process::SteveProcess;
use serde_json::{json, Value};
use std::time::Duration;

#[tokio::test]
async fn held_s3_put_controls() {
    let mut endpoint = HeldObjectStoreEndpoint::start()
        .await
        .expect("start held object-store endpoint");
    let mut steve = SteveProcess::start_with_object_store(&endpoint.url(), 16)
        .expect("start Steve with S3 object storage");
    let listeners = steve
        .wait_ready(Duration::from_secs(15))
        .await
        .unwrap_or_else(|err| panic!("{err}"));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build HTTP client");
    let run = uuid::Uuid::now_v7().to_string();
    let request = json!({"value": {"run": run}});
    let reply: Value = client
        .post(format!("http://{}/api/v1/test/echo", listeners.inference))
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

    let put = endpoint
        .wait_for_put(Duration::from_secs(5))
        .await
        .expect("echo history PutObject");
    assert_eq!(put.method, reqwest::Method::PUT);
    let key = put
        .path
        .strip_prefix("/steve/test/")
        .and_then(|key| key.strip_suffix(".json"))
        .filter(|key| !key.is_empty() && !key.contains('/'))
        .expect("PutObject path contains exactly one test key segment");
    uuid::Uuid::parse_str(key).expect("test object key is a UUID");
    assert_eq!(
        put.body.as_ref(),
        format!("{{\"value\":{{\"run\":\"{run}\"}}}}").as_bytes()
    );
    assert_eq!(
        endpoint
            .wait_for_completion(Duration::from_millis(100))
            .await,
        Err("timed out waiting for PutObject completion".to_owned()),
        "PutObject must remain pending until the test releases it"
    );

    endpoint.release().expect("release PutObject");
    endpoint
        .wait_for_completion(Duration::from_secs(5))
        .await
        .expect("PutObject completed successfully");
    assert_eq!(endpoint.request_count(), 1, "expected one S3 request");
    endpoint
        .shutdown()
        .await
        .expect("stop object-store listener");
}
