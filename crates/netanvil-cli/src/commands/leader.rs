use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};

use netanvil_distributed::{LeaderMetricsState, LeaderServer, QueueConfig, ResultStore, TestQueue};
use netanvil_types::{ConnectionConfig, SchedulerConfig, TestConfig};

use crate::parsing::*;

#[allow(clippy::too_many_arguments)]
pub fn run(
    workers: Vec<String>,
    url: Vec<String>,
    plugin: Option<String>,
    plugin_type: String,
    method: String,
    rps: f64,
    duration: String,
    rate_mode: String,
    steps: Option<String>,
    pid_metric: String,
    pid_target: f64,
    pid_kp: Option<f64>,
    pid_ki: Option<f64>,
    pid_kd: Option<f64>,
    pid_min_rps: f64,
    pid_max_rps: f64,
    pid_constraints: Vec<String>,
    pid_autotune_duration: String,
    cores: usize,
    timeout: String,
    headers: Vec<String>,
    error_threshold: u16,
    external_metrics_url: Option<String>,
    external_metrics_field: Option<String>,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    http_version: String,
    response_signals: Vec<String>,
    payload: Option<String>,
    payload_hex: Option<String>,
    payload_file: Option<String>,
    framing: String,
    no_response: bool,
    mode: String,
    request_size: Option<String>,
    response_size: Option<String>,
    dns_domains: Option<String>,
    dns_query_type: String,
    ramp_warmup: String,
    ramp_multiplier: f64,
    ramp_max_errors: f64,
    baseline_floor: f64,
    latency_limit: Option<f64>,
    error_rate_limit: Option<f64>,
    latency_setpoint: Option<f64>,
    rate_config: Option<String>,
    metrics_port: Option<u16>,
    output: String,
    health_sample_rate: f64,
    control_trace: Option<String>,
) -> Result<()> {
    let output_format =
        crate::output::OutputFormat::parse(&output).map_err(|e| anyhow::anyhow!(e))?;
    let duration = parse_duration(&duration).context("invalid --duration")?;
    let timeout = parse_duration(&timeout).context("invalid --timeout")?;
    let autotune_dur =
        parse_duration(&pid_autotune_duration).context("invalid --pid-autotune-duration")?;
    let ramp_warmup_dur = parse_duration(&ramp_warmup).context("invalid --ramp-warmup")?;
    let http_version = parse_http_version(&http_version).context("invalid --http-version")?;
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|h| parse_header(h))
        .collect::<Result<_>>()?;
    let rate = build_rate_config(
        &rate_mode,
        rps,
        steps.as_deref(),
        &PidArgs {
            metric: &pid_metric,
            target: pid_target,
            kp: pid_kp,
            ki: pid_ki,
            kd: pid_kd,
            min_rps: pid_min_rps,
            max_rps: pid_max_rps,
            constraints: &pid_constraints,
            autotune_duration: autotune_dur,
        },
        &RampArgs {
            warmup_duration: ramp_warmup_dur,
            latency_multiplier: ramp_multiplier,
            max_error_rate: ramp_max_errors,
            baseline_floor_ms: baseline_floor,
        },
        &AdaptiveShortcutArgs {
            latency_limit_ms: latency_limit,
            error_rate_limit_pct: error_rate_limit,
            latency_setpoint_ms: latency_setpoint,
            config_file: rate_config.clone(),
        },
    )?;

    // Build plugin config: read the file locally and embed it for agents
    let plugin_config = if let Some(ref plugin_path) = plugin {
        let ptype = detect_plugin_type(plugin_path, &plugin_type)?;
        let source =
            std::fs::read(plugin_path).context(format!("failed to read plugin: {plugin_path}"))?;
        tracing::info!(
            plugin = %plugin_path,
            plugin_type = ?ptype,
            size = source.len(),
            "embedding plugin for distribution to agents"
        );
        Some(netanvil_types::PluginConfig {
            plugin_type: ptype,
            source,
        })
    } else {
        None
    };

    let mut config = TestConfig {
        targets: url,
        method,
        duration,
        rate,
        scheduler: SchedulerConfig::ConstantRate,
        headers,
        num_cores: cores,
        connections: ConnectionConfig {
            request_timeout: timeout,
            ..Default::default()
        },
        control_interval: Duration::from_secs(3),
        error_status_threshold: error_threshold,
        external_metrics_url: external_metrics_url.clone(),
        external_metrics_field: external_metrics_field.clone(),
        bandwidth_limit_bps: None,
        http_version,
        plugin: plugin_config,
        response_signal_headers: parse_response_signals(&response_signals)?,
        health_sample_rate,
        control_trace,
        ..Default::default()
    };

    // Detect protocol from URLs and populate config.protocol for TCP/UDP
    let protocol = detect_protocol(&config.targets);
    match protocol {
        DetectedProtocol::Tcp => {
            let tcp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                .context("invalid TCP payload")?;
            let _tcp_mode = parse_tcp_mode(&mode).context("invalid --mode")?;
            let req_size_dist = match &request_size {
                Some(s) => parse_u16_distribution(s).context("invalid --request-size")?,
                None => netanvil_types::ValueDistribution::Fixed(tcp_payload.len() as u16),
            };
            let resp_size_dist = match &response_size {
                Some(s) => parse_u32_distribution(s).context("invalid --response-size")?,
                None => match &req_size_dist {
                    netanvil_types::ValueDistribution::Fixed(n) => {
                        netanvil_types::ValueDistribution::Fixed(*n as u32)
                    }
                    _ => netanvil_types::ValueDistribution::Fixed(tcp_payload.len() as u32),
                },
            };
            config.protocol = Some(netanvil_types::ProtocolConfig::Tcp {
                mode: mode.clone(),
                payload_hex: encode_hex(&tcp_payload),
                framing: framing.clone(),
                request_size: req_size_dist,
                response_size: resp_size_dist,
            });
            config.error_status_threshold = 0;
        }
        DetectedProtocol::Udp => {
            let udp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                .context("invalid UDP payload")?;
            config.protocol = Some(netanvil_types::ProtocolConfig::Udp {
                payload_hex: encode_hex(&udp_payload),
                expect_response: !no_response,
            });
            config.error_status_threshold = 0;
        }
        DetectedProtocol::Dns => {
            let domains = dns_domains.as_deref().unwrap_or("example.com");
            config.protocol = Some(netanvil_types::ProtocolConfig::Dns {
                domains: domains.to_string(),
                query_type: dns_query_type.clone(),
                recursion: true,
            });
            config.error_status_threshold = 0;
        }
        DetectedProtocol::Redis => {
            config.protocol = Some(netanvil_types::ProtocolConfig::Redis {
                password: None,
                db: None,
                command: "PING".into(),
                args: vec![],
            });
            config.error_status_threshold = 0;
        }
        DetectedProtocol::Http => {}
    }

    let tls_config = match (&tls_ca, &tls_cert, &tls_key) {
        (Some(ca), Some(cert), Some(key)) => Some(netanvil_types::TlsConfig {
            ca_cert: ca.clone(),
            cert: cert.clone(),
            key: key.clone(),
        }),
        _ => None,
    };

    let summary = format!(
        "{} {:.0} RPS, {:.0}s, {} target(s)",
        rate_mode,
        rps,
        config.duration.as_secs_f64(),
        config.targets.len(),
    );

    tracing::info!(
        agents = workers.len(),
        targets = config.targets.len(),
        rps,
        duration = config.duration.as_secs_f64(),
        plugin = plugin.as_deref().unwrap_or("none"),
        version = crate::VERSION,
        "starting distributed test"
    );

    // --- Run through the same TestQueue/scheduler path as daemon mode ---

    let leader_metrics = LeaderMetricsState::new();

    // Temporary result store (one-shot mode doesn't persist to disk).
    let tmp_dir = tempfile::tempdir().context("failed to create temp dir for results")?;
    let store = ResultStore::open(tmp_dir.path(), 1).context("failed to open temp result store")?;

    let queue = TestQueue::new(store);
    let test_id = queue.enqueue_config(config, summary);

    let scheduler_queue = queue.clone();
    let queue_config = QueueConfig {
        workers,
        tls: tls_config,
        external_metrics_url,
        external_metrics_field,
        agents: Arc::new(Mutex::new(Vec::new())),
        leader_metrics: leader_metrics.clone(),
    };

    // Clone for use after the async block.
    let result_queue = queue.clone();
    let result_test_id = test_id.clone();

    // Single tokio runtime for scheduler + optional metrics + progress polling.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let scheduler = scheduler_queue.run_scheduler(queue_config);
        tokio::pin!(scheduler);

        // Optional Prometheus metrics server.
        let metrics_future = async {
            if let Some(port) = metrics_port {
                let server = LeaderServer::new(leader_metrics);
                let _ = server.serve(port).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(metrics_future);

        // Progress polling loop.
        let is_tty = std::io::stderr().is_terminal();
        let mut last_log_elapsed = Duration::ZERO;
        let progress_poll = async {
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;

                let info = queue.get_info(&test_id);
                let status = info.as_ref().map(|i| i.status);

                if let Some(progress) = queue.get_progress(&test_id) {
                    if is_tty {
                        eprint!(
                            "\r  [{:.1}s] {:.0} RPS target | {} requests | {} errors | {} nodes",
                            progress.elapsed.as_secs_f64(),
                            progress.target_rps,
                            progress.total_requests,
                            progress.total_errors,
                            progress.active_nodes,
                        );
                    } else if progress.elapsed.saturating_sub(last_log_elapsed)
                        >= Duration::from_secs(1)
                    {
                        last_log_elapsed = progress.elapsed;
                        tracing::info!(
                            elapsed_secs = format!("{:.1}", progress.elapsed.as_secs_f64()),
                            target_rps = format!("{:.0}", progress.target_rps),
                            total_requests = progress.total_requests,
                            total_errors = progress.total_errors,
                            active_nodes = progress.active_nodes,
                            "test progress",
                        );
                    }
                }

                match status {
                    Some(netanvil_distributed::TestStatus::Completed)
                    | Some(netanvil_distributed::TestStatus::Failed)
                    | Some(netanvil_distributed::TestStatus::Cancelled) => break,
                    _ => continue,
                }
            }
        };

        // Run all concurrently; progress_poll completes when test finishes.
        tokio::select! {
            _ = &mut scheduler => {},
            _ = &mut metrics_future => {},
            _ = progress_poll => {},
        }
    });

    if std::io::stderr().is_terminal() {
        eprintln!(); // newline after progress
    }

    // Fetch and print results.
    if let Some(entry) = result_queue.get_entry(&result_test_id) {
        if let Some(ref result) = entry.result {
            crate::output::print_stored_results(result, output_format);
        }
    }

    Ok(())
}
