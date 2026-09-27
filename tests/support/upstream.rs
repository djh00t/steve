use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::HeaderValue,
    response::Response,
    routing::post,
    Router,
};
use bytes::Bytes;
use serde_json::Value;
use std::{
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch, Mutex},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

#[allow(dead_code)]
#[derive(Clone)]
pub enum Tail {
    Bytes(Bytes),
    Error(String),
}

struct UpstreamState {
    request_tx: watch::Sender<Option<Value>>,
    request_count: Arc<AtomicUsize>,
    first: Bytes,
    tail: Tail,
    release_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    tail_sent: Arc<AtomicBool>,
    body_drop_tx: watch::Sender<bool>,
}

pub struct ControlledUpstream {
    addr: SocketAddr,
    request_rx: watch::Receiver<Option<Value>>,
    request_count: Arc<AtomicUsize>,
    release_tx: Option<oneshot::Sender<()>>,
    tail_sent: Arc<AtomicBool>,
    body_drop_rx: watch::Receiver<bool>,
    server: Option<JoinHandle<()>>,
}

impl ControlledUpstream {
    pub async fn start(path: &str, first: impl Into<Bytes>, tail: Tail) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (request_tx, request_rx) = watch::channel(None);
        let request_count = Arc::new(AtomicUsize::new(0));
        let tail_sent = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = oneshot::channel();
        let (body_drop_tx, body_drop_rx) = watch::channel(false);
        let state = UpstreamState {
            request_tx,
            request_count: request_count.clone(),
            first: first.into(),
            tail,
            release_rx: Arc::new(Mutex::new(Some(release_rx))),
            tail_sent: tail_sent.clone(),
            body_drop_tx,
        };
        let app = Router::new()
            .route(path, post(serve_request))
            .with_state(Arc::new(state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        Ok(Self {
            addr,
            request_rx,
            request_count,
            release_tx: Some(release_tx),
            tail_sent,
            body_drop_rx,
            server: Some(server),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn wait_for_request(&mut self, timeout: Duration) -> Result<Value, String> {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(request) = self.request_rx.borrow_and_update().clone() {
                    return Ok(request);
                }
                self.request_rx
                    .changed()
                    .await
                    .map_err(|_| "request channel closed".to_owned())?;
            }
        })
        .await
        .map_err(|_| "timed out waiting for upstream request".to_owned())?
    }

    pub fn release_tail(&mut self) {
        if let Some(release) = self.release_tx.take() {
            let _ = release.send(());
        }
    }

    pub fn tail_was_sent(&self) -> bool {
        self.tail_sent.load(Ordering::SeqCst)
    }

    pub async fn wait_for_body_drop(&mut self, timeout: Duration) -> Result<(), String> {
        tokio::time::timeout(timeout, async {
            loop {
                if *self.body_drop_rx.borrow_and_update() {
                    return Ok(());
                }
                self.body_drop_rx
                    .changed()
                    .await
                    .map_err(|_| "body-drop channel closed".to_owned())?;
            }
        })
        .await
        .map_err(|_| "timed out waiting for upstream body drop".to_owned())?
    }

    pub fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }
}

impl Drop for ControlledUpstream {
    fn drop(&mut self) {
        self.release_tx.take();
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

async fn serve_request(State(state): State<Arc<UpstreamState>>, body: Body) -> Response {
    let request = match to_bytes(body, usize::MAX).await {
        Ok(body) => serde_json::from_slice(&body).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    state.request_count.fetch_add(1, Ordering::SeqCst);
    let _ = state.request_tx.send(Some(request));

    let (body_tx, body_rx) = mpsc::channel(2);
    if body_tx
        .send(Ok::<_, io::Error>(state.first.clone()))
        .await
        .is_err()
    {
        return Response::new(Body::empty());
    }
    let release_rx = state.release_rx.lock().await.take();
    let tail = state.tail.clone();
    let tail_sent = state.tail_sent.clone();
    tokio::spawn(async move {
        let Some(release_rx) = release_rx else {
            return;
        };
        tokio::select! {
            result = release_rx => if result.is_err() { return; },
            _ = body_tx.closed() => return,
        }
        let result = match tail {
            Tail::Bytes(bytes) => body_tx.send(Ok(bytes)).await,
            Tail::Error(message) => body_tx.send(Err(io::Error::other(message))).await,
        };
        if result.is_ok() {
            tail_sent.store(true, Ordering::SeqCst);
        }
    });

    let body_drop_guard = BodyDropGuard(state.body_drop_tx.clone());
    let stream = ReceiverStream::new(body_rx).map(move |item| {
        let _ = &body_drop_guard;
        item
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

struct BodyDropGuard(watch::Sender<bool>);

impl Drop for BodyDropGuard {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}
