//! TLS connector construction from `TlsClientConfig` using rustls.
//!
//! Builds a `compio::tls::TlsConnector` backed by a pre-configured
//! `rustls::ClientConfig`. Supports client certificates, custom CAs,
//! server verification control, cipher suite filtering, and session
//! resumption control.

use std::fmt;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring::default_provider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};

use netanvil_types::config::TlsClientConfig;

/// Build a `compio::tls::TlsConnector` from a [`TlsClientConfig`].
///
/// Uses rustls with the ring crypto provider. The returned connector
/// can be passed to [`ThrottledConnector::with_tls`] for use in the
/// HTTP executor pipeline.
pub fn build_tls_connector(
    config: &TlsClientConfig,
) -> Result<compio::tls::TlsConnector, Box<dyn std::error::Error + Send + Sync>> {
    // --- Crypto provider (with optional cipher filtering) ---
    let provider = if let Some(ref cipher_str) = config.cipher_list {
        let mut provider = default_provider();
        provider.cipher_suites = filter_cipher_suites(&provider.cipher_suites, cipher_str)?;
        Arc::new(provider)
    } else {
        Arc::new(default_provider())
    };

    // --- ClientConfig with verification ---
    let builder =
        ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions()?;

    let builder_with_roots = if config.verify_server {
        let root_store = build_root_store(config)?;
        builder.with_root_certificates(root_store)
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
    };

    // --- Client authentication ---
    let mut client_config =
        if let (Some(cert_path), Some(key_path)) = (&config.client_cert, &config.client_key) {
            let cert_pem = std::fs::read(cert_path)
                .map_err(|e| format!("reading client cert '{}': {}", cert_path, e))?;
            let key_pem = std::fs::read(key_path)
                .map_err(|e| format!("reading client key '{}': {}", key_path, e))?;

            let certs: Vec<CertificateDer<'static>> =
                rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<Vec<_>, _>>()?;
            if certs.is_empty() {
                return Err("no certificates found in client cert PEM file".into());
            }

            let key = rustls_pemfile::private_key(&mut &key_pem[..])?
                .ok_or("no private key found in client key PEM file")?;

            builder_with_roots.with_client_auth_cert(certs, key)?
        } else {
            builder_with_roots.with_no_client_auth()
        };

    // --- Session resumption ---
    if config.disable_session_resumption {
        client_config.resumption = rustls::client::Resumption::disabled();
    }

    // --- ALPN ---
    client_config.alpn_protocols = vec![b"http/1.1".to_vec()];

    Ok(compio::tls::TlsConnector::from(Arc::new(client_config)))
}

/// Build a root certificate store from config or system roots.
fn build_root_store(
    config: &TlsClientConfig,
) -> Result<rustls::RootCertStore, Box<dyn std::error::Error + Send + Sync>> {
    let mut root_store = rustls::RootCertStore::empty();

    if let Some(ca_path) = &config.ca_cert {
        let ca_pem =
            std::fs::read(ca_path).map_err(|e| format!("reading CA cert '{}': {}", ca_path, e))?;
        let certs: Vec<CertificateDer<'static>> =
            rustls_pemfile::certs(&mut &ca_pem[..]).collect::<Result<Vec<_>, _>>()?;
        if certs.is_empty() {
            return Err(format!("no certificates found in CA PEM file '{}'", ca_path).into());
        }
        let (added, _ignored) = root_store.add_parsable_certificates(certs);
        if added == 0 {
            return Err(format!("no valid CA certificates in '{}'", ca_path).into());
        }
    } else {
        // Load system root certificates
        let native = rustls_native_certs::load_native_certs();
        let (added, _ignored) = root_store.add_parsable_certificates(native.certs);
        if added == 0 {
            return Err("no system root certificates found".into());
        }
    }

    Ok(root_store)
}

// ---------------------------------------------------------------------------
// Cipher suite filtering
// ---------------------------------------------------------------------------

use rustls::CipherSuite;

/// OpenSSL cipher name → rustls `CipherSuite` enum variant.
///
/// Only covers the suites available in rustls's ring provider (modern
/// AEAD suites). Legacy ciphers (RC4, DES, CBC, non-ECDHE) are not
/// supported and will produce an error.
const CIPHER_MAP: &[(&str, CipherSuite)] = &[
    // TLS 1.2 ECDHE suites
    (
        "ECDHE-RSA-AES128-GCM-SHA256",
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    ),
    (
        "ECDHE-RSA-AES256-GCM-SHA384",
        CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    ),
    (
        "ECDHE-RSA-CHACHA20-POLY1305",
        CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    ),
    (
        "ECDHE-ECDSA-AES128-GCM-SHA256",
        CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    ),
    (
        "ECDHE-ECDSA-AES256-GCM-SHA384",
        CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    ),
    (
        "ECDHE-ECDSA-CHACHA20-POLY1305",
        CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    ),
    // TLS 1.3 suites
    (
        "TLS_AES_128_GCM_SHA256",
        CipherSuite::TLS13_AES_128_GCM_SHA256,
    ),
    (
        "TLS_AES_256_GCM_SHA384",
        CipherSuite::TLS13_AES_256_GCM_SHA384,
    ),
    (
        "TLS_CHACHA20_POLY1305_SHA256",
        CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    ),
];

/// Filter cipher suites based on an OpenSSL-style cipher string.
///
/// The cipher string is colon-separated (e.g. `"ECDHE-RSA-AES128-GCM-SHA256:ECDHE-RSA-AES256-GCM-SHA384"`).
/// Returns an error if none of the requested ciphers map to a supported suite.
fn filter_cipher_suites(
    available: &[rustls::SupportedCipherSuite],
    cipher_str: &str,
) -> Result<Vec<rustls::SupportedCipherSuite>, Box<dyn std::error::Error + Send + Sync>> {
    let requested: Vec<&str> = cipher_str.split(':').map(|s| s.trim()).collect();

    // Map OpenSSL names to rustls CipherSuite variants
    let mut matched_suites: Vec<CipherSuite> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();

    for req in &requested {
        if let Some((_, suite)) = CIPHER_MAP.iter().find(|(openssl, _)| openssl == req) {
            matched_suites.push(*suite);
        } else {
            unknown.push(req);
        }
    }

    // Filter available suites to those requested
    let filtered: Vec<rustls::SupportedCipherSuite> = available
        .iter()
        .filter(|suite| matched_suites.contains(&suite.suite()))
        .copied()
        .collect();

    if filtered.is_empty() {
        let available_names: Vec<String> = CIPHER_MAP.iter().map(|(o, _)| o.to_string()).collect();
        return Err(format!(
            "no supported cipher suites match '{}'. Available ciphers: {}",
            cipher_str,
            available_names.join(", ")
        )
        .into());
    }

    if !unknown.is_empty() {
        tracing::warn!(
            "ignoring unsupported cipher(s): {}. Only modern AEAD suites are available.",
            unknown.join(", ")
        );
    }

    Ok(filtered)
}

// ---------------------------------------------------------------------------
// NoVerifier — skip server certificate verification
// ---------------------------------------------------------------------------

/// A `ServerCertVerifier` that accepts any certificate.
///
/// Used when `TlsClientConfig::verify_server` is false. This is required
/// for testing against servers with self-signed or expired certificates.
#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
        ]
    }
}

impl fmt::Display for NoVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NoVerifier")
    }
}
