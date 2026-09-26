use crate::{
    config::Config,
    deferred::DeferredQueues,
    lifecycle::{Lifecycle, Phase},
    storage::{Database, ObjectStorage},
};
use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    net::{SocketAddr, TcpListener as StdTcpListener},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, signal};
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
    let state = Arc::new(AppState {
        lifecycle: lifecycle.clone(),
        deferred,
        _db: db,
        _objects: objects,
    });
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/system/version", get(version))
        .route("/api/v1/system/status", get(status))
        .route("/api/v1/system/drain", post(drain))
        .route("/api/v1/test/echo", post(echo))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cfg
        .server
        .bind
        .parse()
        .context("parsing server bind address")?;
    let listener = bind_listener(addr).await?;
    let local_addr = listener.local_addr()?;

    lifecycle.ready("listener_bound");
    tracing::info!(
        event = "listener_ready",
        %local_addr,
        dual_stack = is_dual_stack_address(addr),
        "Steve ready"
    );

    let shutdown_lifecycle = lifecycle.clone();
    let timeout = Duration::from_secs(cfg.server.drain_timeout_seconds);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
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
        })
        .await?;

    lifecycle.stopped("server_exited");
    Ok(())
}

async fn bind_listener(addr: SocketAddr) -> Result<TcpListener> {
    if is_dual_stack_address(addr) {
        let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
        socket.set_only_v6(false)?;
        socket.set_reuse_address(true)?;
        socket.bind(&addr.into())?;
        socket.listen(1024)?;
        socket.set_nonblocking(true)?;
        let listener: StdTcpListener = socket.into();
        return TcpListener::from_std(listener).context("creating dual-stack listener");
    }

    TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding listener on {addr}"))
}

fn is_dual_stack_address(addr: SocketAddr) -> bool {
    addr.is_ipv6() && addr.ip().is_unspecified()
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
        "phase": state.lifecycle.phase(),
        "inflight": state.lifecycle.inflight(),
        "queues": state.deferred.snapshot(),
    }))
}

async fn drain(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.lifecycle.drain("management_api");
    Json(json!({"phase": "draining"}))
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
        format!("test/{}.json", uuid::Uuid::now_v7()),
        bytes::Bytes::from(payload.to_string()),
    );

    (StatusCode::OK, Json(payload))
}

async fn shutdown_signal() -> &'static str {
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

    #[test]
    fn unspecified_ipv6_is_dual_stack() {
        let addr: SocketAddr = "[::]:11435".parse().expect("parse");
        assert!(is_dual_stack_address(addr));
    }

    #[test]
    fn loopback_ipv6_is_not_marked_dual_stack() {
        let addr: SocketAddr = "[::1]:11435".parse().expect("parse");
        assert!(!is_dual_stack_address(addr));
    }
}
