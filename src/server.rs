use crate::{
    config::{Config, ModelConfig},
    deferred::DeferredQueues,
    lifecycle::{Lifecycle, Phase},
    models::{self, ModelList},
    net::{bind_listener, is_dual_stack_address},
    proxy::openai_chat,
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
use tokio::{signal, sync::watch};
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
}

#[derive(Clone)]
struct Catalogue(Arc<[ModelConfig]>);

impl FromRef<Arc<AppState>> for Catalogue {
    fn from_ref(state: &Arc<AppState>) -> Self {
        Self(Arc::clone(&state.models))
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
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            RequestClass::Management(state.counters.clone()),
            track_requests,
        ))
        .with_state(state)
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

async fn chat_completions(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(_guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let reply = openai_chat::handle_chat_completions(&body);
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
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
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
}
