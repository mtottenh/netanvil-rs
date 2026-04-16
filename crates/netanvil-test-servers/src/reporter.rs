//! Periodic metrics reporting — text (iperf3-style) and JSON modes.
//!
//! The reporter runs on a dedicated std::thread, reading per-worker
//! `Arc<WorkerMetrics>` at a configurable interval and printing delta
//! summaries to stderr.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::metrics::{WorkerMetrics, WorkerSnapshot};

/// Output mode for server-side metrics reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReportMode {
    /// No periodic reporting.
    #[default]
    None,
    /// iperf3-style text output to stderr.
    Text,
    /// One JSON object per line per interval to stderr.
    Json,
    /// Prometheus exposition format served over HTTP on the given port.
    /// Metrics are read on-demand when scraped, no periodic timer needed.
    Prometheus { port: u16 },
}

/// Spawn a reporter thread that prints periodic metrics to stderr,
/// or serves a Prometheus HTTP endpoint.
///
/// Returns the thread handle.  The thread exits when `shutdown` is set.
pub(crate) fn spawn_reporter(
    worker_metrics: Vec<Arc<WorkerMetrics>>,
    shutdown: Arc<AtomicBool>,
    mode: ReportMode,
    interval_secs: f64,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("metrics-reporter".into())
        .spawn(move || match mode {
            ReportMode::Prometheus { port } => {
                run_prometheus(&worker_metrics, &shutdown, port);
            }
            _ => {
                run_reporter(&worker_metrics, &shutdown, mode, interval_secs);
            }
        })
        .expect("failed to spawn metrics reporter thread")
}

fn run_reporter(
    worker_metrics: &[Arc<WorkerMetrics>],
    shutdown: &AtomicBool,
    mode: ReportMode,
    interval_secs: f64,
) {
    let interval = Duration::from_secs_f64(interval_secs);
    let start = Instant::now();

    // Take initial snapshot as the baseline for deltas.
    let mut prev: Vec<WorkerSnapshot> = worker_metrics.iter().map(|m| m.snapshot()).collect();
    let mut prev_time = start;

    if mode == ReportMode::Text {
        print_text_header();
    }

    loop {
        std::thread::sleep(interval);
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(start).as_secs_f64();
        let dt = now.duration_since(prev_time).as_secs_f64().max(0.001);
        let current: Vec<WorkerSnapshot> = worker_metrics.iter().map(|m| m.snapshot()).collect();

        match mode {
            ReportMode::Text => format_text(&current, &prev, elapsed, dt),
            ReportMode::Json => format_json(&current, &prev, elapsed, dt),
            ReportMode::None | ReportMode::Prometheus { .. } => {}
        }

        prev = current;
        prev_time = now;
    }

    // Final summary.
    let now = Instant::now();
    let total_secs = now.duration_since(start).as_secs_f64().max(0.001);
    let current: Vec<WorkerSnapshot> = worker_metrics.iter().map(|m| m.snapshot()).collect();
    let zeros: Vec<WorkerSnapshot> = vec![WorkerSnapshot::default(); current.len()];

    match mode {
        ReportMode::Text => {
            let stderr = std::io::stderr();
            let mut out = stderr.lock();
            let _ = writeln!(out, "- - - - - - - - - - - - - - - - - - - - - - - -");
            format_text_rows(&current, &zeros, total_secs, total_secs, &mut out);
        }
        ReportMode::Json => format_json(&current, &zeros, total_secs, total_secs),
        ReportMode::None | ReportMode::Prometheus { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Text formatter (iperf3-style)
// ---------------------------------------------------------------------------

fn print_text_header() {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    let _ = writeln!(
        out,
        "[ ID] Interval           Transfer     Bitrate         Reqs   Conns  Errs  \
         RTTavg  Retrans"
    );
}

fn format_text(current: &[WorkerSnapshot], prev: &[WorkerSnapshot], elapsed: f64, dt: f64) {
    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    format_text_rows(current, prev, elapsed, dt, &mut out);
}

fn format_text_rows(
    current: &[WorkerSnapshot],
    prev: &[WorkerSnapshot],
    elapsed: f64,
    dt: f64,
    out: &mut impl Write,
) {
    let interval_start = elapsed - dt;

    // Per-worker delta rows.
    let mut sum = DeltaRow::default();
    for (i, (cur, prv)) in current.iter().zip(prev.iter()).enumerate() {
        let d = delta(cur, prv);
        let _ = writeln!(
            out,
            "[{:>3}] {:.2}-{:.2} sec  {:>10}  {:>12}  {:>7}  {:>6}  {:>4}  {:>6}  {:>7}",
            i,
            interval_start,
            elapsed,
            human_bytes(d.bytes_total),
            human_bitrate(d.bytes_total, dt),
            d.requests,
            d.connections,
            d.errors,
            human_rtt_us(d.rtt_sum_us, d.rtt_count),
            d.retransmits,
        );
        sum.bytes_total += d.bytes_total;
        sum.requests += d.requests;
        sum.connections += d.connections;
        sum.errors += d.errors;
        sum.rtt_sum_us += d.rtt_sum_us;
        sum.rtt_count += d.rtt_count;
        sum.retransmits += d.retransmits;
    }

    // SUM row.
    if current.len() > 1 {
        let _ = writeln!(
            out,
            "[SUM] {:.2}-{:.2} sec  {:>10}  {:>12}  {:>7}  {:>6}  {:>4}  {:>6}  {:>7}",
            interval_start,
            elapsed,
            human_bytes(sum.bytes_total),
            human_bitrate(sum.bytes_total, dt),
            sum.requests,
            sum.connections,
            sum.errors,
            human_rtt_us(sum.rtt_sum_us, sum.rtt_count),
            sum.retransmits,
        );
    }
}

#[derive(Default)]
struct DeltaRow {
    bytes_total: u64,
    requests: u64,
    connections: u64,
    errors: u64,
    rtt_sum_us: u64,
    rtt_count: u64,
    retransmits: u64,
}

fn delta(cur: &WorkerSnapshot, prev: &WorkerSnapshot) -> DeltaRow {
    let bytes_sent = cur.bytes_sent.saturating_sub(prev.bytes_sent);
    let bytes_recv = cur.bytes_received.saturating_sub(prev.bytes_received);
    DeltaRow {
        bytes_total: bytes_sent + bytes_recv,
        requests: cur
            .requests_completed
            .saturating_sub(prev.requests_completed),
        connections: cur
            .connections_accepted
            .saturating_sub(prev.connections_accepted),
        errors: cur.errors.saturating_sub(prev.errors),
        rtt_sum_us: cur.tcp_rtt_sum_us.saturating_sub(prev.tcp_rtt_sum_us),
        rtt_count: cur.tcp_rtt_count.saturating_sub(prev.tcp_rtt_count),
        retransmits: cur.tcp_retransmits.saturating_sub(prev.tcp_retransmits),
    }
}

fn human_bytes(b: u64) -> String {
    if b >= 1_073_741_824 {
        format!("{:.1} GiB", b as f64 / 1_073_741_824.0)
    } else if b >= 1_048_576 {
        format!("{:.1} MiB", b as f64 / 1_048_576.0)
    } else if b >= 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else {
        format!("{} B", b)
    }
}

fn human_bitrate(bytes: u64, dt: f64) -> String {
    let bits = bytes as f64 * 8.0;
    let bps = bits / dt;
    if bps >= 1e9 {
        format!("{:.2} Gbit/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.1} Mbit/s", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.0} Kbit/s", bps / 1e3)
    } else {
        format!("{:.0} bit/s", bps)
    }
}

fn human_rtt_us(sum: u64, count: u64) -> String {
    if count == 0 {
        return "-".to_string();
    }
    let avg = sum as f64 / count as f64;
    if avg >= 1000.0 {
        format!("{:.1}ms", avg / 1000.0)
    } else {
        format!("{:.0}us", avg)
    }
}

// ---------------------------------------------------------------------------
// JSON formatter
// ---------------------------------------------------------------------------

fn format_json(current: &[WorkerSnapshot], prev: &[WorkerSnapshot], elapsed: f64, dt: f64) {
    use std::fmt::Write as FmtWrite;

    let interval_start = elapsed - dt;

    let mut sum_bytes_sent: u64 = 0;
    let mut sum_bytes_recv: u64 = 0;
    let mut sum_requests: u64 = 0;
    let mut sum_conns: u64 = 0;
    let mut sum_errors: u64 = 0;
    let mut sum_dgram_recv: u64 = 0;
    let mut sum_dgram_sent: u64 = 0;
    let mut sum_rtt_sum: u64 = 0;
    let mut sum_rtt_count: u64 = 0;
    let mut sum_retransmits: u64 = 0;
    let mut sum_lost: u64 = 0;

    // Build per-worker array.
    let mut workers_json = String::from("[");
    for (i, (cur, prv)) in current.iter().zip(prev.iter()).enumerate() {
        let bs = cur.bytes_sent.saturating_sub(prv.bytes_sent);
        let br = cur.bytes_received.saturating_sub(prv.bytes_received);
        let rq = cur
            .requests_completed
            .saturating_sub(prv.requests_completed);
        let cn = cur
            .connections_accepted
            .saturating_sub(prv.connections_accepted);
        let er = cur.errors.saturating_sub(prv.errors);
        let dr = cur
            .datagrams_received
            .saturating_sub(prv.datagrams_received);
        let ds = cur.datagrams_sent.saturating_sub(prv.datagrams_sent);
        let rs = cur.tcp_rtt_sum_us.saturating_sub(prv.tcp_rtt_sum_us);
        let rc = cur.tcp_rtt_count.saturating_sub(prv.tcp_rtt_count);
        let rt = cur.tcp_retransmits.saturating_sub(prv.tcp_retransmits);
        let lo = cur.tcp_lost.saturating_sub(prv.tcp_lost);

        sum_bytes_sent += bs;
        sum_bytes_recv += br;
        sum_requests += rq;
        sum_conns += cn;
        sum_errors += er;
        sum_dgram_recv += dr;
        sum_dgram_sent += ds;
        sum_rtt_sum += rs;
        sum_rtt_count += rc;
        sum_retransmits += rt;
        sum_lost += lo;

        if i > 0 {
            workers_json.push(',');
        }
        let _ = write!(
            workers_json,
            "{{\"id\":{},\"bytes_sent\":{},\"bytes_received\":{},\
             \"requests\":{},\"connections\":{},\"errors\":{},\
             \"datagrams_received\":{},\"datagrams_sent\":{}}}",
            i, bs, br, rq, cn, er, dr, ds
        );
    }
    workers_json.push(']');

    let rtt_avg = if sum_rtt_count > 0 {
        sum_rtt_sum as f64 / sum_rtt_count as f64
    } else {
        0.0
    };
    let bitrate = (sum_bytes_sent + sum_bytes_recv) as f64 * 8.0 / dt;

    let stderr = std::io::stderr();
    let mut out = stderr.lock();
    let _ = writeln!(
        out,
        "{{\"interval_start\":{:.2},\"interval_end\":{:.2},\
         \"bytes_sent\":{},\"bytes_received\":{},\"bitrate_bps\":{:.0},\
         \"requests\":{},\"connections\":{},\"errors\":{},\
         \"datagrams_received\":{},\"datagrams_sent\":{},\
         \"tcp_rtt_avg_us\":{:.1},\"tcp_retransmits\":{},\"tcp_lost\":{},\
         \"workers\":{}}}",
        interval_start,
        elapsed,
        sum_bytes_sent,
        sum_bytes_recv,
        bitrate,
        sum_requests,
        sum_conns,
        sum_errors,
        sum_dgram_recv,
        sum_dgram_sent,
        rtt_avg,
        sum_retransmits,
        sum_lost,
        workers_json,
    );
}

// ---------------------------------------------------------------------------
// Prometheus exposition format served over HTTP
// ---------------------------------------------------------------------------

/// Run a blocking HTTP server that serves `/metrics` in Prometheus
/// exposition format.  Reads `WorkerMetrics` on demand per scrape.
/// No periodic timer — Prometheus controls the scrape interval.
fn run_prometheus(worker_metrics: &[Arc<WorkerMetrics>], shutdown: &AtomicBool, port: u16) {
    use std::net::TcpListener;

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| {
        panic!("prometheus: failed to bind to {}: {}", addr, e);
    });
    listener
        .set_nonblocking(true)
        .expect("prometheus: set_nonblocking failed");

    tracing::info!(
        "Prometheus metrics endpoint listening on http://{}/metrics",
        addr
    );

    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                handle_prometheus_request(stream, worker_metrics);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("prometheus: accept error: {}", e);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_prometheus_request(mut stream: std::net::TcpStream, metrics: &[Arc<WorkerMetrics>]) {
    // Read the request (we don't parse it — serve /metrics for everything).
    let mut req_buf = [0u8; 1024];
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = std::io::Read::read(&mut stream, &mut req_buf);

    let body = format_prometheus(metrics);
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/plain; version=0.0.4; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn format_prometheus(metrics: &[Arc<WorkerMetrics>]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(4096);

    // Helper: emit one counter metric with per-worker labels.
    macro_rules! counter {
        ($name:expr, $help:expr, $field:ident) => {
            let _ = writeln!(out, "# HELP {} {}", $name, $help);
            let _ = writeln!(out, "# TYPE {} counter", $name);
            let mut total: u64 = 0;
            for (i, m) in metrics.iter().enumerate() {
                let v = m.$field.load(std::sync::atomic::Ordering::Relaxed);
                total += v;
                let _ = writeln!(out, "{}{{worker=\"{}\"}} {}", $name, i, v);
            }
            let _ = writeln!(out, "{} {}", $name, total);
        };
    }

    counter!(
        "netanvil_server_bytes_sent_total",
        "Total bytes sent to clients.",
        bytes_sent
    );
    counter!(
        "netanvil_server_bytes_received_total",
        "Total bytes received from clients.",
        bytes_received
    );
    counter!(
        "netanvil_server_connections_accepted_total",
        "Total TCP connections accepted.",
        connections_accepted
    );
    counter!(
        "netanvil_server_connections_closed_total",
        "Total TCP connections closed.",
        connections_closed
    );
    counter!(
        "netanvil_server_requests_completed_total",
        "Total request-response cycles completed.",
        requests_completed
    );
    counter!(
        "netanvil_server_errors_total",
        "Total I/O errors encountered.",
        errors
    );
    counter!(
        "netanvil_server_datagrams_received_total",
        "Total UDP/DNS datagrams received.",
        datagrams_received
    );
    counter!(
        "netanvil_server_datagrams_sent_total",
        "Total UDP/DNS datagrams sent.",
        datagrams_sent
    );
    counter!(
        "netanvil_server_datagrams_dropped_total",
        "Total UDP/DNS datagrams dropped (send failures).",
        datagrams_dropped
    );
    counter!(
        "netanvil_server_tcp_retransmits_total",
        "Total TCP retransmits from TCP_INFO sampling.",
        tcp_retransmits
    );
    counter!(
        "netanvil_server_tcp_lost_total",
        "Total TCP lost segments from TCP_INFO sampling.",
        tcp_lost
    );

    // Derived gauge: connections currently active (accepted - closed).
    {
        let _ = writeln!(
            out,
            "# HELP netanvil_server_connections_active Current active TCP connections."
        );
        let _ = writeln!(out, "# TYPE netanvil_server_connections_active gauge");
        let mut total_accepted: u64 = 0;
        let mut total_closed: u64 = 0;
        for (i, m) in metrics.iter().enumerate() {
            let acc = m
                .connections_accepted
                .load(std::sync::atomic::Ordering::Relaxed);
            let cls = m
                .connections_closed
                .load(std::sync::atomic::Ordering::Relaxed);
            let active = acc.saturating_sub(cls);
            total_accepted += acc;
            total_closed += cls;
            let _ = writeln!(
                out,
                "netanvil_server_connections_active{{worker=\"{}\"}} {}",
                i, active
            );
        }
        let _ = writeln!(
            out,
            "netanvil_server_connections_active {}",
            total_accepted.saturating_sub(total_closed)
        );
    }

    // Derived gauge: average TCP RTT in microseconds.
    {
        let _ = writeln!(
            out,
            "# HELP netanvil_server_tcp_rtt_avg_us Average smoothed TCP RTT (microseconds)."
        );
        let _ = writeln!(out, "# TYPE netanvil_server_tcp_rtt_avg_us gauge");
        let mut total_sum: u64 = 0;
        let mut total_count: u64 = 0;
        for m in metrics.iter() {
            total_sum += m.tcp_rtt_sum_us.load(std::sync::atomic::Ordering::Relaxed);
            total_count += m.tcp_rtt_count.load(std::sync::atomic::Ordering::Relaxed);
        }
        let avg = if total_count > 0 {
            total_sum as f64 / total_count as f64
        } else {
            0.0
        };
        let _ = writeln!(out, "netanvil_server_tcp_rtt_avg_us {:.1}", avg);
    }

    // Gauge: max TCP RTT.
    {
        let _ = writeln!(
            out,
            "# HELP netanvil_server_tcp_rtt_max_us Maximum smoothed TCP RTT (microseconds)."
        );
        let _ = writeln!(out, "# TYPE netanvil_server_tcp_rtt_max_us gauge");
        let mut max_rtt: u64 = 0;
        for m in metrics.iter() {
            let v = m.tcp_rtt_max_us.load(std::sync::atomic::Ordering::Relaxed);
            if v > max_rtt {
                max_rtt = v;
            }
        }
        let _ = writeln!(out, "netanvil_server_tcp_rtt_max_us {}", max_rtt);
    }

    out
}
