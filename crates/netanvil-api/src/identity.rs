//! Client identity extraction from mTLS peer certificates.
//!
//! Provides:
//! - `ClientIdentity`: type holding SANs from the client certificate
//! - `CertExtractingAcceptor`: axum-server Accept impl that performs TLS
//!   handshake and injects `ClientIdentity` into each request's extensions
//! - `FromRequestParts` impl so handlers can extract `ClientIdentity`
//! - `SanVerifier`: tower layer that rejects requests from untrusted SANs

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum_server::accept::Accept;
use axum_server::tls_rustls::RustlsConfig;
use http::Request;
use tokio_rustls::server::TlsStream;
use tower::Service;
use x509_parser::prelude::*;

/// Identity extracted from the client's mTLS certificate.
///
/// Injected as a request extension by `CertExtractingAcceptor`.
/// Handlers can extract this via `ClientIdentity` as a parameter.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    /// Subject Alternative Names (DNS names) from the leaf certificate.
    pub sans: Vec<String>,
}

impl ClientIdentity {
    /// Check if any SAN matches the given name.
    pub fn has_san(&self, name: &str) -> bool {
        self.sans.iter().any(|s| s == name)
    }
}

/// Extract SANs from a DER-encoded X.509 certificate.
fn extract_sans(cert_der: &[u8]) -> Vec<String> {
    let Ok((_, cert)) = X509Certificate::from_der(cert_der) else {
        return Vec::new();
    };

    let Ok(Some(san)) = cert.subject_alternative_name() else {
        return Vec::new();
    };

    san.value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some(dns.to_string()),
            GeneralName::RFC822Name(email) => Some(email.to_string()),
            GeneralName::URI(uri) => Some(uri.to_string()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// FromRequestParts — lets handlers extract ClientIdentity
// ---------------------------------------------------------------------------

impl<S> FromRequestParts<S> for ClientIdentity
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = parts
            .extensions
            .get::<ClientIdentity>()
            .cloned()
            .ok_or((StatusCode::UNAUTHORIZED, "client certificate required"));
        std::future::ready(result)
    }
}

// ---------------------------------------------------------------------------
// CertExtractingAcceptor — TLS handshake + cert extraction
// ---------------------------------------------------------------------------

/// An axum-server `Accept` implementation that performs TLS handshake
/// and injects `ClientIdentity` into each request's extensions.
///
/// Replaces `axum_server::tls_rustls::RustlsAcceptor` with added
/// peer certificate extraction.
#[derive(Clone)]
pub struct CertExtractingAcceptor {
    config: RustlsConfig,
    handshake_timeout: Duration,
}

impl CertExtractingAcceptor {
    /// Create a new cert-extracting TLS acceptor.
    pub fn new(config: RustlsConfig) -> Self {
        Self {
            config,
            handshake_timeout: Duration::from_secs(10),
        }
    }

    /// Override the default TLS handshake timeout.
    pub fn handshake_timeout(mut self, val: Duration) -> Self {
        self.handshake_timeout = val;
        self
    }
}

impl<S> Accept<tokio::net::TcpStream, S> for CertExtractingAcceptor
where
    S: Send + 'static,
{
    type Stream = TlsStream<tokio::net::TcpStream>;
    type Service = WithIdentity<S>;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Service)>> + Send>>;

    fn accept(&self, stream: tokio::net::TcpStream, service: S) -> Self::Future {
        let config = self.config.clone();
        let timeout = self.handshake_timeout;

        Box::pin(async move {
            let server_config = config.get_inner();
            let acceptor = tokio_rustls::TlsAcceptor::from(server_config);

            let tls_stream = match tokio::time::timeout(timeout, acceptor.accept(stream)).await {
                Ok(Ok(stream)) => stream,
                Ok(Err(e)) => return Err(io::Error::new(io::ErrorKind::Other, e)),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "TLS handshake timed out",
                    ))
                }
            };

            // Extract peer certificates from the completed TLS handshake.
            let identity = {
                let (_, server_conn) = tls_stream.get_ref();
                let sans = server_conn
                    .peer_certificates()
                    .and_then(|certs| certs.first())
                    .map(|cert| extract_sans(cert.as_ref()))
                    .unwrap_or_default();

                ClientIdentity { sans }
            };

            let service = WithIdentity {
                inner: service,
                identity,
            };

            Ok((tls_stream, service))
        })
    }
}

// ---------------------------------------------------------------------------
// WithIdentity — service wrapper that injects ClientIdentity per request
// ---------------------------------------------------------------------------

/// Tower service wrapper that inserts `ClientIdentity` into every request's
/// extensions. Created per-connection by `CertExtractingAcceptor`.
#[derive(Clone)]
pub struct WithIdentity<S> {
    inner: S,
    identity: ClientIdentity,
}

impl<S, B> Service<Request<B>> for WithIdentity<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        req.extensions_mut().insert(self.identity.clone());
        self.inner.call(req)
    }
}

// ---------------------------------------------------------------------------
// SanVerifier — middleware that rejects untrusted SANs
// ---------------------------------------------------------------------------

/// Tower layer that verifies the client certificate's SAN against an allowlist.
///
/// Applied to the agent router when TLS is enabled. Returns 403 Forbidden
/// for requests from clients with unrecognized SANs.
#[derive(Clone)]
pub struct SanVerifierLayer {
    trusted_sans: Arc<Vec<String>>,
}

impl SanVerifierLayer {
    /// Create a new SAN verifier. An empty list disables verification
    /// (all authenticated clients are accepted).
    pub fn new(trusted_sans: Vec<String>) -> Self {
        Self {
            trusted_sans: Arc::new(trusted_sans),
        }
    }
}

impl<S> tower::Layer<S> for SanVerifierLayer {
    type Service = SanVerifierService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        SanVerifierService {
            inner,
            trusted_sans: self.trusted_sans.clone(),
        }
    }
}

/// The service created by `SanVerifierLayer`.
#[derive(Clone)]
pub struct SanVerifierService<S> {
    inner: S,
    trusted_sans: Arc<Vec<String>>,
}

impl<S, B> Service<Request<B>> for SanVerifierService<S>
where
    S: Service<Request<B>, Response = axum::response::Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let trusted = self.trusted_sans.clone();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            // If no trusted SANs configured, skip verification.
            if trusted.is_empty() {
                return inner.call(req).await;
            }

            // Check if the client's identity has a trusted SAN.
            let identity = req.extensions().get::<ClientIdentity>();
            let authorized = identity
                .map(|id| id.sans.iter().any(|s| trusted.contains(s)))
                .unwrap_or(false);

            if authorized {
                inner.call(req).await
            } else {
                let san_info = identity
                    .map(|id| format!("{:?}", id.sans))
                    .unwrap_or_else(|| "none".to_string());
                tracing::warn!(client_sans = %san_info, "rejected: SAN not in trusted list");
                Ok(axum::response::IntoResponse::into_response((
                    StatusCode::FORBIDDEN,
                    "client certificate SAN not authorized",
                )))
            }
        })
    }
}
