//! Simple authoritative DNS echo server for testing.
//!
//! Responds to any query with NOERROR and a synthetic A record (127.0.0.1).
//! Runs on a background compio thread, binding to a random UDP port.

use std::net::SocketAddr;
use std::time::Duration;

use compio::net::{SocketOpts, UdpSocket};
use compio::BufResult;

use crate::ServerConfig;

/// Handle to a running DNS echo server.
pub struct DnsEchoHandle {
    /// The address the server is listening on.
    pub addr: SocketAddr,
    shutdown: flume::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DnsEchoHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start a DNS echo server on a random port.
pub fn start_dns_echo() -> DnsEchoHandle {
    start_dns_echo_on("127.0.0.1:0")
}

/// Start a DNS echo server on the specified address.
pub fn start_dns_echo_on(addr: &str) -> DnsEchoHandle {
    start_dns_echo_with_config(ServerConfig {
        addr: addr.to_string(),
        ..ServerConfig::udp_default()
    })
}

/// Start a DNS echo server with full configuration.
pub fn start_dns_echo_with_config(config: ServerConfig) -> DnsEchoHandle {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let (shutdown_tx, shutdown_rx) = flume::bounded(1);

    let thread = std::thread::Builder::new()
        .name("dns-echo".into())
        .spawn(move || {
            let rt = compio::runtime::RuntimeBuilder::new().build().unwrap();
            rt.block_on(async {
                let socket_opts = SocketOpts::new()
                    .reuse_address(true)
                    .recv_buffer_size(config.recv_buf_size)
                    .send_buffer_size(config.send_buf_size);

                let sock = UdpSocket::bind_with_options(&config.addr, &socket_opts)
                    .await
                    .unwrap();
                addr_tx.send(sock.local_addr().unwrap()).unwrap();

                loop {
                    // Check shutdown
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }

                    let buf = vec![0u8; 4096];
                    let recv =
                        compio::time::timeout(Duration::from_millis(100), sock.recv_from(buf))
                            .await;

                    if let Ok(BufResult(Ok((n, peer)), buf)) = recv {
                        if n >= 12 {
                            let response = build_response(&buf[..n]);
                            let BufResult(_, _) = sock.send_to(response, peer).await;
                        }
                    }
                }
            });
        })
        .unwrap();

    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("DNS echo server failed to bind");

    DnsEchoHandle {
        addr,
        shutdown: shutdown_tx,
        thread: Some(thread),
    }
}

/// Build a DNS response for any query.
///
/// Copies the query header, sets QR=1 (response), RCODE=0 (NOERROR),
/// copies the question section, and adds an A record pointing to 127.0.0.1.
fn build_response(query: &[u8]) -> Vec<u8> {
    let mut resp = Vec::with_capacity(query.len() + 16);

    // Copy transaction ID
    resp.extend_from_slice(&query[0..2]);

    // Flags: QR=1, RD (copy from query), RA=1, RCODE=0
    let rd = query[2] & 0x01; // copy RD bit
    resp.push(0x80 | rd); // QR=1, Opcode=0, AA=0, TC=0, RD
    resp.push(0x80); // RA=1, Z=0, RCODE=0

    // Counts: QDCOUNT=1, ANCOUNT=1, NSCOUNT=0, ARCOUNT=0
    resp.extend_from_slice(&[0x00, 0x01]); // QDCOUNT
    resp.extend_from_slice(&[0x00, 0x01]); // ANCOUNT
    resp.extend_from_slice(&[0x00, 0x00]); // NSCOUNT
    resp.extend_from_slice(&[0x00, 0x00]); // ARCOUNT

    // Copy question section (skip 12-byte header, find end of question)
    let qsection_start = 12;
    let mut pos = qsection_start;

    // Skip QNAME (sequence of length-prefixed labels ending with 0)
    while pos < query.len() && query[pos] != 0 {
        pos += 1 + query[pos] as usize;
    }
    pos += 1; // skip null terminator
    pos += 4; // skip QTYPE (2) + QCLASS (2)

    let qsection_end = pos.min(query.len());
    resp.extend_from_slice(&query[qsection_start..qsection_end]);

    // Answer section: A record pointing to 127.0.0.1
    // NAME: pointer to offset 12 (start of question QNAME)
    resp.extend_from_slice(&[0xC0, 0x0C]); // compression pointer
    resp.extend_from_slice(&[0x00, 0x01]); // TYPE = A
    resp.extend_from_slice(&[0x00, 0x01]); // CLASS = IN
    resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL = 60s
    resp.extend_from_slice(&[0x00, 0x04]); // RDLENGTH = 4
    resp.extend_from_slice(&[127, 0, 0, 1]); // RDATA = 127.0.0.1

    resp
}
