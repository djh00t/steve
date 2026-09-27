use crate::{
    config::{Config, ModelConfig},
    deferred::DeferredQueues,
    lifecycle::{InflightGuard, Lifecycle, Phase},
    models::{self, ModelList},
    net::{bind_listener, is_dual_stack_address},
    proxy::{anthropic_messages, openai_chat, openai_responses, AnthropicUpstream, OpenAiUpstream},
    storage::{Database, ObjectStorage},
};
use anyhow::{Context, Result};
use axum::{
    body::{Body, HttpBody},
    extract::{FromRef, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use http_body::{Frame, SizeHint};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context as TaskContext, Poll},
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
    openai_upstream: Option<OpenAiUpstream>,
    anthropic_upstream: Option<AnthropicUpstream>,
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

    let inference = axum::serve(inference_listener, inference_app)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()));
    let management = axum::serve(management_listener, management_app)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx));

    let result = serve_until_drained(
        async { tokio::try_join!(inference, management).map(|_| ()) },
        shutdown_signal(),
        shutdown_lifecycle,
        timeout,
        signal_tx,
    )
    .await;
    let _ = shutdown_tx.send(true);

    lifecycle.stopped("listeners_exited");
    result?;
    Ok(())
}

async fn serve_until_drained<S, F>(
    serving: S,
    signal: F,
    lifecycle: Lifecycle,
    timeout: Duration,
    shutdown_tx: watch::Sender<bool>,
) -> std::io::Result<()>
where
    S: Future<Output = std::io::Result<()>>,
    F: Future<Output = &'static str>,
{
    let drain = async {
        lifecycle.drain(signal.await);
        let timed_out = tokio::time::timeout(timeout, lifecycle.wait_for_zero())
            .await
            .is_err();
        if timed_out {
            tracing::warn!(
                event = "drain_timeout",
                timeout_seconds = timeout.as_secs(),
                inflight = lifecycle.inflight(),
                "drain deadline reached"
            );
        }
        let _ = shutdown_tx.send(true);
        timed_out
    };

    tokio::pin!(serving);
    tokio::select! {
        result = &mut serving => result,
        timed_out = drain => {
            if timed_out {
                Ok(())
            } else {
                serving.await
            }
        }
    }
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

async fn messages(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(guard) = state.lifecycle.enter() else {
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
    let streaming = matches!(
        &reply.body,
        anthropic_messages::MessagesReplyBody::Stream(_)
    );
    let response = reply.into_response();
    if streaming {
        hold_inflight_until_body_end(response, guard)
    } else {
        response
    }
}

fn hold_inflight_until_body_end(response: Response, guard: InflightGuard) -> Response {
    hold_guard_until_body_end(response, guard)
}

fn hold_guard_until_body_end<G: Send + Unpin + 'static>(response: Response, guard: G) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(GuardedBody {
            inner: body,
            guard: Some(guard),
        }),
    )
}

struct GuardedBody<G> {
    inner: Body,
    guard: Option<G>,
}

impl<G: Unpin> HttpBody for GuardedBody<G> {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_frame(cx);
        if matches!(&result, Poll::Ready(None | Some(Err(_)))) {
            this.guard.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

async fn responses(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let Some(guard) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let reply = if let Some(upstream) = &state.openai_upstream {
        openai_responses::handle_responses_with_upstream(&body, upstream).await
    } else {
        openai_responses::handle_responses(&body)
    };
    let streaming = matches!(&reply.body, openai_responses::ResponsesReplyBody::Stream(_));
    let response = reply.into_response();
    if streaming {
        hold_inflight_until_body_end(response, guard)
    } else {
        response
    }
}

fn hold_inflight_until_body_end(response: Response, guard: InflightGuard) -> Response {
    hold_guard_until_body_end(response, guard)
}

fn hold_guard_until_body_end<G: Send + Unpin + 'static>(response: Response, guard: G) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(GuardedBody {
            inner: body,
            guard: Some(guard),
        }),
    )
}

struct GuardedBody<G> {
    inner: Body,
    guard: Option<G>,
}

impl<G: Unpin> HttpBody for GuardedBody<G> {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_frame(cx);
        if matches!(&result, Poll::Ready(None | Some(Err(_)))) {
            this.guard.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
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
    let attempt_count = reply
        .request
        .as_ref()
        .map_or(0, |request| request.attempts.len());
    for attempt in &reply.attempts {
        if attempt.finished_at.is_some() {
            tracing::info!(
                request_id = %attempt.request_id.0,
                attempt_id = %attempt.id.0,
                attempt_count,
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
    let guard = RequestCounterGuard::new(class);
    let response = next.run(request).await;
    hold_guard_until_body_end(response, guard)
}

struct RequestCounterGuard(RequestClass);

impl RequestCounterGuard {
    fn new(class: RequestClass) -> Self {
        class.counter().fetch_add(1, Ordering::Relaxed);
        Self(class)
    }
}

impl RequestClass {
    fn counter(&self) -> &AtomicU64 {
        match self {
            Self::Inference(counters) => &counters.inference,
            Self::Management(counters) => &counters.management,
        }
    }
}

impl Drop for RequestCounterGuard {
    fn drop(&mut self) {
        self.0.counter().fetch_sub(1, Ordering::Relaxed);
        tracing::trace!(
            request_class = match self.0 {
                RequestClass::Inference(_) => "inference",
                RequestClass::Management(_) => "management",
            },
            "request completed"
        );
    }
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
    use axum::{body::HttpBody, http::Request};
    use http_body_util::BodyExt;
    use tokio_stream::StreamExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn drain_deadline_cancels_stuck_serving_future() {
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");
        let _guard = lifecycle.enter().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (serve_tx, serve_rx) = tokio::sync::oneshot::channel::<()>();
        let serving = async move {
            let _ = serve_rx.await;
            Ok(())
        };

        tokio::time::timeout(
            Duration::from_millis(200),
            serve_until_drained(
                serving,
                async { "test" },
                lifecycle.clone(),
                Duration::from_millis(10),
                shutdown_tx,
            ),
        )
        .await
        .expect("drain deadline must end the serving wait")
        .unwrap();

        assert_eq!(lifecycle.phase(), Phase::Draining);
        assert!(*shutdown_rx.borrow());
        assert!(serve_tx.is_closed());
    }

    fn tracked_stream_router(
        counters: Arc<RequestCounters>,
        rx: tokio::sync::mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>,
    ) -> Router {
        let rx = Arc::new(std::sync::Mutex::new(Some(rx)));
        Router::new()
            .route(
                "/",
                get(move || {
                    let rx = rx.clone();
                    async move {
                        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(
                            rx.lock().unwrap().take().unwrap(),
                        ))
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(
                RequestClass::Inference(counters),
                track_requests,
            ))
    }

    #[tokio::test]
    async fn request_counter_stays_active_until_stream_eof() {
        let counters = Arc::new(RequestCounters::default());
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let response = tracked_stream_router(counters.clone(), rx)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(counters.inference.load(Ordering::Relaxed), 1);

        tx.send(Ok(bytes::Bytes::from_static(b"data: done\n\n")))
            .await
            .unwrap();
        drop(tx);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(b"data: done\n\n")
        );
        assert_eq!(counters.inference.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn request_counter_decrements_when_stream_body_is_dropped() {
        let counters = Arc::new(RequestCounters::default());
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let response = tracked_stream_router(counters.clone(), rx)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(counters.inference.load(Ordering::Relaxed), 1);
        drop(response);
        assert_eq!(counters.inference.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn tracked_response_preserves_size_hint_and_trailers() {
        let counters = Arc::new(RequestCounters::default());
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    Body::new(Body::from("ok").with_trailers(async {
                        let mut trailers = axum::http::HeaderMap::new();
                        trailers.insert("x-final-status", "complete".parse().unwrap());
                        Some(Ok(trailers))
                    }))
                }),
            )
            .layer(middleware::from_fn_with_state(
                RequestClass::Inference(counters.clone()),
                track_requests,
            ));
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut body = response.into_body();

        assert_eq!(body.size_hint().exact(), Some(2));
        assert_eq!(counters.inference.load(Ordering::Relaxed), 1);
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            bytes::Bytes::from_static(b"ok")
        );
        assert_eq!(
            body.frame()
                .await
                .unwrap()
                .unwrap()
                .into_trailers()
                .unwrap()["x-final-status"],
            "complete"
        );
        assert!(body.frame().await.is_none());
        assert_eq!(counters.inference.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn request_counter_decrements_when_handler_is_cancelled() {
        let counters = Arc::new(RequestCounters::default());
        let app = Router::new()
            .route("/", get(|| async { std::future::pending::<()>().await }))
            .layer(middleware::from_fn_with_state(
                RequestClass::Inference(counters.clone()),
                track_requests,
            ));
        let task = tokio::spawn(async move {
            app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(counters.inference.load(Ordering::Relaxed), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(counters.inference.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn streaming_response_keeps_drain_waiting_until_body_ends() {
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");
        let guard = lifecycle.enter().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
        let response = (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
        )
            .into_response();
        let response = hold_inflight_until_body_end(response, guard);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(lifecycle.inflight(), 1);

        lifecycle.drain("test");
        let waiter = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move { lifecycle.wait_for_zero().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        tx.send(Ok(bytes::Bytes::from_static(b"data: done\n\n")))
            .await
            .unwrap();
        drop(tx);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(
            body.next().await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"data: done\n\n")
        );
        assert!(body.next().await.is_none());
        waiter.await.unwrap();
        assert_eq!(lifecycle.inflight(), 0);
    }

    struct PendingSse {
        first: Option<bytes::Bytes>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Stream for PendingSse {
        type Item = Result<bytes::Bytes, std::io::Error>;

        fn poll_next(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            match this.first.take() {
                Some(first) => Poll::Ready(Some(Ok(first))),
                None => Poll::Pending,
            }
        }
    }

    impl Drop for PendingSse {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    async fn messages_test_state(upstream_url: String) -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.database.url = "sqlite::memory:".into();
        cfg.object_storage.root = dir.path().join("objects").to_string_lossy().into_owned();
        cfg.queues.accounting_journal = dir
            .path()
            .join("accounting.jsonl")
            .to_string_lossy()
            .into_owned();
        let db = Database::connect(&cfg.database).await.unwrap();
        db.migrate().await.unwrap();
        let objects = ObjectStorage::from_config(&cfg.object_storage)
            .await
            .unwrap();
        let deferred = DeferredQueues::start(&cfg, db.background(), objects.clone())
            .await
            .unwrap();
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");
        let counters = Arc::new(RequestCounters::default());
        let state = Arc::new(AppState {
            lifecycle,
            deferred,
            _db: db,
            _objects: objects,
            counters,
            instance_id: "test".into(),
            generation: "test".into(),
            started_at: "test".into(),
            inference_bind: "127.0.0.1:0".into(),
            management_bind: "127.0.0.1:0".into(),
            models: models::resolve_catalogue(&[]).into(),
            openai_upstream: None,
            anthropic_upstream: Some(
                AnthropicUpstream::new(upstream_url, Duration::from_secs(2)).unwrap(),
            ),
        });
        (state, dir)
    }

    #[tokio::test]
    async fn messages_route_disconnect_cancels_upstream_and_releases_guards() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let dropped_tx = Arc::new(std::sync::Mutex::new(Some(dropped_tx)));
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let upstream_server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/v1/messages",
                    post(move || {
                        let dropped = dropped_tx.lock().unwrap().take().unwrap();
                        async move {
                            (
                                [("content-type", "text/event-stream")],
                                Body::from_stream(PendingSse {
                                    first: Some(bytes::Bytes::from_static(
                                        b"event: message_start\ndata: {}\n\n",
                                    )),
                                    dropped: Some(dropped),
                                }),
                            )
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let (state, _dir) = messages_test_state(format!("http://{addr}")).await;
        let counters = state.counters.clone();
        let lifecycle = state.lifecycle.clone();
        let response = inference_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"model":"m","max_tokens":1,"messages":[{"role":"user"}],"stream":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(counters.inference.load(Ordering::Relaxed), 1);
        assert_eq!(lifecycle.inflight(), 1);
        let mut body = response.into_body();
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            bytes::Bytes::from_static(b"event: message_start\ndata: {}\n\n")
        );

        drop(body);

        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .expect("client disconnect did not abort upstream")
            .unwrap();
        assert_eq!(counters.inference.load(Ordering::Relaxed), 0);
        assert_eq!(lifecycle.inflight(), 0);
        upstream_server.abort();
        let _ = upstream_server.await;
    }

    #[tokio::test]
    async fn streaming_response_keeps_drain_waiting_until_body_ends() {
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");
        let guard = lifecycle.enter().unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(1);
        let response = (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
        )
            .into_response();
        let response = hold_inflight_until_body_end(response, guard);
        assert_eq!(lifecycle.inflight(), 1);

        lifecycle.drain("test");
        let waiter = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move { lifecycle.wait_for_zero().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        tx.send(Ok(bytes::Bytes::from_static(b"data: done\n\n")))
            .await
            .unwrap();
        drop(tx);
        let mut body = response.into_body().into_data_stream();
        assert_eq!(
            body.next().await.unwrap().unwrap(),
            bytes::Bytes::from_static(b"data: done\n\n")
        );
        assert!(body.next().await.is_none());
        waiter.await.unwrap();
        assert_eq!(lifecycle.inflight(), 0);
    }

    #[test]
    fn dropping_streaming_response_releases_inflight_guard() {
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");
        let guard = lifecycle.enter().unwrap();
        let response = hold_inflight_until_body_end(
            Body::from_stream(tokio_stream::pending::<Result<bytes::Bytes, std::io::Error>>())
                .into_response(),
            guard,
        );
        assert_eq!(lifecycle.inflight(), 1);

        drop(response);

        assert_eq!(lifecycle.inflight(), 0);
    }

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
