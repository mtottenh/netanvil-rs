//! TLS certificate loading and configuration for leader↔agent communication.
//!
//! Provides helpers to build rustls `ServerConfig` (for agents accepting mTLS
//! connections) and `ClientConfig` (for leaders connecting to agents with mTLS).

use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::RootCertStore;

use netanvil_types::TlsConfig;

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
