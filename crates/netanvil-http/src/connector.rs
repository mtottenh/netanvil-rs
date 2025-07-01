//! Custom hyper connector with bandwidth throttling and socket tuning.
//!
//! Bypasses cyper's hardcoded `Connector` to give us control over the stream
//! pipeline. Uses cyper-core's generic `HyperStream<S>`, `CompioExecutor`, and
//! `CompioTimer` for the compio↔hyper bridge.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use compio::net::TcpStream;
use compio::tls::MaybeTlsStream;
use cyper_core::HyperStream;
use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use send_wrapper::SendWrapper;
use tower_service::Service;

use crate::throttle::{MaybeThrottled, ThrottledStream};

/// The stream type seen by hyper: HyperStream wrapping our maybe-throttled,
/// maybe-TLS TCP stream.
pub type ThrottledHyperStream = HyperStream<MaybeThrottled<MaybeTlsStream<TcpStream>>>;

/// Wrapper implementing `hyper_util::client::legacy::connect::Connection` for
/// our throttled stream, so hyper can detect HTTP/2 negotiation.
pub struct ThrottledHttpStream(pub(crate) ThrottledHyperStream);

impl hyper::rt::Read for ThrottledHttpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let inner = std::pin::pin!(&mut self.0);
        inner.poll_read(cx, buf)
    }
}

impl hyper::rt::Write for ThrottledHttpStream {
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

impl Connection for ThrottledHttpStream {
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

/// Extension trait to check ALPN on our MaybeThrottled stream.
trait NegotiatedAlpn {
    fn negotiated_alpn(&self) -> Option<std::borrow::Cow<'_, [u8]>>;
}

impl NegotiatedAlpn for MaybeThrottled<MaybeTlsStream<TcpStream>> {
    fn negotiated_alpn(&self) -> Option<std::borrow::Cow<'_, [u8]>> {
        match self {
            MaybeThrottled::Direct(s) => s.negotiated_alpn(),
            MaybeThrottled::Throttled(s) => s.inner().negotiated_alpn(),
        }
    }
}

/// Custom connector that creates TCP connections with optional bandwidth
/// throttling and OS-level socket tuning.
///
/// Implements `tower_service::Service<Uri>` so it can be used as the
/// connector for `hyper_util::client::legacy::Client`.
#[derive(Clone)]
pub struct ThrottledConnector {
    /// Target bandwidth in bits per second. None = no throttling.
    throttle_bps: Option<u64>,
}

impl ThrottledConnector {
    pub fn new(throttle_bps: Option<u64>) -> Self {
        Self { throttle_bps }
    }

    async fn connect(
        &self,
        uri: Uri,
    ) -> Result<ThrottledHttpStream, Box<dyn std::error::Error + Send + Sync>> {
        let scheme = uri.scheme_str().unwrap_or("http");
        let host = uri.host().ok_or("URI missing host")?;
        // Strip brackets from IPv6 addresses
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host);
        let port = uri.port_u16();

        let (tcp, is_https) = match scheme {
            "http" => {
                let port = port.unwrap_or(80);
                let tcp = TcpStream::connect((host, port)).await?;
                (tcp, false)
            }
            "https" => {
                let port = port.unwrap_or(443);
                let tcp = TcpStream::connect((host, port)).await?;
                (tcp, true)
            }
            other => return Err(format!("unsupported scheme: {other}").into()),
        };

        // Apply per-socket OS-level tuning when throttling is enabled.
        // These are setsockopt calls on THIS socket only — they do NOT
        // affect any global kernel settings or other sockets.
        #[cfg(target_os = "linux")]
        if let Some(bps) = self.throttle_bps {
            use std::os::unix::io::AsRawFd;
            let fd = tcp.as_raw_fd();
            let (rcvbuf, clamp) = crate::throttle::compute_socket_opts(bps);
            if let Err(e) = crate::throttle::set_rcvbuf(fd, rcvbuf) {
                tracing::warn!(rcvbuf, "failed to set SO_RCVBUF: {e}");
            }
            if let Err(e) = crate::throttle::set_window_clamp(fd, clamp) {
                tracing::warn!(clamp, "failed to set TCP_WINDOW_CLAMP: {e}");
            }
        }

        // TLS wrapping
        let stream: MaybeTlsStream<TcpStream> = if is_https {
            let connector = compio::tls::TlsConnector::from(
                compio::native_tls::TlsConnector::new().map_err(io::Error::other)?,
            );
            let tls_stream = connector.connect(host, tcp).await?;
            MaybeTlsStream::new_tls(tls_stream)
        } else {
            MaybeTlsStream::new_plain(tcp)
        };

        // Throttle wrapping
        let maybe_throttled = match self.throttle_bps {
            Some(bps) => MaybeThrottled::Throttled(ThrottledStream::new(stream, bps)),
            None => MaybeThrottled::Direct(stream),
        };

        Ok(ThrottledHttpStream(HyperStream::new(maybe_throttled)))
    }
}

impl Service<Uri> for ThrottledConnector {
    type Response = ThrottledHttpStream;
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
