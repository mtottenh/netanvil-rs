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
}

/// Spawn a reporter thread that prints periodic metrics to stderr.
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
        .spawn(move || {
            run_reporter(&worker_metrics, &shutdown, mode, interval_secs);
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
            ReportMode::None => {}
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
        ReportMode::None => {}
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
