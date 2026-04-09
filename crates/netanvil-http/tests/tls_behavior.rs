//! Behavioral tests for TLS client configuration.
//!
//! Tests cover:
//! - `build_tls_connector` with various configurations
//! - End-to-end HTTPS connections via `HttpExecutor::with_tls_config`
//! - Client certificate authentication (mTLS)
//! - SNI override
//! - Cipher suite filtering
//! - Server verification control

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use netanvil_http::{build_tls_connector, HttpExecutor};
use netanvil_types::config::{HttpVersion, TlsClientConfig};
use netanvil_types::{HttpRequestSpec, RequestContext, RequestExecutor};

// ---------------------------------------------------------------------------
// Certificate generation helpers (rcgen)
// ---------------------------------------------------------------------------

struct TestCerts {
    ca_cert_pem: String,
    server_cert_pem: String,
    server_key_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
}

fn generate_test_certs() -> TestCerts {
    // CA certificate
    let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".to_string()]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .key_usages
        .push(rcgen::KeyUsagePurpose::KeyCertSign);
    ca_params.key_usages.push(rcgen::KeyUsagePurpose::CrlSign);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();

    // Server certificate signed by CA
    let mut server_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    server_params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(
            "127.0.0.1".parse::<std::net::IpAddr>().unwrap(),
        ));
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .unwrap();

    // Client certificate signed by CA
    let client_params = rcgen::CertificateParams::new(vec!["test-client".to_string()]).unwrap();
    let client_key = rcgen::KeyPair::generate().unwrap();
    let client_cert = client_params
        .signed_by(&client_key, &ca_cert, &ca_key)
        .unwrap();

    TestCerts {
        ca_cert_pem: ca_cert.pem(),
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
        client_cert_pem: client_cert.pem(),
        client_key_pem: client_key.serialize_pem(),
    }
}

/// Write cert/key PEM strings to temp files, return paths.
#[allow(dead_code)]
struct TempCertFiles {
    dir: tempfile::TempDir,
    ca_cert_path: String,
    server_cert_path: String,
    server_key_path: String,
    client_cert_path: String,
    client_key_path: String,
}

fn write_cert_files(certs: &TestCerts) -> TempCertFiles {
    let dir = tempfile::TempDir::new().unwrap();
    let base = dir.path();

    let ca_path = base.join("ca.pem");
    let srv_cert_path = base.join("server.pem");
    let srv_key_path = base.join("server-key.pem");
    let cli_cert_path = base.join("client.pem");
    let cli_key_path = base.join("client-key.pem");

    std::fs::write(&ca_path, &certs.ca_cert_pem).unwrap();
    std::fs::write(&srv_cert_path, &certs.server_cert_pem).unwrap();
    std::fs::write(&srv_key_path, &certs.server_key_pem).unwrap();
    std::fs::write(&cli_cert_path, &certs.client_cert_pem).unwrap();
    std::fs::write(&cli_key_path, &certs.client_key_pem).unwrap();

    TempCertFiles {
        dir,
        ca_cert_path: ca_path.to_str().unwrap().to_string(),
        server_cert_path: srv_cert_path.to_str().unwrap().to_string(),
        server_key_path: srv_key_path.to_str().unwrap().to_string(),
        client_cert_path: cli_cert_path.to_str().unwrap().to_string(),
        client_key_path: cli_key_path.to_str().unwrap().to_string(),
    }
}

// ---------------------------------------------------------------------------
// HTTPS test server (tokio + tokio-rustls + axum)
// ---------------------------------------------------------------------------

fn start_https_server(
    certs: &TestCerts,
    require_client_cert: bool,
) -> (SocketAddr, std::thread::JoinHandle<()>) {
    let server_cert_pem = certs.server_cert_pem.clone();
    let server_key_pem = certs.server_key_pem.clone();
    let ca_cert_pem = certs.ca_cert_pem.clone();

    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use axum::routing::get;
            use tokio::net::TcpListener;

            // Install ring crypto provider for server-side rustls
            let _ = rustls::crypto::ring::default_provider().install_default();

            // Build rustls ServerConfig
            let server_certs: Vec<rustls::pki_types::CertificateDer<'static>> =
                rustls_pemfile::certs(&mut server_cert_pem.as_bytes())
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
            let server_key = rustls_pemfile::private_key(&mut server_key_pem.as_bytes())
                .unwrap()
                .unwrap();

            let mut server_config = if require_client_cert {
                let mut root_store = rustls::RootCertStore::empty();
                let ca_certs: Vec<rustls::pki_types::CertificateDer<'static>> =
                    rustls_pemfile::certs(&mut ca_cert_pem.as_bytes())
                        .collect::<Result<Vec<_>, _>>()
                        .unwrap();
                root_store.add_parsable_certificates(ca_certs);
                let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(root_store))
                    .build()
                    .unwrap();
                rustls::ServerConfig::builder()
                    .with_client_cert_verifier(verifier)
                    .with_single_cert(server_certs, server_key)
                    .unwrap()
            } else {
                rustls::ServerConfig::builder()
                    .with_no_client_auth()
                    .with_single_cert(server_certs, server_key)
                    .unwrap()
            };
            server_config.alpn_protocols = vec![b"http/1.1".to_vec()];

            let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

            let app = axum::Router::new()
                .route("/", get(|| async { "OK" }))
                .route("/hello", get(|| async { "hello from TLS" }));

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();

            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = tls_acceptor.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    let Ok(tls_stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let service = hyper_util::service::TowerToHyperService::new(app);
                    let _ = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .serve_connection(io, service)
                    .await;
                });
            }
        });
    });

    let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    (addr, handle)
}

fn make_context() -> RequestContext {
    let now = Instant::now();
    RequestContext {
        request_id: 1,
        intended_time: now,
        sent_time: now,
        actual_time: now,
        dispatch_time: now,
        core_id: 0,
        is_sampled: false,
        session_id: None,
    }
}

// ---------------------------------------------------------------------------
// build_tls_connector unit tests
// ---------------------------------------------------------------------------

#[test]
fn build_connector_with_no_verify_succeeds() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_custom_ca_succeeds() {
    let certs = generate_test_certs();
    let files = write_cert_files(&certs);

    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: Some(files.ca_cert_path.clone()),
        verify_server: true,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_client_cert_succeeds() {
    let certs = generate_test_certs();
    let files = write_cert_files(&certs);

    let config = TlsClientConfig {
        client_cert: Some(files.client_cert_path.clone()),
        client_key: Some(files.client_key_path.clone()),
        ca_cert: Some(files.ca_cert_path.clone()),
        verify_server: true,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_valid_cipher_filter_succeeds() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: Some("ECDHE-RSA-AES128-GCM-SHA256:ECDHE-RSA-AES256-GCM-SHA384".to_string()),
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_single_cipher_succeeds() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: Some("ECDHE-RSA-AES128-GCM-SHA256".to_string()),
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_tls13_cipher_succeeds() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: Some("TLS_AES_256_GCM_SHA384".to_string()),
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_invalid_cipher_fails() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: Some("RC4-MD5".to_string()),
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no supported cipher suites match"),
        "error should mention no matching suites: {err}"
    );
    assert!(
        err.contains("Available ciphers"),
        "error should list available ciphers: {err}"
    );
}

#[test]
fn build_connector_with_mixed_valid_and_invalid_ciphers_warns_but_succeeds() {
    // Valid cipher + invalid cipher: should succeed with the valid one
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: Some("ECDHE-RSA-AES128-GCM-SHA256:RC4-MD5".to_string()),
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(
        result.is_ok(),
        "should succeed with partial match: {:?}",
        result.err()
    );
}

#[test]
fn build_connector_with_session_resumption_disabled_succeeds() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: true,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_ok(), "should succeed: {:?}", result.err());
}

#[test]
fn build_connector_with_nonexistent_ca_cert_fails() {
    let config = TlsClientConfig {
        client_cert: None,
        client_key: None,
        ca_cert: Some("/nonexistent/ca.pem".to_string()),
        verify_server: true,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("reading CA cert"),
        "error should mention CA cert path"
    );
}

#[test]
fn build_connector_with_nonexistent_client_cert_fails() {
    let config = TlsClientConfig {
        client_cert: Some("/nonexistent/client.pem".to_string()),
        client_key: Some("/nonexistent/client-key.pem".to_string()),
        ca_cert: None,
        verify_server: false,
        sni_override: None,
        cipher_list: None,
        disable_session_resumption: false,
    };
    let result = build_tls_connector(&config, HttpVersion::Http1);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("reading client cert"),
        "error should mention client cert path"
    );
}

// ---------------------------------------------------------------------------
// End-to-end HTTPS tests
// ---------------------------------------------------------------------------

#[test]
fn https_request_with_no_verify_succeeds() {
    let certs = generate_test_certs();
    let (addr, _server) = start_https_server(&certs, false);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let config = TlsClientConfig {
            client_cert: None,
            client_key: None,
            ca_cert: None,
            verify_server: false,
            sni_override: None,
            cipher_list: None,
            disable_session_resumption: false,
        };
        let executor =
            HttpExecutor::with_tls_config(&config, None, Duration::from_secs(5)).unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;
        assert_eq!(result.status, Some(200), "error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
    });
}

#[test]
fn https_request_with_custom_ca_succeeds() {
    let certs = generate_test_certs();
    let files = write_cert_files(&certs);
    let (addr, _server) = start_https_server(&certs, false);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let config = TlsClientConfig {
            client_cert: None,
            client_key: None,
            ca_cert: Some(files.ca_cert_path.clone()),
            verify_server: true,
            sni_override: None,
            cipher_list: None,
            disable_session_resumption: false,
        };
        let executor =
            HttpExecutor::with_tls_config(&config, None, Duration::from_secs(5)).unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;
        assert_eq!(result.status, Some(200), "error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
    });
}

#[test]
fn https_mtls_with_client_cert_succeeds() {
    let certs = generate_test_certs();
    let files = write_cert_files(&certs);
    let (addr, _server) = start_https_server(&certs, true);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let config = TlsClientConfig {
            client_cert: Some(files.client_cert_path.clone()),
            client_key: Some(files.client_key_path.clone()),
            ca_cert: Some(files.ca_cert_path.clone()),
            verify_server: true,
            sni_override: None,
            cipher_list: None,
            disable_session_resumption: false,
        };
        let executor =
            HttpExecutor::with_tls_config(&config, None, Duration::from_secs(5)).unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;
        assert_eq!(result.status, Some(200), "error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
    });
}

#[test]
fn https_mtls_without_client_cert_fails_when_server_requires_it() {
    let certs = generate_test_certs();
    let files = write_cert_files(&certs);
    let (addr, _server) = start_https_server(&certs, true);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        // No client cert, but server requires mTLS
        let config = TlsClientConfig {
            client_cert: None,
            client_key: None,
            ca_cert: Some(files.ca_cert_path.clone()),
            verify_server: true,
            sni_override: None,
            cipher_list: None,
            disable_session_resumption: false,
        };
        let executor =
            HttpExecutor::with_tls_config(&config, None, Duration::from_secs(5)).unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;
        // Should fail — server requires client cert but we didn't provide one
        assert!(
            result.error.is_some(),
            "should fail without client cert when server requires mTLS"
        );
    });
}

#[test]
fn https_with_sni_override_succeeds() {
    let certs = generate_test_certs();
    let (addr, _server) = start_https_server(&certs, false);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let config = TlsClientConfig {
            client_cert: None,
            client_key: None,
            ca_cert: None,
            verify_server: false,
            sni_override: Some("custom-sni-name.example.com".to_string()),
            cipher_list: None,
            disable_session_resumption: false,
        };
        let executor =
            HttpExecutor::with_tls_config(&config, None, Duration::from_secs(5)).unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        // SNI is set to custom-sni-name.example.com, but verify is off
        // so the connection should succeed despite hostname mismatch
        let result = executor.execute(&spec, &make_context()).await;
        assert_eq!(result.status, Some(200), "error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
    });
}

#[test]
fn https_with_bandwidth_throttling_succeeds() {
    let certs = generate_test_certs();
    let (addr, _server) = start_https_server(&certs, false);
    let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();

    rt.block_on(async {
        let config = TlsClientConfig {
            client_cert: None,
            client_key: None,
            ca_cert: None,
            verify_server: false,
            sni_override: None,
            cipher_list: None,
            disable_session_resumption: false,
        };
        // TLS + bandwidth throttling combined
        let executor =
            HttpExecutor::with_tls_config(&config, Some(10_000_000), Duration::from_secs(5))
                .unwrap();

        let spec = HttpRequestSpec {
            method: http::Method::GET,
            url: format!("https://127.0.0.1:{}/hello", addr.port()),
            headers: vec![],
            body: None,
        };

        let result = executor.execute(&spec, &make_context()).await;
        assert_eq!(result.status, Some(200), "error: {:?}", result.error);
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
    });
}
