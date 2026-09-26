use crate::{
    config::Config,
    deferred::DeferredQueues,
    lifecycle::{Lifecycle, Phase},
    storage::{Database, ObjectStorage},
};
use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::signal;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
struct AppState {
    lifecycle: Lifecycle,
    deferred: DeferredQueues,
    _db: Database,
    _objects: ObjectStorage,
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

pub async fn run(
    cfg: Config,
    db: Database,
    objects: ObjectStorage,
    deferred: DeferredQueues,
    lifecycle: Lifecycle,
) -> Result<()> {
    let state = Arc::new(AppState { lifecycle: lifecycle.clone(), deferred, _db: db, _objects: objects });
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/system/version", get(version))
        .route("/api/v1/system/status", get(status))
        .route("/api/v1/system/drain", post(drain))
        .route("/api/v1/test/echo", post(echo))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cfg.server.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    lifecycle.ready();
    tracing::info!(%addr, "Steve ready");

    let shutdown_lifecycle = lifecycle.clone();
    let timeout = Duration::from_secs(cfg.server.drain_timeout_seconds);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_lifecycle.drain();
            tracing::info!("Steve draining");
            let _ = tokio::time::timeout(timeout, shutdown_lifecycle.wait_for_zero()).await;
        })
        .await?;

    lifecycle.stopped();
    Ok(())
}

async fn live(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(Health { status: "ok", phase: state.lifecycle.phase(), inflight: state.lifecycle.inflight() })
}

async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let health = Health {
        status: if state.lifecycle.is_ready() { "ready" } else { "not_ready" },
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
    Json(Version { name: "steve", version: env!("CARGO_PKG_VERSION") })
}

async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "phase": state.lifecycle.phase(),
        "inflight": state.lifecycle.inflight(),
        "queues": state.deferred.snapshot(),
    }))
}

async fn drain(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.lifecycle.drain();
    Json(json!({"phase": "draining"}))
}

async fn echo(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EchoRequest>,
) -> impl IntoResponse {
    let Some(_guard) = state.lifecycle.enter() else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"draining"})));
    };

    let payload = json!({"value": req.value});
    state.deferred.accounting("test.echo", payload.clone());
    state.deferred.telemetry("test.echo", payload.clone());
    state.deferred.history(
        format!("test/{}.json", uuid::Uuid::now_v7()),
        bytes::Bytes::from(payload.to_string()),
    );

    (StatusCode::OK, Json(payload))
}

async fn shutdown_signal() {
    let ctrl_c = async { signal::ctrl_c().await.expect("install Ctrl+C handler"); };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
