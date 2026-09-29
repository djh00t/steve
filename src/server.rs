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
    future::{self, Future},
    net::SocketAddr,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};
use tokio::{
    signal,
    sync::{mpsc, watch, Semaphore},
};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Default)]
struct RequestCounters {
    inference: AtomicU64,
    management: AtomicU64,
}

struct AdmissionBudget {
    semaphore: Arc<Semaphore>,
    limit: u32,
    rejected_total: AtomicU64,
}

impl AdmissionBudget {
    fn new(limit: u32) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(limit as usize)),
            limit,
            rejected_total: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> AdmissionCounters {
        AdmissionCounters {
            limit: self.limit,
            active: self.limit - self.semaphore.available_permits() as u32,
            rejected_total: self.rejected_total.load(Ordering::Relaxed),
        }
    }

    fn try_acquire(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                let _ = self.rejected_total.fetch_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |total| Some(total.saturating_add(1)),
                );
                None
            }
        }
    }
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
    inference_admission: Arc<AdmissionBudget>,
    management_admission: Arc<AdmissionBudget>,
    openai_upstream: Option<OpenAiUpstream>,
    anthropic_upstream: Option<AnthropicUpstream>,
    provider_probe: ProviderProbeState,
    shutdown: mpsc::Sender<ShutdownRequest>,
    drain_timeout: Duration,
    active_work_cancelled: watch::Receiver<bool>,
}

#[derive(Clone, Copy)]
struct ShutdownRequest {
    reason: &'static str,
    deadline: Instant,
}

impl ShutdownRequest {
    fn new(reason: &'static str, timeout: Duration) -> Result<Self> {
        Ok(Self {
            reason,
            deadline: Instant::now()
                .checked_add(timeout)
                .context("shutdown deadline is out of range")?,
        })
    }
}

struct ShutdownOutcome {
    request: ShutdownRequest,
    result: Result<()>,
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
    #[cfg(test)]
    fn new(
        openai_url: Option<String>,
        anthropic_url: Option<String>,
    ) -> std::result::Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self::with_client(openai_url, anthropic_url, client))
    }

    fn with_client(
        openai_url: Option<String>,
        anthropic_url: Option<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            client,
            probe_gate: Arc::new(Semaphore::new(1)),
            openai_url,
            anthropic_url,
        }
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
    admission: Admission,
    accounting_incident: Value,
}

#[derive(Serialize)]
struct Admission {
    inference: AdmissionCounters,
    management: AdmissionCounters,
}

#[derive(Serialize)]
struct AdmissionCounters {
    limit: u32,
    active: u32,
    rejected_total: u64,
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
    #[serde(default)]
    hold_response_ms: Option<u64>,
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
    let deferred_shutdown = deferred.clone();
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

    if inference_addr == management_addr && inference_addr.port() != 0 {
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
    let certificates = if let Some(path) = &cfg.server.upstream_ca_bundle {
        let pem = std::fs::read(path)
            .with_context(|| format!("reading upstream CA bundle {}", path.display()))?;
        let mut certificates = Vec::new();
        let mut start = None;
        let mut offset = 0;
        for line in pem.split_inclusive(|byte| *byte == b'\n') {
            let content = line.strip_suffix(b"\n").unwrap_or(line);
            let content = content.strip_suffix(b"\r").unwrap_or(content);
            match (start, content) {
                (None, b"") => {}
                (None, b"-----BEGIN CERTIFICATE-----") => start = Some(offset),
                (Some(begin), b"-----END CERTIFICATE-----") => {
                    certificates.push(
                        reqwest::Certificate::from_pem(&pem[begin..offset + line.len()])
                            .with_context(|| {
                                format!("parsing upstream CA bundle {}", path.display())
                            })?,
                    );
                    start = None;
                }
                (Some(_), content)
                    if content.iter().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(*byte, b'+' | b'/' | b'=')
                    }) => {}
                _ => anyhow::bail!(
                    "parsing upstream CA bundle {}: unexpected PEM content",
                    path.display()
                ),
            }
            offset += line.len();
        }
        if start.is_some() {
            anyhow::bail!(
                "parsing upstream CA bundle {}: incomplete certificate",
                path.display()
            );
        }
        if certificates.is_empty() {
            anyhow::bail!("empty upstream CA bundle {}", path.display());
        }
        certificates
    } else {
        Vec::new()
    };
    let mut normal_builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    let mut anthropic_builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(0);
    for certificate in certificates {
        normal_builder = normal_builder.add_root_certificate(certificate.clone());
        anthropic_builder = anthropic_builder.add_root_certificate(certificate);
    }
    let ca_source = cfg
        .server
        .upstream_ca_bundle
        .as_ref()
        .map(|path| format!(" with CA bundle {}", path.display()))
        .unwrap_or_default();
    let normal_http = normal_builder
        .build()
        .with_context(|| format!("building upstream HTTP client{ca_source}"))?;
    let anthropic_http = anthropic_builder
        .build()
        .with_context(|| format!("building Anthropic HTTP client{ca_source}"))?;
    let openai_upstream = cfg
        .server
        .openai_upstream_url
        .as_ref()
        .map(|url| OpenAiUpstream::with_client(url, Duration::from_secs(30), normal_http.clone()))
        .transpose()?;
    let anthropic_upstream = cfg
        .server
        .anthropic_upstream_url
        .as_ref()
        .map(|url| AnthropicUpstream::with_client(url, Duration::from_secs(30), anthropic_http))
        .transpose()?;
    let provider_probe = ProviderProbeState::with_client(
        cfg.server.openai_upstream_url.clone(),
        cfg.server.anthropic_upstream_url.clone(),
        normal_http,
    );
    let timeout = Duration::from_secs(cfg.server.drain_timeout_seconds);
    let (shutdown, shutdown_requests) = mpsc::channel(1);
    let (active_work_cancel, active_work_cancelled) = watch::channel(false);
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
        inference_admission: Arc::new(AdmissionBudget::new(32)),
        management_admission: Arc::new(AdmissionBudget::new(4)),
        openai_upstream,
        anthropic_upstream,
        provider_probe,
        shutdown: shutdown.clone(),
        drain_timeout: timeout,
        active_work_cancelled,
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

    let (inference_shutdown_tx, inference_shutdown_rx) = watch::channel(false);
    let mut inference = tokio::spawn(async move {
        axum::serve(inference_listener, inference_app)
            .with_graceful_shutdown(wait_for_shutdown(inference_shutdown_rx))
            .await
    });
    let inference_abort = inference.abort_handle();
    let mut management =
        tokio::spawn(async move { axum::serve(management_listener, management_app).await });
    let management_abort = management.abort_handle();
    let shutdown_lifecycle = lifecycle.clone();
    let mut coordinator = tokio::spawn(async move {
        coordinate_shutdown(
            shutdown_requests,
            shutdown_lifecycle,
            deferred_shutdown,
            timeout,
            inference_shutdown_tx,
            inference_abort.clone(),
            active_work_cancel,
        )
        .await
    });

    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut coordinator_outcome = None;
    let mut inference_result = None;
    let mut management_result = None;
    let mut server_failure = None;
    let exit_request = loop {
        tokio::select! {
            reason = &mut signal => {
                break ShutdownRequest::new(reason, timeout)?;
            }
            outcome = &mut coordinator, if coordinator_outcome.is_none() => {
                coordinator_outcome = Some(outcome.map_err(|error| {
                    anyhow::anyhow!("shutdown coordinator failed: {error}")
                })?);
            }
            result = &mut inference, if inference_result.is_none() => {
                let result = server_result(result);
                let unexpected = lifecycle.phase() != Phase::Draining || result.is_err();
                if unexpected {
                    server_failure = Some(result.err().map_or_else(
                        || anyhow::anyhow!("inference server exited before shutdown"),
                        anyhow::Error::from,
                    ));
                    inference_result = Some(Ok(()));
                    break ShutdownRequest::new("inference_server_exit", timeout)?;
                }
                inference_result = Some(result);
            }
            result = &mut management, if management_result.is_none() => {
                let result = server_result(result);
                server_failure = Some(result.err().map_or_else(
                    || anyhow::anyhow!("management server exited before shutdown"),
                    anyhow::Error::from,
                ));
                management_result = Some(Ok(()));
                break ShutdownRequest::new("management_server_exit", timeout)?;
            }
        }
    };
    lifecycle.drain(exit_request.reason);
    let _ = shutdown.try_send(exit_request);
    let outcome = match coordinator_outcome {
        Some(outcome) => outcome,
        None => coordinator
            .await
            .map_err(|error| anyhow::anyhow!("shutdown coordinator failed: {error}"))?,
    };
    let listener_deadline = if outcome.request.reason == "management_api" {
        exit_request.deadline
    } else {
        outcome.request.deadline
    };
    management_abort.abort();
    if outcome.result.is_err() || Instant::now() >= listener_deadline {
        inference.abort();
    }
    let inference_result = match inference_result {
        Some(result) => result,
        None => join_server_until(&mut inference, listener_deadline, "inference").await,
    };
    let management_result = match management_result {
        Some(result) => result,
        None => join_server_until(&mut management, listener_deadline, "management").await,
    };
    lifecycle.stopped("listeners_exited");
    if let Some(error) = server_failure {
        return Err(error);
    }
    outcome.result?;
    inference_result?;
    management_result?;
    Ok(())
}

async fn coordinate_shutdown(
    mut requests: mpsc::Receiver<ShutdownRequest>,
    lifecycle: Lifecycle,
    deferred: DeferredQueues,
    timeout: Duration,
    inference_shutdown: watch::Sender<bool>,
    inference_abort: tokio::task::AbortHandle,
    active_work_cancel: watch::Sender<bool>,
) -> ShutdownOutcome {
    let request = match requests
        .recv()
        .await
        .context("shutdown request channel closed")
    {
        Ok(request) => request,
        Err(error) => {
            return ShutdownOutcome {
                request: ShutdownRequest {
                    reason: "shutdown_channel_closed",
                    deadline: Instant::now(),
                },
                result: Err(error),
            }
        }
    };
    let deadline = request.deadline;
    lifecycle.drain(request.reason);

    if let Err(error) = deferred.prepare_shutdown(deadline).await {
        cancel_active_work(&active_work_cancel, &lifecycle).await;
        inference_abort.abort();
        tokio::task::yield_now().await;
        let _ = deferred.shutdown(deadline).await;
        return ShutdownOutcome {
            request,
            result: Err(error),
        };
    }
    if let Err(error) = deferred.mark_unclean(deadline).await {
        cancel_active_work(&active_work_cancel, &lifecycle).await;
        inference_abort.abort();
        tokio::task::yield_now().await;
        let _ = deferred.shutdown(deadline).await;
        return ShutdownOutcome {
            request,
            result: Err(error),
        };
    }

    let remaining = deadline.saturating_duration_since(Instant::now());
    let timed_out = remaining.is_zero()
        || tokio::time::timeout(remaining, lifecycle.wait_for_zero())
            .await
            .is_err();
    let inflight_at_deadline = lifecycle.inflight();
    if timed_out {
        tracing::warn!(
            event = "drain_timeout",
            timeout_seconds = timeout.as_secs(),
            inflight = inflight_at_deadline,
            "drain deadline reached"
        );
        cancel_active_work(&active_work_cancel, &lifecycle).await;
        inference_abort.abort();
        tokio::task::yield_now().await;
    } else {
        let _ = inference_shutdown.send(true);
    }

    let queues = deferred.shutdown(deadline).await;
    let result = if timed_out {
        Err(anyhow::anyhow!(
            "drain deadline reached with {} request(s) in flight",
            inflight_at_deadline
        ))
    } else {
        queues
    };
    ShutdownOutcome { request, result }
}

async fn cancel_active_work(cancel: &watch::Sender<bool>, lifecycle: &Lifecycle) {
    cancel.send_replace(true);
    lifecycle.wait_for_zero().await;
}

fn server_result(
    result: std::result::Result<std::io::Result<()>, tokio::task::JoinError>,
) -> std::io::Result<()> {
    match result {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(std::io::Error::other(format!(
            "server task failed: {error}"
        ))),
    }
}

async fn join_server_until(
    server: &mut tokio::task::JoinHandle<std::io::Result<()>>,
    deadline: Instant,
    name: &str,
) -> std::io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        server.abort();
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("timed out stopping {name} server"),
        ));
    }
    match tokio::time::timeout(remaining, &mut *server).await {
        Ok(result) => server_result(result),
        Err(_) => {
            server.abort();
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out stopping {name} server"),
            ))
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
            state.clone(),
            admit_inference,
        ))
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
            state.clone(),
            admit_management,
        ))
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
        admission: admission(&state),
        accounting_incident: state.deferred.accounting_incident(),
    })
}

async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let accounting_incident = state.deferred.accounting_incident();
    let incident_ready = !matches!(
        accounting_incident["state"].as_str(),
        Some("blocked" | "unreconciled")
    );
    let is_ready = state.lifecycle.is_ready() && incident_ready;
    let health = Health {
        status: if is_ready { "ready" } else { "not_ready" },
        phase: state.lifecycle.phase(),
        inflight: state.lifecycle.inflight(),
        admission: admission(&state),
        accounting_incident,
    };

    if is_ready {
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
        "admission": admission(&state),
        "inference_bind": state.inference_bind,
        "management_bind": state.management_bind,
        "queues": state.deferred.snapshot(),
        "accounting_incident": state.deferred.accounting_incident(),
    }))
}

fn admission(state: &AppState) -> Admission {
    Admission {
        inference: state.inference_admission.snapshot(),
        management: state.management_admission.snapshot(),
    }
}

async fn drain(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.lifecycle.drain("management_api");
    if let Ok(request) = ShutdownRequest::new("management_api", state.drain_timeout) {
        let _ = state.shutdown.try_send(request);
    }
    Json(json!({"phase": "draining"}))
}

async fn messages(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let reply = if let Some(upstream) = &state.anthropic_upstream {
        anthropic_messages::handle_messages_with_upstream(&body, upstream).await
    } else {
        anthropic_messages::handle_messages(&body)
    };
    reply.into_response()
}

async fn responses(State(state): State<Arc<AppState>>, body: bytes::Bytes) -> Response {
    let reply = if let Some(upstream) = &state.openai_upstream {
        openai_responses::handle_responses_with_upstream(&body, upstream).await
    } else {
        openai_responses::handle_responses(&body)
    };
    reply.into_response()
}

fn hold_inflight_until_body_end(response: Response, guard: InflightGuard) -> Response {
    hold_guard_until_body_end(response, guard)
}

fn cancel_body_on_forced_shutdown(
    response: Response,
    cancelled: watch::Receiver<bool>,
) -> Response {
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(CancelledBody {
            inner: Some(body),
            cancelled: active_work_cancelled(cancelled),
        }),
    )
}

fn active_work_cancelled(
    mut cancelled: watch::Receiver<bool>,
) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        while !*cancelled.borrow_and_update() {
            if cancelled.changed().await.is_err() {
                future::pending::<()>().await;
            }
        }
    })
}

struct CancelledBody {
    inner: Option<Body>,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl HttpBody for CancelledBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.cancelled.as_mut().poll(cx).is_ready() {
            this.inner.take();
            return Poll::Ready(None);
        }
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        let result = Pin::new(inner).poll_frame(cx);
        if matches!(&result, Poll::Ready(None | Some(Err(_)))) {
            this.inner.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().is_none_or(HttpBody::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        self.inner
            .as_ref()
            .map_or_else(SizeHint::default, HttpBody::size_hint)
    }
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
    let reply = if let Some(upstream) = &state.openai_upstream {
        let deferred = state.deferred.clone();
        openai_chat::handle_chat_completions_with_upstream_and_terminal(
            &body,
            upstream,
            Arc::new(move |request, model, attempt| {
                offer_chat_terminal_event(&deferred, request, model, attempt);
            }),
        )
        .await
    } else {
        openai_chat::handle_chat_completions(&body)
    };
    let attempt_count = reply
        .request
        .as_ref()
        .map_or(0, |request| request.attempts.len());
    for attempt in &reply.attempts {
        if let (Some(request), Some(model), Some(finished_at)) = (
            reply.request.as_ref(),
            reply.model.as_ref(),
            attempt.finished_at.as_ref(),
        ) {
            tracing::info!(
                request_id = %attempt.request_id.0,
                attempt_id = %attempt.id.0,
                attempt_count,
                status = ?attempt.status,
                finished_at = ?finished_at,
                "chat completions upstream attempt finished"
            );
            if !reply.requested_stream {
                offer_chat_terminal_event(&state.deferred, request, model, attempt);
            }
        }
    }
    reply.into_response()
}

fn offer_chat_terminal_event(
    deferred: &DeferredQueues,
    request: &crate::proxy::Request,
    model: &str,
    attempt: &crate::proxy::RequestAttempt,
) {
    let provider = (!attempt.provider.is_empty() && attempt.provider != "unassigned")
        .then_some(attempt.provider.as_str());
    let account = (!attempt.account.is_empty() && attempt.account != "unassigned")
        .then_some(attempt.account.as_str());
    deferred.accounting(
        "chat.attempt.terminal.v1",
        json!({
            "request_id": request.id,
            "attempt_id": attempt.id,
            "request_created_at": request.created_at,
            "model": model,
            "provider": provider,
            "account": account,
            "status": attempt.status,
            "started_at": attempt.started_at,
            "finished_at": attempt.finished_at,
        }),
    );
}
async fn echo(State(state): State<Arc<AppState>>, Json(req): Json<EchoRequest>) -> Response {
    let payload = json!({"value": req.value});
    state.deferred.accounting("test.echo", payload.clone());
    state.deferred.telemetry("test.echo", payload.clone());
    state.deferred.history(
        format!("test/{}.json", Uuid::now_v7()),
        bytes::Bytes::from(payload.to_string()),
    );
    let response = (StatusCode::OK, Json(payload.clone())).into_response();
    let response = if let Some(delay_ms) = req.hold_response_ms {
        let (parts, _) = response.into_parts();
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            let _ = sender
                .send(Ok::<_, std::convert::Infallible>(bytes::Bytes::from(
                    payload.to_string(),
                )))
                .await;
        });
        Response::from_parts(
            parts,
            Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver)),
        )
    } else {
        response
    };
    response
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

async fn admit_inference(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let exempt = request.method() == axum::http::Method::GET
        && matches!(request.uri().path(), "/health/live" | "/health/ready");
    if exempt {
        return next.run(request).await;
    }
    if state.lifecycle.phase() == Phase::Draining {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    }

    let permit = match state
        .deferred
        .with_accounting_admission(|| state.inference_admission.try_acquire())
    {
        Err(incident) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [
                    ("content-type", "application/json"),
                    ("cache-control", "no-store"),
                    ("retry-after", "1"),
                ],
                Json(json!({"error":{
                    "type":"unavailable",
                    "code":"accounting_incident",
                    "message":"inference admission stopped by an unresolved accounting incident",
                    "incident_id":incident["incident_id"],
                    "state":incident["state"],
                }})),
            )
                .into_response();
        }
        Ok(permit) => permit,
    };
    let Some(permit) = permit else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                ("content-type", "application/json"),
                ("cache-control", "no-store"),
                ("retry-after", "1"),
            ],
            r#"{"error":{"type":"overloaded","code":"admission_limit","message":"inference capacity exhausted"}}"#,
        )
            .into_response();
    };
    let Some(inflight) = state.lifecycle.enter() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "draining"})),
        )
            .into_response();
    };

    let mut cancelled = active_work_cancelled(state.active_work_cancelled.clone());
    let response = tokio::select! {
        biased;
        _ = &mut cancelled => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "draining"})),
            ).into_response();
        }
        response = next.run(request) => response,
    };
    cancel_body_on_forced_shutdown(
        hold_inflight_until_body_end(hold_guard_until_body_end(response, permit), inflight),
        state.active_work_cancelled.clone(),
    )
}

async fn admit_management(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let exempt = request.method() == axum::http::Method::GET
        && matches!(request.uri().path(), "/health/live" | "/health/ready");
    if exempt {
        return next.run(request).await;
    }

    let Some(permit) = state.management_admission.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                ("content-type", "application/json"),
                ("cache-control", "no-store"),
                ("retry-after", "1"),
            ],
            r#"{"error":{"type":"overloaded","code":"admission_limit","message":"management capacity exhausted"}}"#,
        )
            .into_response();
    };

    hold_guard_until_body_end(next.run(request).await, permit)
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
    use crate::{accounting::AccountingCoordinator, test_upstream};
    use axum::{body::HttpBody, http::Request};
    use http_body_util::BodyExt;
    use std::future::IntoFuture;
    use tokio_stream::{Stream, StreamExt};
    use tower::ServiceExt;

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
        cfg.database.url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("steve.db").display()
        );
        cfg.object_storage.root = dir.path().join("objects").to_string_lossy().into_owned();
        let accounting_root = dir.path().join("accounting");
        AccountingCoordinator::provision(&accounting_root).unwrap();
        cfg.queues.accounting_journal = accounting_root.to_string_lossy().into_owned();
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
        let (shutdown, _shutdown_requests) = mpsc::channel(1);
        let (_active_work_cancel, active_work_cancelled) = watch::channel(false);
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
            inference_admission: Arc::new(AdmissionBudget::new(32)),
            management_admission: Arc::new(AdmissionBudget::new(4)),
            openai_upstream: None,
            anthropic_upstream: Some(
                AnthropicUpstream::new(upstream_url, Duration::from_secs(2)).unwrap(),
            ),
            provider_probe: ProviderProbeState::new(None, None).unwrap(),
            shutdown,
            drain_timeout: Duration::from_secs(60),
            active_work_cancelled,
        });
        (state, dir)
    }

    #[tokio::test]
    async fn inference_admission_holds_permit_until_body_end() {
        let (state, _dir) = messages_test_state("http://127.0.0.1:1".into()).await;
        let lifecycle = state.lifecycle.clone();
        let app = inference_router(state.clone());
        let management = management_router(state.clone());
        let idle_status = management
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let idle_status: Value =
            serde_json::from_slice(&idle_status.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(idle_status["admission"]["inference"]["limit"], 32);
        assert_eq!(idle_status["admission"]["inference"]["active"], 0);
        assert_eq!(idle_status["admission"]["inference"]["rejected_total"], 0);
        assert_eq!(idle_status["admission"]["management"]["limit"], 4);
        assert_eq!(idle_status["admission"]["management"]["active"], 1);
        assert_eq!(idle_status["admission"]["management"]["rejected_total"], 0);
        for router in [&app, &management] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/health/live")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let health: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(health["admission"]["inference"]["limit"], 32);
            assert_eq!(health["admission"]["inference"]["active"], 0);
            assert_eq!(health["admission"]["inference"]["rejected_total"], 0);
            assert_eq!(health["admission"]["management"]["limit"], 4);
            assert_eq!(health["admission"]["management"]["active"], 0);
            assert_eq!(health["admission"]["management"]["rejected_total"], 0);
        }
        let request = || {
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap()
        };
        let mut held = Vec::new();
        for _ in 0..32 {
            let response = app.clone().oneshot(request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            held.push(response);
        }

        let overloaded = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(overloaded.headers()["content-type"], "application/json");
        assert_eq!(overloaded.headers()["cache-control"], "no-store");
        assert_eq!(overloaded.headers()["retry-after"], "1");
        assert_eq!(
            overloaded.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(
                br#"{"error":{"type":"overloaded","code":"admission_limit","message":"inference capacity exhausted"}}"#
            )
        );
        assert_eq!(
            state
                .inference_admission
                .rejected_total
                .load(Ordering::Relaxed),
            1
        );

        let mut management_held = Vec::new();
        for _ in 0..4 {
            management_held.push(
                management
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/api/v1/system/status")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap(),
            );
        }
        let management_rejected = management
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            management_rejected.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state
                .management_admission
                .rejected_total
                .load(Ordering::Relaxed),
            1
        );

        for router in [&app, &management] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/health/ready")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let health: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            assert_eq!(health["admission"]["inference"]["limit"], 32);
            assert_eq!(health["admission"]["inference"]["active"], 32);
            assert_eq!(health["admission"]["inference"]["rejected_total"], 1);
            assert_eq!(health["admission"]["management"]["limit"], 4);
            assert_eq!(health["admission"]["management"]["active"], 4);
            assert_eq!(health["admission"]["management"]["rejected_total"], 1);
        }
        let held_status = management
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(held_status.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(management_held);

        let held_status = management
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let held_status: Value =
            serde_json::from_slice(&held_status.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(held_status["admission"]["inference"]["active"], 32);
        assert_eq!(held_status["admission"]["inference"]["rejected_total"], 1);
        assert_eq!(held_status["admission"]["management"]["active"], 1);
        assert_eq!(held_status["admission"]["management"]["rejected_total"], 2);

        state
            .inference_admission
            .rejected_total
            .store(u64::MAX, Ordering::Relaxed);
        let _saturated_rejection = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(
            state
                .inference_admission
                .rejected_total
                .load(Ordering::Relaxed),
            u64::MAX
        );

        for path in ["/health/live", "/health/ready"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.into_body().collect().await.unwrap();
        }

        drop(held.pop());
        let resumed = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(resumed.status(), StatusCode::OK);
        drop(resumed);
        held.push(app.clone().oneshot(request()).await.unwrap());

        lifecycle.drain("test");
        let draining = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(draining.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            draining.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(br#"{"error":"draining"}"#)
        );
        drop(held);
    }

    #[tokio::test]
    async fn management_admission_is_independent_and_exempts_health() {
        let (state, _dir) = messages_test_state("http://127.0.0.1:1".into()).await;
        let lifecycle = state.lifecycle.clone();
        let management = management_router(state.clone());
        let inference = inference_router(state.clone());
        let management_request = || {
            Request::builder()
                .uri("/api/v1/system/status")
                .body(Body::empty())
                .unwrap()
        };
        let mut held = Vec::new();
        for _ in 0..4 {
            let response = management
                .clone()
                .oneshot(management_request())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            held.push(response);
        }

        let overloaded = management
            .clone()
            .oneshot(management_request())
            .await
            .unwrap();
        assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(overloaded.headers()["content-type"], "application/json");
        assert_eq!(overloaded.headers()["cache-control"], "no-store");
        assert_eq!(overloaded.headers()["retry-after"], "1");
        assert_eq!(
            overloaded.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(
                br#"{"error":{"type":"overloaded","code":"admission_limit","message":"management capacity exhausted"}}"#
            )
        );

        let inference_response = inference
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(inference_response.status(), StatusCode::OK);
        inference_response.into_body().collect().await.unwrap();

        for path in ["/health/live", "/health/ready"] {
            let response = management
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            response.into_body().collect().await.unwrap();
        }

        drop(held.pop());
        let resumed = management
            .clone()
            .oneshot(management_request())
            .await
            .unwrap();
        assert_eq!(resumed.status(), StatusCode::OK);
        held.push(resumed);

        lifecycle.drain("test");
        let draining = management
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(draining.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            draining.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(
                br#"{"error":{"type":"overloaded","code":"admission_limit","message":"management capacity exhausted"}}"#
            )
        );
        drop(held);

        let _probe = state
            .provider_probe
            .probe_gate
            .clone()
            .try_acquire_owned()
            .unwrap();
        let busy_probe = management
            .oneshot(
                Request::builder()
                    .uri("/api/v1/providers/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(busy_probe.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            busy_probe.into_body().collect().await.unwrap().to_bytes(),
            bytes::Bytes::from_static(br#"{"status":"busy"}"#)
        );
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

    #[tokio::test]
    async fn server_task_failure_is_propagated_without_waiting_for_a_signal() {
        let mut server = tokio::spawn(async move {
            panic!("injected server failure");
            #[allow(unreachable_code)]
            Ok::<(), std::io::Error>(())
        });

        let error = join_server_until(&mut server, Instant::now() + Duration::from_secs(1), "test")
            .await
            .expect_err("server panic must be visible");

        assert!(error.to_string().contains("server task failed"), "{error}");
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
