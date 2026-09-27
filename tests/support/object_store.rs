use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use bytes::Bytes;
use std::{
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{oneshot, Mutex},
    task::JoinHandle,
};

const MAX_OBJECT_BYTES: usize = 16 * 1024;

pub struct CapturedPut {
    pub method: Method,
    pub path: String,
    pub body: Bytes,
}

struct EndpointState {
    put_tx: Mutex<Option<oneshot::Sender<CapturedPut>>>,
    release_rx: Mutex<Option<oneshot::Receiver<()>>>,
    completed_tx: Mutex<Option<oneshot::Sender<()>>>,
    request_count: Arc<AtomicUsize>,
}

pub struct HeldObjectStoreEndpoint {
    addr: SocketAddr,
    put_rx: Option<oneshot::Receiver<CapturedPut>>,
    release_tx: Option<oneshot::Sender<()>>,
    completed_rx: Option<oneshot::Receiver<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<()>>,
    request_count: Arc<AtomicUsize>,
}

impl HeldObjectStoreEndpoint {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (put_tx, put_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let (completed_tx, completed_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let request_count = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(EndpointState {
            put_tx: Mutex::new(Some(put_tx)),
            release_rx: Mutex::new(Some(release_rx)),
            completed_tx: Mutex::new(Some(completed_tx)),
            request_count: request_count.clone(),
        });
        let app = Router::new().fallback(any(put_object)).with_state(state);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Ok(Self {
            addr,
            put_rx: Some(put_rx),
            release_tx: Some(release_tx),
            completed_rx: Some(completed_rx),
            shutdown_tx: Some(shutdown_tx),
            server: Some(server),
            request_count,
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    pub async fn wait_for_put(&mut self, timeout: Duration) -> Result<CapturedPut, String> {
        let receiver = self
            .put_rx
            .take()
            .ok_or_else(|| "PutObject was already captured".to_owned())?;
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| "timed out waiting for PutObject".to_owned())?
            .map_err(|_| "object-store listener stopped before PutObject".to_owned())
    }

    pub fn release(&mut self) -> Result<(), String> {
        self.release_tx
            .take()
            .ok_or_else(|| "PutObject was already released".to_owned())?
            .send(())
            .map_err(|_| "PutObject handler stopped before release".to_owned())
    }

    pub async fn wait_for_completion(&mut self, timeout: Duration) -> Result<(), String> {
        let receiver = self
            .completed_rx
            .as_mut()
            .ok_or_else(|| "PutObject completion was already observed".to_owned())?;
        tokio::time::timeout(timeout, receiver)
            .await
            .map_err(|_| "timed out waiting for PutObject completion".to_owned())?
            .map_err(|_| "object-store listener stopped before PutObject completion".to_owned())?;
        self.completed_rx.take();
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), String> {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(mut server) = self.server.take() {
            match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
                Ok(result) => {
                    result.map_err(|err| format!("object-store listener failed: {err}"))?;
                }
                Err(_) => {
                    server.abort();
                    let _ = server.await;
                    return Err("timed out stopping object-store listener".to_owned());
                }
            }
        }
        Ok(())
    }
}

impl Drop for HeldObjectStoreEndpoint {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(());
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}

async fn put_object(State(state): State<Arc<EndpointState>>, request: Request) -> Response {
    state.request_count.fetch_add(1, Ordering::SeqCst);
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_OBJECT_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let captured = CapturedPut {
        method: parts.method.clone(),
        path: parts.uri.path().to_owned(),
        body,
    };
    if let Some(put_tx) = state.put_tx.lock().await.take() {
        let _ = put_tx.send(captured);
    }
    if parts.method != Method::PUT {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }

    let release_rx = state.release_rx.lock().await.take();
    let released = match release_rx {
        Some(release) => release.await.is_ok(),
        None => false,
    };
    if !released {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(completed_tx) = state.completed_tx.lock().await.take() {
        let _ = completed_tx.send(());
    }
    (StatusCode::OK, Body::empty()).into_response()
}
