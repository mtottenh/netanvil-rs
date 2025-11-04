//! mTLS server and certificate loading for leader↔agent communication.
//!
//! When TLS is configured, the agent runs a custom HTTP/1.1 server over rustls
//! instead of tiny_http. The server verifies client certificates against a CA,
//! ensuring only authorized leaders can send commands (including plugin uploads).
//!
//! The HTTP protocol surface is intentionally minimal: fixed routes, JSON bodies,
//! sequential request handling. This is a control plane (~10 req/sec), not a
//! data plane.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;

use netanvil_types::TlsConfig;

/// A parsed HTTP/1.1 request from a TLS stream.
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// An HTTP response to write back.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }

    pub fn error(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

/// Load PEM certificates from a file.
pub fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse certs from {path}: {e}"))
}

/// Load a private key from a PEM file.
pub fn load_key(path: &str) -> Result<PrivateKeyDer<'static>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| format!("parse key from {path}: {e}"))?
        .ok_or_else(|| format!("no private key found in {path}"))
}

/// Build a rustls `ServerConfig` with mTLS (client cert verification).
pub fn build_server_config(tls: &TlsConfig) -> Result<Arc<rustls::ServerConfig>, String> {
    // Load CA cert for verifying client certificates
    let ca_certs = load_certs(&tls.ca_cert)?;
    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| format!("add CA cert: {e}"))?;
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());

    // Require client certificates, verified against our CA
    let client_verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(root_store), provider.clone())
            .build()
            .map_err(|e| format!("build client verifier: {e}"))?;

    // Load server cert + key
    let server_certs = load_certs(&tls.cert)?;
    let server_key = load_key(&tls.key)?;

    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("protocol versions: {e}"))?
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certs, server_key)
        .map_err(|e| format!("server config: {e}"))?;

    Ok(Arc::new(config))
}

/// Build a rustls `ClientConfig` with mTLS (present client cert, verify server).
pub fn build_client_config(tls: &TlsConfig) -> Result<Arc<rustls::ClientConfig>, String> {
    // Load CA cert for verifying server certificates
    let ca_certs = load_certs(&tls.ca_cert)?;
    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| format!("add CA cert: {e}"))?;
    }

    // Load client cert + key
    let client_certs = load_certs(&tls.cert)?;
    let client_key = load_key(&tls.key)?;

    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("protocol versions: {e}"))?
    .with_root_certificates(root_store)
    .with_client_auth_cert(client_certs, client_key)
    .map_err(|e| format!("client config: {e}"))?;

    Ok(Arc::new(config))
}

/// A simple mTLS HTTP/1.1 server.
///
/// Accepts TLS connections, verifies client certificates, parses HTTP requests,
/// and routes them to a handler function. Runs synchronously (sequential
/// request handling) — appropriate for the low-throughput control plane.
pub struct MtlsServer {
    listener: TcpListener,
    tls_config: Arc<rustls::ServerConfig>,
}

impl MtlsServer {
    /// Create a new mTLS server bound to the given address.
    pub fn new(addr: &str, tls: &TlsConfig) -> Result<Self, String> {
        let listener = TcpListener::bind(addr).map_err(|e| format!("bind {addr}: {e}"))?;
        let tls_config = build_server_config(tls)?;
        tracing::info!(addr, "mTLS server listening");
        Ok(Self {
            listener,
            tls_config,
        })
    }

    /// Accept connections and dispatch to the handler.
    ///
    /// The handler receives an `HttpRequest` and returns an `HttpResponse`.
    /// This method blocks forever (call from a dedicated thread).
    ///
    /// Returns `stop_flag` receiver so the handler can signal shutdown.
    pub fn serve(&self, handler: impl Fn(HttpRequest) -> HttpResponse) {
        for stream in self.listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("accept error: {e}");
                    continue;
                }
            };

            let peer_addr = stream.peer_addr().ok();

            // Perform TLS handshake with client cert verification
            let server_conn = match rustls::ServerConnection::new(self.tls_config.clone()) {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::warn!(?peer_addr, "TLS connection create error: {e}");
                    continue;
                }
            };

            let mut tls_stream = rustls::StreamOwned::new(server_conn, stream);

            // Parse HTTP request
            match parse_request(&mut tls_stream) {
                Ok(request) => {
                    let response = handler(request);
                    if let Err(e) = write_response(&mut tls_stream, &response) {
                        tracing::warn!(?peer_addr, "write response error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!(?peer_addr, "parse request error: {e}");
                    let _ = write_response(
                        &mut tls_stream,
                        &HttpResponse::error(400, format!("{{\"error\":\"{e}\"}}")),
                    );
                }
            }
        }
    }
}

/// Parse an HTTP/1.1 request from a stream.
///
/// Reads the request line, headers, and body (based on Content-Length).
/// Supports the minimal subset needed by the agent protocol.
fn parse_request<S: Read>(stream: &mut S) -> Result<HttpRequest, String> {
    let mut reader = BufReader::new(stream);

    // Read request line: "METHOD /path HTTP/1.1\r\n"
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("read request line: {e}"))?;
    let request_line = request_line.trim_end();

    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("empty request line")?.to_string();
    let path = parts.next().ok_or("missing path")?.to_string();
    // Ignore HTTP version

    // Read headers
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("read header: {e}"))?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value
                .trim()
                .parse()
                .map_err(|e| format!("bad Content-Length: {e}"))?;
        } else if let Some(value) = line.strip_prefix("content-length:") {
            content_length = value
                .trim()
                .parse()
                .map_err(|e| format!("bad content-length: {e}"))?;
        }
    }

    // Read body
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("read body ({content_length} bytes): {e}"))?;
    }

    Ok(HttpRequest { method, path, body })
}

/// Write an HTTP/1.1 response to a stream.
fn write_response<S: Write>(stream: &mut S, response: &HttpResponse) -> Result<(), String> {
    let status_text = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Unknown",
    };

    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status, status_text, response.body.len()
    );

    stream
        .write_all(header.as_bytes())
        .map_err(|e| format!("write header: {e}"))?;
    stream
        .write_all(response.body.as_bytes())
        .map_err(|e| format!("write body: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;

    Ok(())
}

/// Connect to a remote server with mTLS and send an HTTP request.
///
/// Used by the leader's `HttpNodeCommander` when TLS is configured.
pub fn mtls_request(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    client_config: &Arc<rustls::ClientConfig>,
) -> Result<String, String> {
    // Extract hostname from addr for SNI
    let hostname = addr.split(':').next().unwrap_or(addr);
    let server_name = rustls::pki_types::ServerName::try_from(hostname.to_string())
        .map_err(|e| format!("invalid server name '{hostname}': {e}"))?;

    let stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    let conn = rustls::ClientConnection::new(client_config.clone(), server_name)
        .map_err(|e| format!("TLS handshake: {e}"))?;
    let mut tls = rustls::StreamOwned::new(conn, stream);

    // Write HTTP request
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(request.as_bytes())
        .map_err(|e| format!("write request: {e}"))?;
    tls.flush().map_err(|e| format!("flush: {e}"))?;

    // Read response (skip headers, return body)
    let mut response = String::new();
    tls.read_to_string(&mut response)
        .map_err(|e| format!("read response: {e}"))?;

    // Extract body after \r\n\r\n
    if let Some(idx) = response.find("\r\n\r\n") {
        Ok(response[idx + 4..].to_string())
    } else {
        Ok(response)
    }
}
