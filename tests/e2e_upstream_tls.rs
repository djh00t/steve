#![allow(dead_code)]

#[path = "support/process.rs"]
mod process;
#[path = "support/upstream_tls.rs"]
mod upstream_tls;

use axum::{http::StatusCode, routing::get, Router};
use process::SteveProcess;
use serde_json::Value;
use std::{fs, path::Path, time::Duration};
use upstream_tls::TlsFixture;

const READY_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test]
async fn stv_prov_38_shared_trust_acceptance() {
    let fixture = TlsFixture::start(
        Router::new()
            .route(
                "/v1/chat/completions",
                get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            )
            .route(
                "/v1/messages",
                get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
            ),
    )
    .await
    .expect("start HTTPS provider fixture");
    let ca = Path::new("tests/fixtures/tls/ca.pem");
    let url = fixture.url();
    let mut steve = SteveProcess::start_with_upstream_ca_bundle(Some(&url), Some(&url), Some(ca))
        .expect("start Steve with CA bundle");
    let listeners = steve
        .wait_ready(READY_TIMEOUT)
        .await
        .unwrap_or_else(|error| panic!("Steve did not become ready: {error}"));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("management client")
        .get(format!(
            "http://{}/api/v1/providers/health",
            listeners.management
        ))
        .send()
        .await
        .expect("provider health request");
    let status = response.status();
    let body: Value = response.json().await.expect("provider health JSON");
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["providers"]["openai"]["status"], "healthy");
    assert_eq!(body["providers"]["anthropic"]["status"], "healthy");
    drop(steve);

    let mut without_ca =
        SteveProcess::start_with_upstream_urls(Some(&url), Some(&url)).expect("start without CA");
    let listeners = without_ca
        .wait_ready(READY_TIMEOUT)
        .await
        .unwrap_or_else(|error| panic!("Steve without CA did not become ready: {error}"));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("management client")
        .get(format!(
            "http://{}/api/v1/providers/health",
            listeners.management
        ))
        .send()
        .await
        .expect("provider health without CA");
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("provider health JSON");
    assert_eq!(body["providers"]["openai"]["status"], "unhealthy");
    assert_eq!(body["providers"]["anthropic"]["status"], "unhealthy");
    drop(without_ca);
    fixture.shutdown().await.expect("HTTPS fixture shutdown");
}

#[tokio::test]
async fn invalid_ca_bundles_fail_before_readiness() {
    let temp = tempfile::tempdir().expect("CA temp directory");
    let missing = temp.path().join("missing.pem");
    let empty = temp.path().join("empty.pem");
    let malformed = temp.path().join("malformed.pem");
    let invalid_der = temp.path().join("invalid-der.pem");
    fs::write(&empty, b"").expect("write empty bundle");
    fs::write(
        &malformed,
        b"-----BEGIN CERTIFICATE-----\n@@@\n-----END CERTIFICATE-----\n",
    )
    .expect("write malformed bundle");
    fs::write(
        &invalid_der,
        b"-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n",
    )
    .expect("write invalid DER bundle");

    for (path, kind) in [
        (missing.as_path(), "reading"),
        (empty.as_path(), "empty"),
        (malformed.as_path(), "parsing"),
        (invalid_der.as_path(), "building"),
    ] {
        assert_ca_rejected(path, kind).await;
    }
}

async fn assert_ca_rejected(path: &Path, kind: &str) {
    let mut steve = SteveProcess::start_with_upstream_ca_bundle(None, None, Some(path))
        .expect("start Steve with invalid CA bundle");
    let error = match steve.wait_ready(READY_TIMEOUT).await {
        Ok(_) => panic!(
            "Steve became ready with invalid CA bundle: {}",
            path.display()
        ),
        Err(error) => error,
    };
    assert!(error.contains("Steve exited"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
    assert!(error.contains(kind), "{error}");
    assert!(!error.contains("@@@"), "CA contents leaked: {error}");
}
