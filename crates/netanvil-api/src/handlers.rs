use crate::types::*;

/// Build Prometheus text exposition body from shared state.
pub fn build_prometheus_body(state: &SharedState) -> String {
    let node_id = state.get_node_id();
    let label = match &node_id {
        Some(id) => format!("{{node_id=\"{id}\"}}"),
        None => String::new(),
    };

    let m = match state.get_metrics() {
        Some(m) => m,
        None => {
            // Always emit a minimal set of zero-valued metrics so that
            // the node_id label is discoverable by Prometheus at all times.
            let mut out = String::with_capacity(512);
            out.push_str("# HELP netanvil_requests_total Total requests sent.\n");
            out.push_str("# TYPE netanvil_requests_total counter\n");
            out.push_str(&format!("netanvil_requests_total{label} 0\n"));
            out.push_str("# HELP netanvil_errors_total Total errors.\n");
            out.push_str("# TYPE netanvil_errors_total counter\n");
            out.push_str(&format!("netanvil_errors_total{label} 0\n"));
            out.push_str("# HELP netanvil_request_rate Current requests per second.\n");
            out.push_str("# TYPE netanvil_request_rate gauge\n");
            out.push_str(&format!("netanvil_request_rate{label} 0\n"));
            out.push_str("# HELP netanvil_target_rps Target requests per second.\n");
            out.push_str("# TYPE netanvil_target_rps gauge\n");
            out.push_str(&format!("netanvil_target_rps{label} 0\n"));
            out.push_str("# HELP netanvil_error_rate Current error rate (0-1).\n");
            out.push_str("# TYPE netanvil_error_rate gauge\n");
            out.push_str(&format!("netanvil_error_rate{label} 0\n"));
            return out;
        }
    };

    let bucket_prefix = match &node_id {
        Some(id) => format!("{{node_id=\"{id}\",le="),
        None => "{le=".to_string(),
    };

    let mut out = String::with_capacity(2048);

    // Counters
    out.push_str("# HELP netanvil_requests_total Total requests sent.\n");
    out.push_str("# TYPE netanvil_requests_total counter\n");
    out.push_str(&format!(
        "netanvil_requests_total{label} {}\n",
        m.total_requests
    ));

    out.push_str("# HELP netanvil_errors_total Total errors.\n");
    out.push_str("# TYPE netanvil_errors_total counter\n");
    out.push_str(&format!(
        "netanvil_errors_total{label} {}\n",
        m.total_errors
    ));

    // Throughput counters
    out.push_str("# HELP netanvil_bytes_sent_total Total bytes sent to targets.\n");
    out.push_str("# TYPE netanvil_bytes_sent_total counter\n");
    out.push_str(&format!(
        "netanvil_bytes_sent_total{label} {}\n",
        m.bytes_sent
    ));

    out.push_str("# HELP netanvil_bytes_received_total Total bytes received from targets.\n");
    out.push_str("# TYPE netanvil_bytes_received_total counter\n");
    out.push_str(&format!(
        "netanvil_bytes_received_total{label} {}\n",
        m.bytes_received
    ));

    // Gauges
    out.push_str("# HELP netanvil_request_rate Current requests per second.\n");
    out.push_str("# TYPE netanvil_request_rate gauge\n");
    out.push_str(&format!(
        "netanvil_request_rate{label} {:.1}\n",
        m.current_rps
    ));

    out.push_str("# HELP netanvil_target_rps Target requests per second.\n");
    out.push_str("# TYPE netanvil_target_rps gauge\n");
    out.push_str(&format!("netanvil_target_rps{label} {:.1}\n", m.target_rps));

    out.push_str("# HELP netanvil_error_rate Current error rate (0-1).\n");
    out.push_str("# TYPE netanvil_error_rate gauge\n");
    out.push_str(&format!("netanvil_error_rate{label} {:.6}\n", m.error_rate));

    out.push_str("# HELP netanvil_elapsed_seconds Test elapsed time.\n");
    out.push_str("# TYPE netanvil_elapsed_seconds gauge\n");
    out.push_str(&format!(
        "netanvil_elapsed_seconds{label} {:.1}\n",
        m.elapsed_secs
    ));

    // Latency histogram
    out.push_str("# HELP netanvil_latency_nanoseconds Request latency distribution.\n");
    out.push_str("# TYPE netanvil_latency_nanoseconds histogram\n");
    for &(le, count) in &m.latency_buckets {
        if le.is_infinite() {
            out.push_str(&format!(
                "netanvil_latency_nanoseconds_bucket{bucket_prefix}\"+Inf\"}} {count}\n"
            ));
        } else {
            out.push_str(&format!(
                "netanvil_latency_nanoseconds_bucket{bucket_prefix}\"{le}\"}} {count}\n"
            ));
        }
    }
    out.push_str(&format!(
        "netanvil_latency_nanoseconds_count{label} {}\n",
        m.total_requests
    ));
    let approx_sum = m.latency_p50_ms / 1000.0 * m.total_requests as f64;
    out.push_str(&format!(
        "netanvil_latency_nanoseconds_sum{label} {:.3}\n",
        approx_sum
    ));

    // Saturation / client health metrics
    let s = &m.saturation;

    out.push_str(
        "# HELP netanvil_backpressure_drops_total Requests dropped (fire channel full).\n",
    );
    out.push_str("# TYPE netanvil_backpressure_drops_total counter\n");
    out.push_str(&format!(
        "netanvil_backpressure_drops_total{label} {}\n",
        s.backpressure_drops
    ));

    // Decomposed scheduling delay components
    out.push_str(
        "# HELP netanvil_timer_lag_mean_nanoseconds Mean timer thread lag (intended to sent).\n",
    );
    out.push_str("# TYPE netanvil_timer_lag_mean_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_timer_lag_mean_nanoseconds{label} {}\n",
        s.timer_lag_mean_ns
    ));

    out.push_str("# HELP netanvil_timer_lag_max_nanoseconds Max timer thread lag this window.\n");
    out.push_str("# TYPE netanvil_timer_lag_max_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_timer_lag_max_nanoseconds{label} {}\n",
        s.timer_lag_max_ns
    ));

    out.push_str("# HELP netanvil_channel_transit_mean_nanoseconds Mean fire channel transit time (sent to dequeue).\n");
    out.push_str("# TYPE netanvil_channel_transit_mean_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_channel_transit_mean_nanoseconds{label} {}\n",
        s.channel_transit_mean_ns
    ));

    out.push_str("# HELP netanvil_channel_transit_max_nanoseconds Max fire channel transit time this window.\n");
    out.push_str("# TYPE netanvil_channel_transit_max_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_channel_transit_max_nanoseconds{label} {}\n",
        s.channel_transit_max_ns
    ));

    out.push_str("# HELP netanvil_dispatch_gap_mean_nanoseconds Mean dispatch gap (dequeue to exec start).\n");
    out.push_str("# TYPE netanvil_dispatch_gap_mean_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_dispatch_gap_mean_nanoseconds{label} {}\n",
        s.dispatch_gap_mean_ns
    ));

    out.push_str("# HELP netanvil_dispatch_gap_max_nanoseconds Max dispatch gap this window.\n");
    out.push_str("# TYPE netanvil_dispatch_gap_max_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_dispatch_gap_max_nanoseconds{label} {}\n",
        s.dispatch_gap_max_ns
    ));

    out.push_str("# HELP netanvil_scheduling_delay_mean_nanoseconds Mean total scheduling delay (intended to dispatch).\n");
    out.push_str("# TYPE netanvil_scheduling_delay_mean_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_scheduling_delay_mean_nanoseconds{label} {}\n",
        s.scheduling_delay_mean_ns
    ));

    out.push_str("# HELP netanvil_scheduling_delay_max_nanoseconds Max total scheduling delay this window.\n");
    out.push_str("# TYPE netanvil_scheduling_delay_max_nanoseconds gauge\n");
    out.push_str(&format!(
        "netanvil_scheduling_delay_max_nanoseconds{label} {}\n",
        s.scheduling_delay_max_ns
    ));

    out.push_str(
        "# HELP netanvil_rate_achievement Ratio of achieved to target RPS (1.0 = on target).\n",
    );
    out.push_str("# TYPE netanvil_rate_achievement gauge\n");
    out.push_str(&format!(
        "netanvil_rate_achievement{label} {:.4}\n",
        s.rate_achievement
    ));

    out.push_str(
        "# HELP netanvil_client_saturated Whether the client is the bottleneck (1=yes).\n",
    );
    out.push_str("# TYPE netanvil_client_saturated gauge\n");
    let saturated = matches!(
        s.assessment,
        netanvil_types::SaturationAssessment::ClientSaturated
            | netanvil_types::SaturationAssessment::BothSaturated
    );
    out.push_str(&format!(
        "netanvil_client_saturated{label} {}\n",
        if saturated { 1 } else { 0 }
    ));

    // CPU affinity ratio (only emit when there's data)
    if s.cpu_affinity_ratio > 0.0 {
        out.push_str("# HELP netanvil_cpu_affinity_ratio Fraction of sampled reads with correct CPU affinity.\n");
        out.push_str("# TYPE netanvil_cpu_affinity_ratio gauge\n");
        out.push_str(&format!(
            "netanvil_cpu_affinity_ratio{label} {:.4}\n",
            s.cpu_affinity_ratio
        ));
    }

    // TCP health metrics (from sampled TCP_INFO)
    if s.tcp_rtt_mean_ms > 0.0 {
        out.push_str("# HELP netanvil_tcp_rtt_mean_seconds Mean sampled TCP RTT.\n");
        out.push_str("# TYPE netanvil_tcp_rtt_mean_seconds gauge\n");
        out.push_str(&format!(
            "netanvil_tcp_rtt_mean_seconds{label} {:.6}\n",
            s.tcp_rtt_mean_ms / 1000.0
        ));

        out.push_str("# HELP netanvil_tcp_rtt_max_seconds Max sampled TCP RTT.\n");
        out.push_str("# TYPE netanvil_tcp_rtt_max_seconds gauge\n");
        out.push_str(&format!(
            "netanvil_tcp_rtt_max_seconds{label} {:.6}\n",
            s.tcp_rtt_max_ms / 1000.0
        ));

        out.push_str(
            "# HELP netanvil_tcp_retransmit_ratio TCP retransmit ratio from sampled TCP_INFO.\n",
        );
        out.push_str("# TYPE netanvil_tcp_retransmit_ratio gauge\n");
        out.push_str(&format!(
            "netanvil_tcp_retransmit_ratio{label} {:.6}\n",
            s.tcp_retransmit_ratio
        ));
    }

    // Protocol-level packet loss tracking
    if m.packets_sent > 0 {
        out.push_str("# HELP netanvil_packets_sent_total Protocol messages sent.\n");
        out.push_str("# TYPE netanvil_packets_sent_total counter\n");
        out.push_str(&format!(
            "netanvil_packets_sent_total{label} {}\n",
            m.packets_sent
        ));

        out.push_str("# HELP netanvil_packets_received_total Protocol responses received.\n");
        out.push_str("# TYPE netanvil_packets_received_total counter\n");
        out.push_str(&format!(
            "netanvil_packets_received_total{label} {}\n",
            m.packets_received
        ));

        out.push_str("# HELP netanvil_packets_lost_total Protocol messages declared lost.\n");
        out.push_str("# TYPE netanvil_packets_lost_total counter\n");
        out.push_str(&format!(
            "netanvil_packets_lost_total{label} {}\n",
            m.packets_lost
        ));
    }

    out
}
