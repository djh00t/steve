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
    sync::{mpsc, watch},
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
    request_tx: mpsc::Sender<Value>,
    request_count: Arc<AtomicUsize>,
    request_overflow: Arc<AtomicBool>,
    first: Bytes,
    tail: Tail,
    release_tx: watch::Sender<bool>,
    tail_sent: Arc<AtomicBool>,
    tail_count: Arc<AtomicUsize>,
    body_drop_tx: watch::Sender<bool>,
}

pub struct ControlledUpstream {
    addr: SocketAddr,
    request_rx: mpsc::Receiver<Value>,
    request_count: Arc<AtomicUsize>,
    request_overflow: Arc<AtomicBool>,
    release_tx: watch::Sender<bool>,
    tail_sent: Arc<AtomicBool>,
    tail_count: Arc<AtomicUsize>,
    body_drop_rx: watch::Receiver<bool>,
    server: Option<JoinHandle<()>>,
}

impl ControlledUpstream {
    pub async fn start(path: &str, first: impl Into<Bytes>, tail: Tail) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (request_tx, request_rx) = mpsc::channel(32);
        let request_count = Arc::new(AtomicUsize::new(0));
        let request_overflow = Arc::new(AtomicBool::new(false));
        let tail_sent = Arc::new(AtomicBool::new(false));
        let tail_count = Arc::new(AtomicUsize::new(0));
        let (release_tx, _) = watch::channel(false);
        let (body_drop_tx, body_drop_rx) = watch::channel(false);
        let state = UpstreamState {
            request_tx,
            request_count: request_count.clone(),
            request_overflow: request_overflow.clone(),
            first: first.into(),
            tail,
            release_tx: release_tx.clone(),
            tail_sent: tail_sent.clone(),
            tail_count: tail_count.clone(),
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
            request_overflow,
            release_tx,
            tail_sent,
            tail_count,
            body_drop_rx,
            server: Some(server),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn wait_for_request(&mut self, timeout: Duration) -> Result<Value, String> {
        self.wait_for_requests(1, timeout)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| "request channel closed".to_owned())
    }

    pub async fn wait_for_requests(
        &mut self,
        count: usize,
        timeout: Duration,
    ) -> Result<Vec<Value>, String> {
        if count > 32 {
            return Err(format!("cannot capture {count} requests; maximum is 32"));
        }
        tokio::time::timeout(timeout, async {
            let mut requests = Vec::with_capacity(count);
            while requests.len() < count {
                if self.request_overflow.load(Ordering::SeqCst) {
                    return Err(
                        "upstream request capture exceeded its 32-request capacity".to_owned()
                    );
                }
                match self.request_rx.recv().await {
                    Some(request) => requests.push(request),
                    None => return Err("request channel closed".to_owned()),
                }
            }
            if self.request_overflow.load(Ordering::SeqCst) {
                return Err("upstream request capture exceeded its 32-request capacity".to_owned());
            }
            Ok(requests)
        })
        .await
        .map_err(|_| "timed out waiting for upstream requests".to_owned())?
    }

    pub fn release_tail(&mut self) {
        self.release_tx.send_replace(true);
    }

    pub fn tail_was_sent(&self) -> bool {
        self.tail_sent.load(Ordering::SeqCst)
    }

    pub fn tail_count(&self) -> usize {
        self.tail_count.load(Ordering::SeqCst)
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
        self.release_tx.send_replace(true);
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
    if state.request_tx.try_send(request).is_err() {
        state.request_overflow.store(true, Ordering::SeqCst);
    }

    let (body_tx, body_rx) = mpsc::channel(2);
    if body_tx
        .send(Ok::<_, io::Error>(state.first.clone()))
        .await
        .is_err()
    {
        return Response::new(Body::empty());
    }
    let mut release_rx = state.release_tx.subscribe();
    let tail = state.tail.clone();
    let tail_sent = state.tail_sent.clone();
    let tail_count = state.tail_count.clone();
    tokio::spawn(async move {
        if !*release_rx.borrow() {
            tokio::select! {
                result = release_rx.changed() => if result.is_err() { return; },
                _ = body_tx.closed() => return,
            }
        }
        if body_tx.is_closed() {
            return;
        }
        let result = match tail {
            Tail::Bytes(bytes) => body_tx.send(Ok(bytes)).await,
            Tail::Error(message) => body_tx.send(Err(io::Error::other(message))).await,
        };
        if result.is_ok() {
            tail_sent.store(true, Ordering::SeqCst);
            tail_count.fetch_add(1, Ordering::SeqCst);
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
