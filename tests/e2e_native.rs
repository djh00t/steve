#[allow(dead_code)]
#[path = "support/process.rs"]
mod process;

use axum::{
    body::Body,
    extract::{Request, State},
    response::IntoResponse,
    routing::post,
    Router,
};
use serde_json::{json, Value};
use std::{
    fs,
    process::{Child, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

struct NativeProcess {
    child: Child,
    log: std::path::PathBuf,
    management: String,
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl NativeProcess {
    fn start(root: &std::path::Path, credential: Option<&str>) -> Self {
        let log = root.join("run.log");
        let output = fs::File::create(&log).unwrap();
        let mut command = process::steve_command();
        command
            .arg("--config")
            .arg(root.join("config.toml"))
            .arg("serve")
            .env_remove("STEVE_NATIVE_E2E_KEY")
            .env_remove("STEVE_NATIVE_DISABLED_KEY")
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output));
        if let Some(value) = credential {
            command.env("STEVE_NATIVE_E2E_KEY", value);
        }
        Self {
            child: command.spawn().unwrap(),
            log,
            management: String::new(),
        }
    }
    async fn ready(&mut self) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let logs = fs::read_to_string(&self.log).unwrap();
            for line in logs.lines() {
                if let Ok(event) = serde_json::from_str::<Value>(line) {
                    if event["fields"]["event"] == "listeners_ready" {
                        self.management =
                            format!("http://{}", event["fields"]["management"].as_str().unwrap());
                        return format!(
                            "http://{}",
                            event["fields"]["inference"].as_str().unwrap()
                        );
                    }
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "startup failed: {logs}"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "startup timed out: {logs}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

type Recorded = Arc<Mutex<Vec<(String, axum::http::HeaderMap, Value)>>>;
async fn upstream(State(recorded): State<Recorded>, request: Request) -> impl IntoResponse {
    let path = request.uri().path().to_owned();
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let stream = value["stream"] == true;
    let fixture = value["fixture"].as_str().unwrap_or("").to_owned();
    recorded.lock().unwrap().push((path, parts.headers, value));
    if fixture == "timeout" {
        return std::future::pending::<axum::response::Response>().await;
    }
    if fixture == "fail" {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "fixture failure",
        )
            .into_response();
    }
    if fixture == "disconnect" {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
        let cancelled = recorded.clone();
        tokio::spawn(async move {
            tx.send(Ok(bytes::Bytes::from_static(b"data: {}\n\n")))
                .await
                .unwrap();
            tx.closed().await;
            cancelled.lock().unwrap().push((
                "cancelled".into(),
                axum::http::HeaderMap::new(),
                Value::Null,
            ));
        });
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
        )
            .into_response();
    }
    if stream {
        ([("content-type", "text/event-stream")], Body::from("data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n")).into_response()
    } else {
        axum::Json(json!({"id":"fixture", "usage":{"prompt_tokens":2,"completion_tokens":3}}))
            .into_response()
    }
}
fn config(root: &std::path::Path, url: &str) {
    let quote = |s: String| toml::Value::String(s).to_string();
    fs::write(
        root.join("config.toml"),
        format!(
            r#"
[server]
inference_bind = "127.0.0.1:0"
management_bind = "127.0.0.1:0"
[database]
url = {}
[object_storage]
root = {}
[queues]
accounting_journal = {}
[logging]
json = true
[native]
[[native.providers]]
id = "open"
protocol = "openai"
base_url = "{url}"
credential_env = "STEVE_NATIVE_E2E_KEY"
[[native.providers]]
id = "anth"
protocol = "anthropic"
base_url = "{url}"
credential_env = "STEVE_NATIVE_E2E_KEY"
[[native.providers]]
id = "disabled"
protocol = "openai"
base_url = "{url}"
credential_env = "STEVE_NATIVE_DISABLED_KEY"
enabled = false
[[native.models]]
id = "z-tie"
provider = "open"
upstream_model = "up-z"
protocols = ["chat", "responses"]
input_micro_usd_per_million = 1
output_micro_usd_per_million = 1
[[native.models]]
id = "a-tie"
provider = "open"
upstream_model = "up-a"
protocols = ["chat", "responses"]
input_micro_usd_per_million = 1
output_micro_usd_per_million = 1
[[native.models]]
id = "claude"
provider = "anth"
upstream_model = "up-claude"
protocols = ["messages"]
input_micro_usd_per_million = 0
output_micro_usd_per_million = 0
[[native.models]]
id = "disabled-model"
provider = "disabled"
upstream_model = "disabled"
protocols = ["chat"]
input_micro_usd_per_million = 0
output_micro_usd_per_million = 0
"#,
            quote(format!(
                "sqlite://{}?mode=rwc",
                root.join("db.sqlite").display()
            )),
            quote(root.join("objects").display().to_string()),
            quote(root.join("accounting").display().to_string())
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn native_process_routes_credentials_catalogue_and_restart() {
    let recorded: Recorded = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .route("/v1/responses", post(upstream))
        .route("/v1/messages", post(upstream))
        .with_state(recorded.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    config(root.path(), &url);
    let status = process::steve_command()
        .args(["accounting", "provision", "--root"])
        .arg(root.path().join("accounting"))
        .status()
        .unwrap();
    assert!(status.success());
    for cycle in 0..if cfg!(unix) { 2 } else { 1 } {
        let client = reqwest::Client::new();
        let lock = process::acquire_accounting_startup_lock().unwrap();
        let mut child = NativeProcess::start(root.path(), Some("server-dummy-key"));
        let base = child.ready().await;
        drop(lock);
        let models: Value = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ids: Vec<_> = models["data"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "models: {models}; logs: {}",
                    fs::read_to_string(&child.log).unwrap()
                )
            })
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a-tie", "claude", "z-tie"]);
        for stream in [false, true] {
            for (path, mut body, upstream_model) in [
                (
                    "chat/completions",
                    json!({"model":"auto","messages":[{"role":"user","content":"hi"}],"temperature":0.4}),
                    "up-a",
                ),
                (
                    "responses",
                    json!({"model":"z-tie","input":"hi","stream":true}),
                    "up-z",
                ),
                (
                    "messages",
                    json!({"model":"auto","max_tokens":5,"messages":[{"role":"user","content":"hi"}]}),
                    "up-claude",
                ),
            ] {
                if path == "chat/completions" && stream {
                    body["model"] = "a-tie".into();
                }
                body["stream"] = stream.into();
                let response = client
                    .post(format!("{base}/v1/{path}"))
                    .header("authorization", "Bearer client-secret")
                    .header("x-api-key", "client-key")
                    .header("anthropic-version", "client-version")
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                assert!(
                    response.status().is_success(),
                    "{}",
                    response.text().await.unwrap()
                );
                let bytes = response.bytes().await.unwrap();
                if body["stream"] == true {
                    assert_eq!(bytes, "data: {\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n");
                }
                let received = recorded.lock().unwrap().last().unwrap().clone();
                let mut expected = body.clone();
                expected["model"] = upstream_model.into();
                assert_eq!(received.2, expected);
                if path == "messages" {
                    assert_eq!(received.1["x-api-key"], "server-dummy-key");
                    assert_eq!(received.1["anthropic-version"], "2023-06-01");
                    assert!(!received.1.contains_key("authorization"));
                } else {
                    assert_eq!(received.1["authorization"], "Bearer server-dummy-key");
                    assert!(!received.1.contains_key("x-api-key"));
                    assert!(!received.1.contains_key("anthropic-version"));
                }
            }
        }
        for body in [
            json!({"messages":[]}),
            json!({"model":"claude","messages":[]}),
            json!({"model":"disabled-model","messages":[]}),
        ] {
            assert_eq!(
                client
                    .post(format!("{base}/v1/chat/completions"))
                    .json(&body)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                400
            );
        }
        assert_eq!(
            client
                .post(format!("{base}/v1/chat/completions"))
                .body("{")
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let logs = fs::read_to_string(&child.log).unwrap();
            let responses: Vec<Value> = logs
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|event| {
                    event["fields"]["message"] == "native_response"
                        && event["fields"]["status"] == 200
                })
                .collect();
            if responses.len() == 6 {
                for response in responses {
                    assert_eq!(response["fields"]["input_tokens"], "Some(2)");
                    assert_eq!(response["fields"]["output_tokens"], "Some(3)");
                    assert_eq!(response["fields"]["completed"], true);
                    assert!(response["fields"]["request_id"].as_str().is_some());
                }
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "missing telemetry: {logs}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!fs::read_to_string(&child.log)
            .unwrap()
            .contains("server-dummy-key"));
        let before = recorded.lock().unwrap().len();
        let prior_cancelled = recorded
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| entry.0 == "cancelled")
            .count();
        let response = client.post(format!("{base}/v1/chat/completions")).json(&json!({"model":"auto","messages":[{"role":"user","content":"hi"}],"fixture":"fail"})).send().await.unwrap();
        assert_eq!(response.status(), 502);
        response.bytes().await.unwrap();
        assert_eq!(
            recorded.lock().unwrap().len(),
            before + 2,
            "upstream error exceeded one bounded retry"
        );
        let mut response = client.post(format!("{base}/v1/chat/completions")).json(&json!({"model":"auto","messages":[{"role":"user","content":"hi"}],"stream":true,"fixture":"disconnect"})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.chunk().await.unwrap().unwrap(), "data: {}\n\n");
        drop(response);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let cancelled = recorded
                .lock()
                .unwrap()
                .iter()
                .filter(|entry| entry.0 == "cancelled")
                .count()
                == prior_cancelled + 1;
            let logs = fs::read_to_string(&child.log).unwrap();
            let observed = logs
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|event| {
                    event["fields"]["message"] == "native_response"
                        && event["fields"]["cancelled"] == true
                        && event["fields"]["input_tokens"] == "None"
                        && event["fields"]["output_tokens"] == "None"
                });
            if cancelled && observed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "native disconnect did not cancel upstream/observer: {logs}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            recorded.lock().unwrap().len(),
            before + 4,
            "disconnect replayed"
        );
        if cycle == 0 {
            let before_timeout = recorded.lock().unwrap().len();
            let response = tokio::time::timeout(
                Duration::from_secs(35),
                client
                    .post(format!("{base}/v1/responses"))
                    .json(&json!({"model":"auto","input":"hi","stream":true,"fixture":"timeout"}))
                    .send(),
            )
            .await
            .expect("native header timeout did not finish")
            .unwrap();
            assert_eq!(response.status(), 504);
            response.bytes().await.unwrap();
            assert_eq!(
                recorded.lock().unwrap().len(),
                before_timeout + 1,
                "timeout replayed"
            );
        }
        client
            .post(format!("{}/api/v1/system/drain", child.management))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        drop(client);
        #[cfg(unix)]
        {
            assert!(std::process::Command::new("kill")
                .args(["-TERM", &child.child.id().to_string()])
                .status()
                .unwrap()
                .success());
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "graceful shutdown timed out: {}",
                    fs::read_to_string(&child.log).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        #[cfg(unix)]
        {
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .connect(&format!(
                    "sqlite://{}",
                    root.path().join("db.sqlite").display()
                ))
                .await
                .unwrap();
            let payloads: Vec<String> = sqlx::query_scalar("SELECT payload FROM steve_background_events WHERE kind = 'chat.attempt.terminal.v1'").fetch_all(&pool).await.unwrap();
            assert!(!payloads.is_empty());
            let mut requested_models = std::collections::BTreeSet::new();
            for payload in payloads {
                let payload: Value = serde_json::from_str(&payload).unwrap();
                let model = payload["model"].as_str().unwrap();
                assert!(matches!(model, "auto" | "a-tie"), "{payload}");
                requested_models.insert(model.to_owned());
                assert_eq!(payload["provider"], "open");
            }
            assert_eq!(
                requested_models,
                ["a-tie".to_owned(), "auto".to_owned()].into()
            );
            pool.close().await;
        }
        drop(child);
    }
    for credential in [None, Some("invalid\ncredential")] {
        let lock = process::acquire_accounting_startup_lock().unwrap();
        let mut child = NativeProcess::start(root.path(), credential);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.child.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        drop(lock);
        let logs = fs::read_to_string(&child.log).unwrap();
        assert!(!logs.contains("invalid\ncredential"));
        assert!(!logs.contains("listeners_ready"));
    }
    server.abort();
}
