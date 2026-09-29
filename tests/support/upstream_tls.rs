use axum::{http::StatusCode, middleware, response::IntoResponse, serve::Listener, Router};
use chrono::Datelike;
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{copy_bidirectional, duplex, DuplexStream},
    net::{TcpListener, TcpStream},
    sync::{watch, Notify, Semaphore},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_rustls::{
    rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        ServerConfig,
    },
    server::TlsStream,
    TlsAcceptor,
};

pub const TRUSTED_CA_PEM: &[u8] = include_bytes!("../fixtures/tls/ca.pem");
const SERVER_CERT_DER: &[u8] = include_bytes!("../fixtures/tls/server.der");
const SERVER_KEY_DER: &[u8] = include_bytes!("../fixtures/tls/server.key.der");
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

struct TlsListener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    handshakes: JoinSet<Option<(TlsStream<TcpStream>, SocketAddr)>>,
    handshake_slots: Arc<Semaphore>,
    handshake_started: Arc<Notify>,
    cancel_rx: watch::Receiver<bool>,
    bridges: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Listener for TlsListener {
    type Io = DuplexStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                accepted = self.listener.accept(), if self.handshakes.len() < 4 => {
                    let (stream, address) = accepted.expect("TLS listener accept");
                    let acceptor = self.acceptor.clone();
                    let permit = Arc::clone(&self.handshake_slots)
                        .try_acquire_owned()
                        .expect("TLS handshake slot");
                    self.handshakes.spawn(async move {
                        let _permit = permit;
                        timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                            .await.ok()?.ok().map(|stream| (stream, address))
                    });
                    self.handshake_started.notify_one();
                }
                completed = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    let Some(Ok(Some((mut stream, address)))) = completed else { continue };
                    let (server_stream, mut bridge_stream) = duplex(64 * 1024);
                    let mut cancel_rx = self.cancel_rx.clone();
                    let bridge = tokio::spawn(async move {
                        tokio::select! {
                            _ = async {
                                if !*cancel_rx.borrow() {
                                    let _ = cancel_rx.changed().await;
                                }
                            } => {},
                            _ = copy_bidirectional(&mut stream, &mut bridge_stream) => {},
                        }
                    });
                    self.bridges
                        .lock()
                        .expect("TLS bridge registry")
                        .push(bridge);
                    return (server_stream, address);
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

pub struct TlsFixture {
    address: SocketAddr,
    shutdown: Option<watch::Sender<bool>>,
    cancel_requests: Option<watch::Sender<bool>>,
    bridges: Arc<Mutex<Vec<JoinHandle<()>>>>,
    handshake_slots: Arc<Semaphore>,
    handshake_started: Arc<Notify>,
    server: Option<JoinHandle<io::Result<()>>>,
}

impl TlsFixture {
    pub async fn start(router: Router) -> io::Result<Self> {
        Self::start_with_material(router, SERVER_CERT_DER, SERVER_KEY_DER).await
    }

    pub async fn start_with_material(
        router: Router,
        certificate_der: &[u8],
        private_key_der: &[u8],
    ) -> io::Result<Self> {
        let year = chrono::Utc::now().year();
        if !(2026..=2034).contains(&year) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("TLS fixture certificate is only accepted during UTC 2026-2034; current year is {year}"),
            ));
        }
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(certificate_der.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(private_key_der.to_vec())),
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let (cancel_requests, cancel_rx) = watch::channel(false);
        let bridges = Arc::new(Mutex::new(Vec::new()));
        let handshake_slots = Arc::new(Semaphore::new(4));
        let handshake_started = Arc::new(Notify::new());
        let tls_listener = TlsListener {
            listener,
            acceptor: TlsAcceptor::from(Arc::new(config)),
            handshakes: JoinSet::new(),
            handshake_slots: Arc::clone(&handshake_slots),
            handshake_started: Arc::clone(&handshake_started),
            cancel_rx: cancel_rx.clone(),
            bridges: Arc::clone(&bridges),
        };
        let router = router.layer(middleware::from_fn(
            move |request: axum::extract::Request, next: middleware::Next| {
                let mut cancel_rx = cancel_rx.clone();
                async move {
                    tokio::select! {
                        response = next.run(request) => response,
                        _ = cancel_rx.changed() => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
            },
        ));
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(async move {
            axum::serve(tls_listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.changed().await;
                })
                .await
        });

        Ok(Self {
            address,
            shutdown: Some(shutdown),
            cancel_requests: Some(cancel_requests),
            bridges,
            handshake_slots,
            handshake_started,
            server: Some(server),
        })
    }

    pub fn url(&self) -> String {
        format!("https://{}", self.address)
    }

    pub async fn wait_for_handshake_start(&self) {
        self.handshake_started.notified().await;
    }

    async fn join_bridges(&self) -> Result<(), String> {
        let bridges = std::mem::take(&mut *self.bridges.lock().expect("TLS bridge registry"));
        let mut timed_out = false;
        for mut bridge in bridges {
            if timeout(Duration::from_secs(1), &mut bridge).await.is_err() {
                bridge.abort();
                let _ = bridge.await;
                timed_out = true;
            }
        }
        if timed_out {
            Err("TLS fixture bridge shutdown timed out".to_owned())
        } else {
            Ok(())
        }
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        self.shutdown
            .take()
            .expect("TLS fixture shutdown sender")
            .send_replace(true);
        let mut server = self.server.take().expect("TLS fixture server task");
        let result = timeout(Duration::from_secs(6), &mut server).await;
        let timed_out = result.is_err();
        if timed_out {
            self.cancel_requests
                .as_ref()
                .expect("TLS fixture request cancellation sender")
                .send_replace(true);
            if timeout(Duration::from_secs(1), &mut server).await.is_err() {
                server.abort();
                let _ = server.await;
            }
        }
        self.cancel_requests
            .take()
            .expect("TLS fixture request cancellation sender")
            .send_replace(true);
        let _handshake_permits =
            timeout(Duration::from_secs(1), self.handshake_slots.acquire_many(4))
                .await
                .map_err(|_| "TLS fixture handshake shutdown timed out".to_owned())?
                .map_err(|error| format!("TLS fixture handshake slots closed: {error}"))?;
        self.join_bridges().await?;
        if timed_out {
            return Err("TLS fixture shutdown timed out; active requests cancelled".to_owned());
        }
        result
            .expect("checked timeout")
            .map_err(|error| format!("TLS fixture server task failed: {error}"))?
            .map_err(|error| format!("TLS fixture server failed: {error}"))
    }
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(true);
        }
        if let Some(cancel_requests) = self.cancel_requests.take() {
            cancel_requests.send_replace(true);
        }
        if let Some(server) = self.server.take() {
            server.abort();
        }
    }
}
