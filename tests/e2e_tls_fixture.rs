#![allow(dead_code)]

#[path = "support/upstream_tls.rs"]
mod upstream_tls;

use axum::{
    body::Bytes,
    routing::{get, post},
    Router,
};
use std::{
    future::pending,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_rustls::{
    rustls::{
        pki_types::{CertificateDer, ServerName},
        ClientConfig, RootCertStore,
    },
    TlsConnector,
};

#[tokio::test]
async fn stv_prov_38_fixture_acceptance() {
    let (request_tx, mut request_rx) = mpsc::channel(1);
    let app = Router::new().route(
        "/fixture",
        post(move |body: Bytes| {
            let request_tx = request_tx.clone();
            async move {
                request_tx.send(body).await.expect("capture request");
                "ok"
            }
        }),
    );
    let fixture = upstream_tls::TlsFixture::start(app)
        .await
        .expect("start TLS fixture");
    let url = format!("{}/fixture", fixture.url());
    let ca =
        reqwest::Certificate::from_pem(upstream_tls::TRUSTED_CA_PEM).expect("parse trusted CA");
    let trusted = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .build()
        .expect("build trusted client");
    let response = trusted
        .post(&url)
        .body("fixture request")
        .send()
        .await
        .expect("trusted request")
        .error_for_status()
        .expect("trusted status");
    assert_eq!(response.text().await.expect("trusted response body"), "ok");
    assert_eq!(
        request_rx.recv().await.expect("captured trusted request"),
        Bytes::from_static(b"fixture request")
    );

    let untrusted = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("build untrusted client");
    let error = untrusted
        .post(&url)
        .body("must fail")
        .send()
        .await
        .expect_err("request without CA must fail validation");
    assert!(
        error.is_connect(),
        "expected TLS validation failure: {error}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), request_rx.recv())
            .await
            .is_err(),
        "untrusted request reached handler"
    );

    fixture.shutdown().await.expect("fixture shutdown");
}

#[tokio::test]
async fn tls_fixture_shutdown_aborts_stalled_connection() {
    struct DropMarker(Arc<AtomicBool>);
    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let handler_stopped = Arc::new(AtomicBool::new(false));
    let handler_marker = Arc::clone(&handler_stopped);
    let (entered_tx, mut entered_rx) = mpsc::channel(1);
    let app = Router::new().route(
        "/stall",
        post(move || {
            let entered_tx = entered_tx.clone();
            let handler_stopped = Arc::clone(&handler_marker);
            async move {
                let _marker = DropMarker(handler_stopped);
                entered_tx.send(()).await.expect("signal stalled handler");
                pending::<&'static str>().await
            }
        }),
    );
    let fixture = upstream_tls::TlsFixture::start(app)
        .await
        .expect("start TLS fixture");
    let address = fixture.url();
    let socket_addr = address
        .strip_prefix("https://")
        .expect("TLS URL scheme")
        .parse::<std::net::SocketAddr>()
        .expect("TLS loopback address");
    let ca =
        reqwest::Certificate::from_pem(upstream_tls::TRUSTED_CA_PEM).expect("parse trusted CA");
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .build()
        .expect("build trusted client");
    let request = tokio::spawn(async move { client.post(format!("{address}/stall")).send().await });
    tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
        .await
        .expect("handler entered before timeout")
        .expect("handler signal");

    assert_eq!(
        fixture
            .shutdown()
            .await
            .expect_err("stalled handler must time out"),
        "TLS fixture shutdown timed out; active requests cancelled"
    );
    assert!(
        handler_stopped.load(Ordering::SeqCst),
        "stalled handler remained active"
    );
    request.abort();
    let _ = request.await;
    tokio::net::TcpStream::connect(socket_addr)
        .await
        .expect_err("TLS listener remained open after timeout");
}

#[tokio::test]
async fn tls_fixture_shutdown_closes_partial_request() {
    let fixture = upstream_tls::TlsFixture::start(Router::new())
        .await
        .expect("start TLS fixture");
    let socket_addr = fixture
        .url()
        .strip_prefix("https://")
        .expect("TLS URL scheme")
        .parse::<std::net::SocketAddr>()
        .expect("TLS loopback address");
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(
            include_bytes!("fixtures/tls/ca.der").to_vec(),
        ))
        .expect("trusted CA DER");
    let client = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(socket_addr)
        .await
        .expect("connect loopback");
    let mut tls = TlsConnector::from(Arc::new(client))
        .connect(ServerName::IpAddress(socket_addr.ip().into()), tcp)
        .await
        .expect("trusted TLS handshake");
    tls.write_all(b"GET /stall HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Test: ")
        .await
        .expect("send partial HTTP headers");

    fixture.shutdown().await.expect("partial request cleanup");
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(1), tls.read(&mut byte))
        .await
        .expect("partial connection remained open");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "partial connection was not closed"
    );
}

#[tokio::test]
async fn tls_fixture_accepts_while_handshake_stalls() {
    let app = Router::new().route("/ok", get(|| async { "ok" }));
    let fixture = upstream_tls::TlsFixture::start(app)
        .await
        .expect("start TLS fixture");
    let address = fixture.url();
    let socket_addr = address
        .strip_prefix("https://")
        .expect("TLS URL scheme")
        .parse::<std::net::SocketAddr>()
        .expect("TLS loopback address");
    let mut idle = tokio::net::TcpStream::connect(socket_addr)
        .await
        .expect("open idle handshake");
    tokio::time::timeout(Duration::from_secs(1), fixture.wait_for_handshake_start())
        .await
        .expect("idle TLS handshake was not accepted");
    let ca =
        reqwest::Certificate::from_pem(upstream_tls::TRUSTED_CA_PEM).expect("parse trusted CA");
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .timeout(Duration::from_secs(4))
        .build()
        .expect("build trusted client");
    let response = client
        .get(format!("{address}/ok"))
        .send()
        .await
        .expect("valid client blocked by stalled handshake");
    assert_eq!(response.text().await.expect("response body"), "ok");
    fixture.shutdown().await.expect("fixture shutdown");
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut byte))
        .await
        .expect("idle handshake remained open");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "idle handshake was not closed"
    );
}

#[tokio::test]
async fn stv_prov_38_certificate_variants() {
    async fn assert_rejected(
        label: &str,
        expected_cause: &str,
        certificate: &[u8],
        private_key: &[u8],
    ) {
        let captured = Arc::new(AtomicUsize::new(0));
        let route_captured = Arc::clone(&captured);
        let fixture = upstream_tls::TlsFixture::start_with_material(
            Router::new().route(
                "/variant",
                get(move || {
                    let captured = Arc::clone(&route_captured);
                    async move {
                        captured.fetch_add(1, Ordering::SeqCst);
                        "unexpected"
                    }
                }),
            ),
            certificate,
            private_key,
        )
        .await
        .unwrap_or_else(|error| panic!("{label}: start TLS fixture: {error}"));
        let ca =
            reqwest::Certificate::from_pem(upstream_tls::TRUSTED_CA_PEM).expect("parse trusted CA");
        let client = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(ca)
            .build()
            .expect("build variant client");
        let result = client
            .get(format!("{}/variant", fixture.url()))
            .send()
            .await;
        let error = match result {
            Ok(response) => panic!("{label}: unexpected HTTP response {response:?}"),
            Err(error) => error,
        };
        assert!(error.is_connect(), "{label}: expected TLS failure: {error}");
        let debug = format!("{error:?}");
        assert!(
            debug.contains(expected_cause),
            "{label}: expected {expected_cause} in TLS error, got {debug}"
        );
        assert_eq!(
            captured.load(Ordering::SeqCst),
            0,
            "{label}: route was reached"
        );
        fixture
            .shutdown()
            .await
            .unwrap_or_else(|error| panic!("{label}: fixture shutdown: {error}"));
    }

    assert_rejected(
        "unrelated CA, current validity, right IP",
        "UnknownIssuer",
        include_bytes!("fixtures/tls/unrelated-server.der"),
        include_bytes!("fixtures/tls/unrelated-server.key.der"),
    )
    .await;
    assert_rejected(
        "trusted CA, current validity, wrong IP",
        "NotValidForName",
        include_bytes!("fixtures/tls/wrong-ip-server.der"),
        include_bytes!("fixtures/tls/wrong-ip-server.key.der"),
    )
    .await;
    assert_rejected(
        "trusted CA, expired, right IP",
        "Expired",
        include_bytes!("fixtures/tls/expired-server.der"),
        include_bytes!("fixtures/tls/expired-server.key.der"),
    )
    .await;
}
