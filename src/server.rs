use crate::{
    config::{Config, ModelConfig},
    deferred::DeferredQueues,
    lifecycle::{Lifecycle, Phase},
    models::{self, ModelList},
    net::{bind_listener, is_dual_stack_address},
    proxy::{anthropic_messages, openai_chat, openai_responses, AnthropicUpstream, OpenAiUpstream},
    storage::{Database, ObjectStorage},
};
use anyhow::{Context, Result};
use axum::{
    extract::{FromRef, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    signal,
    sync::{watch, Semaphore},
};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Default)]
struct RequestCounters {
    inference: AtomicU64,
    management: AtomicU64,
}

#[derive(Clone)]
struct AppState {
    lifecycle: Lifecycle,
    deferred: DeferredQueues,
    _db: Database,
    _objects: ObjectStorage,
    counters: Arc<RequestCounters>,
    instance_id: String,
    generation: String,
    started_at: String,
    inference_bind: String,
    management_bind: String,
    models: Arc<[ModelConfig]>,
    openai_upstream: Option<OpenAiUpstream>,
    anthropic_upstream: Option<AnthropicUpstream>,
    provider_probe: ProviderProbeState,
}

#[derive(Clone)]
struct Catalogue(Arc<[ModelConfig]>);

impl FromRef<Arc<AppState>> for Catalogue {
    fn from_ref(state: &Arc<AppState>) -> Self {
        Self(Arc::clone(&state.models))
    }
}

#[derive(Clone)]
struct ProviderProbeState {
    client: reqwest::Client,
    probe_gate: Arc<Semaphore>,
    openai_url: Option<String>,
    anthropic_url: Option<String>,
}

impl ProviderProbeState {
    fn new(
        openai_url: Option<String>,
        anthropic_url: Option<String>,
    ) -> std::result::Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            probe_gate: Arc::new(Semaphore::new(1)),
            openai_url,
            anthropic_url,
        })
    }
}

impl FromRef<Arc<AppState>> for ProviderProbeState {
    fn from_ref(state: &Arc<AppState>) -> Self {
        state.provider_probe.clone()
    }
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    phase: Phase,
    inflight: u64,
}

#[derive(Serialize)]
struct Version {
    name: &'static str,
    version: &'static str,
}

#[derive(Deserialize)]
struct EchoRequest {
    #[serde(default)]
    value: Value,
}

#[derive(Clone)]
enum RequestClass {
    Inference(Arc<RequestCounters>),
    Management(Arc<RequestCounters>),
}

pub async fn run(
    cfg: Config,
    db: Database,
    objects: ObjectStorage,
    deferred: DeferredQueues,
    lifecycle: Lifecycle,
) -> Result<()> {
    let inference_addr: SocketAddr = cfg
        .server
        .inference_bind
        .parse()
        .context("parsing inference bind address")?;
    let management_addr: SocketAddr = cfg
        .server
        .management_bind
        .parse()
        .context("parsing management bind address")?;

    if inference_addr == management_addr {
        anyhow::bail!("inference and management listeners must use different addresses");
    }

    let inference_listener = bind_listener(inference_addr).await?;
    let management_listener = bind_listener(management_addr).await?;
    let inference_local = inference_listener.local_addr()?;
    let management_local = management_listener.local_addr()?;

    let catalogue: Arc<[ModelConfig]> = models::resolve_catalogue(&cfg.models).into();
    tracing::info!(
        event = "model_catalogue_loaded",
        count = catalogue.len(),
        source = if cfg.models.is_empty() {
            "static"
        } else {
            "config"
        },
        "model catalogue loaded"
    );

    let counters = Arc::new(RequestCounters::default());
    let openai_upstream = cfg
        .server
        .openai_upstream_url
        .as_ref()
        .map(|url| OpenAiUpstream::new(url, Duration::from_secs(30)))
        .transpose()?;
    let anthropic_upstream = cfg
        .server
        .anthropic_upstream_url
        .as_ref()
        .map(|url| AnthropicUpstream::new(url, Duration::from_secs(30)))
        .transpose()?;
    let provider_probe = ProviderProbeState::new(
        cfg.server.openai_upstream_url.clone(),
        cfg.server.anthropic_upstream_url.clone(),
    )
    .context("building provider health client")?;
    let state = Arc::new(AppState {
        lifecycle: lifecycle.clone(),
        deferred,
        _db: db,
        _objects: objects,
        counters: counters.clone(),
        instance_id: Uuid::now_v7().to_string(),
        generation: std::env::var("STEVE_GENERATION").unwrap_or_else(|_| "standalone".into()),
        started_at: Utc::now().to_rfc3339(),
        inference_bind: cfg.server.inference_bind.clone(),
        management_bind: cfg.server.management_bind.clone(),
        models: catalogue,
        openai_upstream,
        anthropic_upstream,
        provider_probe,
    });

    let inference_app = inference_router(state.clone());
    let management_app = management_router(state.clone());

    lifecycle.ready("listeners_bound");
    tracing::info!(
        event = "listeners_ready",
        inference = %inference_local,
        inference_dual_stack = is_dual_stack_address(inference_addr),
        management = %management_local,
        management_dual_stack = is_dual_stack_address(management_addr),
        instance_id = %state.instance_id,
        generation = %state.generation,
        pid = std::process::id(),
        "Steve ready"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_lifecycle = lifecycle.clone();
    let timeout = Duration::from_secs(cfg.server.drain_timeout_seconds);
    let signal_tx = shutdown_tx.clone();

    let signal_task = tokio::spawn(async move {
        let reason = shutdown_signal().await;
        shutdown_lifecycle.drain(reason);

        let result = tokio::time::timeout(timeout, shutdown_lifecycle.wait_for_zero()).await;
        if result.is_err() {
            tracing::warn!(
                event = "drain_timeout",
                timeout_seconds = timeout.as_secs(),
                inflight = shutdown_lifecycle.inflight(),
                "drain deadline reached"
            );
        }

        let _ = signal_tx.send(true);
    });

    let inference = axum::serve(inference_listener, inference_app)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()));
    let management = axum::serve(management_listener, management_app)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx));

    let result = tokio::try_join!(inference, management);
    let _ = shutdown_tx.send(true);
    signal_task.abort();

    lifecycle.stopped("listeners_exited");
    result?;
    Ok(())
}

fn inference_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/responses", post(responses))
        .route("/api/v1/test/echo", post(echo))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            RequestClass::Inference(state.counters.clone()),
            track_requests,
        ))
        .with_state(state)
}

fn management_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/system/version", get(version))
        .route("/api/v1/system/status", get(status))
        .route("/api/v1/system/drain", post(drain))
        .route("/api/v1/providers/health", get(provider_health))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            RequestClass::Management(state.counters.clone()),
            track_requests,
        ))
        .with_state(state)
}

async fn probe_provider(
    client: reqwest::Client,
    url: Option<String>,
    path: &'static str,
) -> &'static str {
    let Some(url) = url else {
        return "unconfigured";
    };

    let base = url.trim().trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        format!("{base}{path}")
    } else {
        format!("{base}/v1{path}")
    };
    match tokio::time::timeout(Duration::from_millis(500), client.get(url).send()).await {
        Ok(Ok(response))
            if response.status().as_u16() < 500 && response.status().as_u16() != 404 =>
        {
            "healthy"
        }
        _ => "unhealthy",
    }
}

async fn provider_health(State(probe): State<ProviderProbeState>) -> Response {
    let Ok(_permit) = probe.probe_gate.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"status": "busy"})),
        )
            .into_response();
    };
    let (openai, anthropic) = tokio::join!(
        probe_provider(probe.client.clone(), probe.openai_url, "/chat/completions"),
        probe_provider(probe.client, probe.anthropic_url, "/messages"),
    );
    let healthy = openai != "unhealthy" && anthropic != "unhealthy";
    let status = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(json!({
            "status": if healthy { "healthy" } else { "unhealthy" },
            "providers": {
                "openai": {"status": openai},
                "anthropic": {"status": anthropic},
            }
        })),
    )
        .into_response()
}

async fn list_models(State(Catalogue(catalogue)): State<Catalogue>) -> Json<ModelList> {
    Json(ModelList::from_catalogue(&catalogue))
}

async fn live(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(Health {
        status: "ok",
        phase: state.lifecycle.phase(),
        inflight: state.lifecycle.inflight(),
    })
}

async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let health = Health {
        status: if state.lifecycle.is_ready() {
            "ready"
        } else {
            "not_ready"
        },
        phase: state.lifecycle.phase(),
        inflight: state.lifecycle.inflight(),
    };

    if state.lifecycle.is_ready() {
        (StatusCode::OK, Json(health)).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(health)).into_response()
    }
}

async fn version() -> Json<Version> {
    Json(Version {
        name: "steve",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "name": "steve",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "instance_id": state.instance_id,
        "generation": state.generation,
        "started_at": state.started_at,
        "phase": state.lifecycle.phase(),
        "inflight": state.lifecycle.inflight(),
        "active_inference_requests": state.counters.inference.load(Ordering::Relaxed),
        "active_management_requests": state.counters.management.load(Ordering::Relaxed),
        "inference_bind": state.inference_bind,
        "management_bind": state.management_bind,
        "queues": state.deferred.snapshot(),
    }))
}

async fn drain(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.lifecycle.drain("management_api");
    Json(json!({"phase": "draining"}))
}

async fn messages(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(_guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let reply = if let Some(upstream) = &state.anthropic_upstream {
        anthropic_messages::handle_messages_with_upstream(&body, upstream).await
    } else {
        anthropic_messages::handle_messages(&body)
    };
    if let Some(attempt) = &reply.attempt {
        if attempt.finished_at.is_some() {
            tracing::info!(
                request_id = %attempt.request_id.0,
                attempt_id = %attempt.id.0,
                status = ?attempt.status,
                finished_at = ?attempt.finished_at,
                "messages upstream attempt finished"
            );
        }
    }
    (reply.status, Json(reply.body)).into_response()
}

async fn responses(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(_guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let reply = openai_responses::handle_responses(&body);
    (reply.status, Json(reply.body)).into_response()
}

async fn chat_completions(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(_guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let reply = if let Some(upstream) = &state.openai_upstream {
        openai_chat::handle_chat_completions_with_upstream(&body, upstream).await
    } else {
        openai_chat::handle_chat_completions(&body)
    };
    if let Some(attempt) = &reply.attempt {
        if attempt.finished_at.is_some() {
            tracing::info!(
                request_id = %attempt.request_id.0,
                attempt_id = %attempt.id.0,
                status = ?attempt.status,
                finished_at = ?attempt.finished_at,
                "chat completions upstream attempt finished"
            );
        }
    }
    (reply.status, Json(reply.body)).into_response()
}

async fn echo(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EchoRequest>,
) -> impl IntoResponse {
    let Some(_guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"draining"})),
        );
    };

    let payload = json!({"value": req.value});
    state.deferred.accounting("test.echo", payload.clone());
    state.deferred.telemetry("test.echo", payload.clone());
    state.deferred.history(
        format!("test/{}.json", Uuid::now_v7()),
        bytes::Bytes::from(payload.to_string()),
    );

    (StatusCode::OK, Json(payload))
}

async fn track_requests(
    State(class): State<RequestClass>,
    request: Request,
    next: Next,
) -> Response {
    let (counter, class_name) = match &class {
        RequestClass::Inference(counters) => (&counters.inference, "inference"),
        RequestClass::Management(counters) => (&counters.management, "management"),
    };

    counter.fetch_add(1, Ordering::Relaxed);
    let response = next.run(request).await;
    counter.fetch_sub(1, Ordering::Relaxed);

    tracing::trace!(request_class = class_name, "request completed");
    response
}

async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }

    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

pub(crate) async fn shutdown_signal() -> &'static str {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
        "ctrl_c"
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
        "sigterm"
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<&'static str>();

    tokio::select! {
        reason = ctrl_c => reason,
        reason = terminate => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_upstream;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use std::future::IntoFuture;
    use tower::ServiceExt;

    fn models_router(catalogue: Arc<[ModelConfig]>) -> Router {
        Router::new()
            .route("/v1/models", get(list_models))
            .with_state(Catalogue(catalogue))
    }

    async fn get_models(catalogue: Arc<[ModelConfig]>) -> (StatusCode, Value) {
        let response = tokio::time::timeout(
            Duration::from_millis(200),
            models_router(catalogue).oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("model listing must not wait on an upstream")
        .expect("response");

        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).expect("json");
        (status, body)
    }

    #[tokio::test]
    async fn get_v1_models_lists_configured_models() {
        let catalogue: Arc<[ModelConfig]> = vec![
            ModelConfig {
                id: "alpha".into(),
                owned_by: "lab".into(),
                created: 7,
            },
            ModelConfig {
                id: "beta".into(),
                owned_by: "lab".into(),
                created: 8,
            },
        ]
        .into();

        let (status, body) = get_models(catalogue).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "object": "list",
                "data": [
                    {"id": "alpha", "object": "model", "created": 7, "owned_by": "lab"},
                    {"id": "beta", "object": "model", "created": 8, "owned_by": "lab"}
                ]
            })
        );
    }

    #[tokio::test]
    async fn get_v1_models_uses_static_catalogue_when_unconfigured() {
        let catalogue: Arc<[ModelConfig]> = models::resolve_catalogue(&[]).into();
        let (status, body) = get_models(catalogue).await;
        assert_eq!(status, StatusCode::OK);
        let data = body["data"].as_array().expect("data array");
        assert!(!data.is_empty());
        assert_eq!(body["object"], "list");
        assert_eq!(data[0]["id"], "steve-test-model");
        assert_eq!(data[0]["object"], "model");
        assert_eq!(data[0]["owned_by"], "steve");
    }

    fn provider_router(state: ProviderProbeState) -> Router {
        Router::new()
            .route("/api/v1/providers/health", get(provider_health))
            .route("/health/live", get(|| async { "live" }))
            .with_state(state)
    }

    async fn get_provider_health(state: ProviderProbeState) -> (StatusCode, Value) {
        let response = provider_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/providers/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    async fn hanging_probe() -> &'static str {
        tokio::time::sleep(Duration::from_secs(2)).await;
        "late"
    }

    #[tokio::test]
    async fn provider_health_reports_reachable_closed_and_unconfigured() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/chat/completions",
                    get(|| async { (StatusCode::METHOD_NOT_ALLOWED, "") }),
                ),
            )
            .into_future(),
        );
        let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_addr = closed_listener.local_addr().unwrap();
        let closed_task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = closed_listener.accept().await else {
                    break;
                };
                drop(socket);
            }
        });
        let (status, body) = get_provider_health(
            ProviderProbeState::new(
                Some(format!("http://{addr}")),
                Some(format!("http://{closed_addr}")),
            )
            .unwrap(),
        )
        .await;
        server.abort();
        closed_task.abort();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "unhealthy");
        assert_eq!(body["providers"]["openai"]["status"], "healthy");
        assert_eq!(body["providers"]["anthropic"]["status"], "unhealthy");
        assert!(!body["providers"]["openai"]
            .as_object()
            .unwrap()
            .contains_key("url"));
    }

    #[tokio::test]
    async fn provider_health_reports_fixture_and_unconfigured_as_healthy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, test_upstream::router()).into_future());
        let (status, body) = get_provider_health(
            ProviderProbeState::new(Some(format!("  http://{addr}///  ")), None).unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["providers"]["openai"]["status"], "healthy");
        assert_eq!(body["providers"]["anthropic"]["status"], "unconfigured");

        let (status, body) = get_provider_health(
            ProviderProbeState::new(Some(format!(" http://{addr}/v1/ ")), None).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["providers"]["openai"]["status"], "healthy");
        server.abort();
    }

    #[tokio::test]
    async fn provider_health_rejects_missing_and_server_error_provider_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/v1/chat/completions",
                        get(|| async { StatusCode::NOT_FOUND }),
                    )
                    .route(
                        "/v1/messages",
                        get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
                    ),
            )
            .into_future(),
        );
        let (status, body) = get_provider_health(
            ProviderProbeState::new(
                Some(format!("http://{addr}")),
                Some(format!("http://{addr}")),
            )
            .unwrap(),
        )
        .await;
        server.abort();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "unhealthy");
        assert_eq!(body["providers"]["openai"]["status"], "unhealthy");
        assert_eq!(body["providers"]["anthropic"]["status"], "unhealthy");
    }

    #[tokio::test]
    async fn provider_health_times_out_without_blocking_other_management_routes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = provider_router(
            ProviderProbeState::new(
                Some(format!("http://{addr}")),
                Some(format!("http://{addr}")),
            )
            .unwrap(),
        );
        let server = tokio::spawn(
            axum::serve(
                listener,
                router
                    .clone()
                    .route("/v1/chat/completions", get(hanging_probe))
                    .route("/v1/messages", get(hanging_probe)),
            )
            .into_future(),
        );
        let client = reqwest::Client::new();
        let probe = tokio::spawn(
            client
                .get(format!("http://{addr}/api/v1/providers/health"))
                .send(),
        );
        let live = tokio::time::timeout(
            Duration::from_millis(100),
            client.get(format!("http://{addr}/health/live")).send(),
        )
        .await
        .expect("management route must remain responsive")
        .unwrap();
        assert_eq!(live.status(), StatusCode::OK);
        let response = tokio::time::timeout(Duration::from_secs(1), probe)
            .await
            .expect("probe must finish within one second")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        server.abort();
    }

    #[tokio::test]
    async fn provider_health_returns_busy_while_another_probe_is_running() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", get(hanging_probe))
                    .route("/v1/messages", get(hanging_probe)),
            )
            .into_future(),
        );
        let state = ProviderProbeState::new(
            Some(format!("http://{addr}")),
            Some(format!("http://{addr}")),
        )
        .unwrap();
        let gate = state.probe_gate.clone();
        let router = provider_router(state);
        let first = tokio::spawn(
            router.clone().oneshot(
                Request::builder()
                    .uri("/api/v1/providers/health")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::time::timeout(Duration::from_millis(100), async {
            while gate.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first probe must acquire the gate");
        let second = tokio::time::timeout(
            Duration::from_millis(100),
            router.oneshot(
                Request::builder()
                    .uri("/api/v1/providers/health")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("busy response must not queue")
        .unwrap();
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
        first.abort();
        server.abort();
    }
}
