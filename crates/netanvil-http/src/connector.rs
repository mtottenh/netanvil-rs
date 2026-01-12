//! Custom hyper connector with bandwidth throttling and socket tuning.
//!
//! Bypasses cyper's hardcoded `Connector` to give us control over the stream
//! pipeline. Uses cyper-core's generic `HyperStream<S>`, `CompioExecutor`, and
//! `CompioTimer` for the compio↔hyper bridge.
//!
//! The connector is generic over the inner stream type `S` (defaulting to
//! `TcpStream`). When health sampling is enabled, `S = ObservedTcpStream`.

use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::os::unix::io::AsRawFd;
use std::pin::Pin;
use std::task::{Context, Poll};

use compio::io::{AsyncRead, AsyncWrite};
use compio::net::TcpStream;
use compio::tls::MaybeTlsStream;
use cyper_core::HyperStream;
use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use netanvil_types::config::DnsMode;
use send_wrapper::SendWrapper;
use tower_service::Service;

use crate::observed::{ObservedTcpStream, SendObserveConfig};
use crate::throttle::{MaybeThrottled, ThrottledStream};

/// The stream type seen by hyper, generic over the inner TCP stream.
pub type GenericHyperStream<S> = HyperStream<MaybeThrottled<MaybeTlsStream<S>>>;

/// Concrete type aliases for the two configurations.
pub type ThrottledHyperStream = GenericHyperStream<TcpStream>;
pub type ObservedHyperStream = GenericHyperStream<ObservedTcpStream>;

/// Wrapper implementing `hyper_util::client::legacy::connect::Connection` for
/// our throttled stream, so hyper can detect HTTP/2 negotiation.
pub struct ThrottledHttpStream<S = TcpStream>(pub(crate) GenericHyperStream<S>);

impl<S> hyper::rt::Read for ThrottledHttpStream<S>
where
    GenericHyperStream<S>: hyper::rt::Read + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let inner = std::pin::pin!(&mut self.0);
        inner.poll_read(cx, buf)
    }
}

impl<S> hyper::rt::Write for ThrottledHttpStream<S>
where
    GenericHyperStream<S>: hyper::rt::Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let inner = std::pin::pin!(&mut self.0);
        inner.poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = std::pin::pin!(&mut self.0);
        inner.poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let inner = std::pin::pin!(&mut self.0);
        inner.poll_shutdown(cx)
    }
}

/// Extension trait to check ALPN on our MaybeThrottled stream.
trait NegotiatedAlpn {
    fn negotiated_alpn(&self) -> Option<std::borrow::Cow<'_, [u8]>>;
}

impl<S> NegotiatedAlpn for MaybeThrottled<MaybeTlsStream<S>> {
    fn negotiated_alpn(&self) -> Option<std::borrow::Cow<'_, [u8]>> {
        match self {
            MaybeThrottled::Direct(s) => s.negotiated_alpn(),
            MaybeThrottled::Throttled(s) => s.inner().negotiated_alpn(),
        }
    }
}

impl<S> Connection for ThrottledHttpStream<S>
where
    S: 'static,
{
    fn connected(&self) -> Connected {
        let conn = Connected::new();
        let is_h2 = self
            .0
            .get_ref()
            .negotiated_alpn()
            .map(|alpn| *alpn == *b"h2")
            .unwrap_or_default();
        if is_h2 {
            conn.negotiated_h2()
        } else {
            conn
        }
    }
}

/// Trait abstracting the TCP → S transformation, allowing the connector
/// to be generic over plain `TcpStream` vs `ObservedTcpStream`.
pub trait WrapTcp: Clone + Send + Sync + 'static {
    /// The output stream type.
    type Stream: AsyncRead + AsyncWrite + Unpin + AsRawFd + 'static;

    /// Wrap a freshly-connected `TcpStream`.
    fn wrap(&self, tcp: TcpStream) -> Self::Stream;
}

/// Identity wrapper — passes `TcpStream` through unchanged.
#[derive(Clone)]
pub struct Identity;

impl WrapTcp for Identity {
    type Stream = TcpStream;

    #[inline]
    fn wrap(&self, tcp: TcpStream) -> TcpStream {
        tcp
    }
}

/// Wraps `TcpStream` in `ObservedTcpStream` for health sampling.
#[derive(Clone)]
pub struct Observe(pub SendObserveConfig);

impl WrapTcp for Observe {
    type Stream = ObservedTcpStream;

    #[inline]
    fn wrap(&self, tcp: TcpStream) -> ObservedTcpStream {
        ObservedTcpStream::new(tcp, self.0.get())
    }
}

/// Custom connector that creates TCP connections with optional bandwidth
/// throttling, OS-level socket tuning, configurable TLS, and optional
/// health observation.
///
/// Generic over `W: WrapTcp` which determines the inner stream type.
/// Default `W = Identity` gives plain `TcpStream` (zero overhead).
#[derive(Clone)]
pub struct ThrottledConnector<W: WrapTcp = Identity> {
    throttle_bps: Option<u64>,
    tls_connector: Option<compio::tls::TlsConnector>,
    sni_override: Option<String>,
    noreuse: bool,
    dns_mode: Option<DnsMode>,
    wrapper: W,
}

impl ThrottledConnector<Identity> {
    pub fn new(throttle_bps: Option<u64>) -> Self {
        Self {
            throttle_bps,
            tls_connector: None,
            sni_override: None,
            noreuse: false,
            dns_mode: None,
            wrapper: Identity,
        }
    }

    pub fn with_tls(
        throttle_bps: Option<u64>,
        tls_connector: compio::tls::TlsConnector,
        sni_override: Option<String>,
    ) -> Self {
        Self {
            throttle_bps,
            tls_connector: Some(tls_connector),
            sni_override,
            noreuse: false,
            dns_mode: None,
            wrapper: Identity,
        }
    }
}

impl<W: WrapTcp> ThrottledConnector<W> {
    /// Replace the TCP wrapper, producing a connector with a different
    /// stream type. Preserves all other configuration.
    pub fn with_wrapper<W2: WrapTcp>(self, wrapper: W2) -> ThrottledConnector<W2> {
        ThrottledConnector {
            throttle_bps: self.throttle_bps,
            tls_connector: self.tls_connector,
            sni_override: self.sni_override,
            noreuse: self.noreuse,
            dns_mode: self.dns_mode,
            wrapper,
        }
    }

    pub fn set_noreuse(&mut self, noreuse: bool) {
        self.noreuse = noreuse;
    }

    pub fn set_dns_mode(&mut self, dns_mode: Option<DnsMode>) {
        self.dns_mode = dns_mode;
    }

    fn resolve_with_dns_mode(
        host: &str,
        port: u16,
        dns_mode: &DnsMode,
    ) -> Result<SocketAddr, Box<dyn std::error::Error + Send + Sync>> {
        use std::net::ToSocketAddrs;

        let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(format!("DNS resolution failed for {host}:{port}").into());
        }

        match dns_mode {
            DnsMode::V4 => addrs
                .iter()
                .find(|a| a.is_ipv4())
                .copied()
                .ok_or_else(|| format!("no IPv4 address for {host}").into()),
            DnsMode::V6 => addrs
                .iter()
                .find(|a| a.is_ipv6())
                .copied()
                .ok_or_else(|| format!("no IPv6 address for {host}").into()),
            DnsMode::PreferV4 => {
                let v4 = addrs.iter().find(|a| a.is_ipv4());
                let v6 = addrs.iter().find(|a| a.is_ipv6());
                Ok(*v4.or(v6).unwrap())
            }
            DnsMode::PreferV6 => {
                let v6 = addrs.iter().find(|a| a.is_ipv6());
                let v4 = addrs.iter().find(|a| a.is_ipv4());
                Ok(*v6.or(v4).unwrap())
            }
        }
    }

    async fn connect(
        &self,
        uri: Uri,
    ) -> Result<ThrottledHttpStream<W::Stream>, Box<dyn std::error::Error + Send + Sync>> {
        let scheme = uri.scheme_str().unwrap_or("http");
        let host = uri.host().ok_or("URI missing host")?;
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        let port = uri.port_u16();

        let (tcp, is_https) = match scheme {
            "http" => {
                let port = port.unwrap_or(80);
                let tcp = if let Some(ref dns_mode) = self.dns_mode {
                    let addr = Self::resolve_with_dns_mode(host, port, dns_mode)?;
                    TcpStream::connect(addr).await?
                } else {
                    TcpStream::connect((host, port)).await?
                };
                (tcp, false)
            }
            "https" => {
                let port = port.unwrap_or(443);
                let tcp = if let Some(ref dns_mode) = self.dns_mode {
                    let addr = Self::resolve_with_dns_mode(host, port, dns_mode)?;
                    TcpStream::connect(addr).await?
                } else {
                    TcpStream::connect((host, port)).await?
                };
                (tcp, true)
            }
            other => return Err(format!("unsupported scheme: {other}").into()),
        };

        #[cfg(target_os = "linux")]
        if let Some(bps) = self.throttle_bps {
            let fd = tcp.as_raw_fd();
            let (rcvbuf, clamp) = crate::throttle::compute_socket_opts(bps);
            if let Err(e) = crate::throttle::set_rcvbuf(fd, rcvbuf) {
                tracing::warn!(rcvbuf, "failed to set SO_RCVBUF: {e}");
            }
            if let Err(e) = crate::throttle::set_window_clamp(fd, clamp) {
                tracing::warn!(clamp, "failed to set TCP_WINDOW_CLAMP: {e}");
            }
        }

        #[cfg(target_os = "linux")]
        if self.noreuse {
            use std::os::unix::io::FromRawFd;
            let fd = tcp.as_raw_fd();
            let dup_fd = unsafe { libc::dup(fd) };
            if dup_fd >= 0 {
                let owned = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(dup_fd) };
                std::mem::forget(owned);
            }
        }

        // Apply the TCP wrapper (Identity or Observe) BEFORE TLS.
        let stream = self.wrapper.wrap(tcp);

        // TLS wrapping
        let stream: MaybeTlsStream<W::Stream> = if is_https {
            let connector = match &self.tls_connector {
                Some(c) => c.clone(),
                None => compio::tls::TlsConnector::from(
                    compio::native_tls::TlsConnector::new().map_err(io::Error::other)?,
                ),
            };
            let sni_host = self.sni_override.as_deref().unwrap_or(host);
            let tls_stream = connector.connect(sni_host, stream).await?;
            MaybeTlsStream::new_tls(tls_stream)
        } else {
            MaybeTlsStream::new_plain(stream)
        };

        // Throttle wrapping
        let maybe_throttled = match self.throttle_bps {
            Some(bps) => MaybeThrottled::Throttled(ThrottledStream::new(stream, bps)),
            None => MaybeThrottled::Direct(stream),
        };

        Ok(ThrottledHttpStream(HyperStream::new(maybe_throttled)))
    }
}

impl<W> Service<Uri> for ThrottledConnector<W>
where
    W: WrapTcp,
    W::Stream: Unpin,
{
    type Response = ThrottledHttpStream<W::Stream>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let this = self.clone();
        Box::pin(SendWrapper::new(async move { this.connect(uri).await }))
    }
}
