use std::time::Duration;

use anyhow::{Context, Result};

use netanvil_distributed::{
    DistributedCoordinator, HttpMetricsFetcher, HttpNodeCommander, HttpSignalPoller,
    LeaderMetricsState, LeaderServer, MtlsMetricsFetcher, MtlsNodeCommander, MtlsStaticDiscovery,
    StaticDiscovery,
};
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
    response_signals: Vec<String>,
    payload: Option<String>,
    payload_hex: Option<String>,
    payload_file: Option<String>,
    framing: String,
    no_response: bool,
    mode: String,
    request_size: Option<u16>,
    response_size: Option<u32>,
    dns_domains: Option<String>,
    dns_query_type: String,
    ramp_warmup: String,
    ramp_multiplier: f64,
    ramp_max_errors: f64,
    metrics_port: Option<u16>,
    output: String,
) -> Result<()> {
    let output_format = crate::output::OutputFormat::parse(&output).map_err(|e| anyhow::anyhow!(e))?;
    let duration = parse_duration(&duration).context("invalid --duration")?;
    let timeout = parse_duration(&timeout).context("invalid --timeout")?;
    let autotune_dur =
        parse_duration(&pid_autotune_duration).context("invalid --pid-autotune-duration")?;
    let ramp_warmup_dur = parse_duration(&ramp_warmup).context("invalid --ramp-warmup")?;
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
        metrics_interval: Duration::from_millis(500),
        control_interval: Duration::from_millis(200),
        error_status_threshold: error_threshold,
        external_metrics_url: external_metrics_url.clone(),
        external_metrics_field: external_metrics_field.clone(),
        bandwidth_limit_bps: None,
        plugin: plugin_config,
        response_signal_headers: parse_response_signals(&response_signals)?,
        ..Default::default()
    };

    // Detect protocol from URLs and populate config.protocol for TCP/UDP
    let protocol = detect_protocol(&config.targets);
    match protocol {
        DetectedProtocol::Tcp => {
            let tcp_payload = resolve_payload(&payload, &payload_hex, &payload_file)
                .context("invalid TCP payload")?;
            let _tcp_mode = parse_tcp_mode(&mode).context("invalid --mode")?;
            let req_size = request_size.unwrap_or(tcp_payload.len() as u16);
            let resp_size = response_size.unwrap_or(req_size as u32);
            config.protocol = Some(netanvil_types::ProtocolConfig::Tcp {
                mode: mode.clone(),
                payload_hex: encode_hex(&tcp_payload),
                framing: framing.clone(),
                request_size: req_size,
                response_size: resp_size,
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
        DetectedProtocol::Http => {
            // No change needed -- config.protocol stays None (default HTTP)
        }
    }

    tracing::info!(
        agents = workers.len(),
        targets = config.targets.len(),
        rps,
        ?duration,
        plugin = plugin.as_deref().unwrap_or("none"),
        "starting distributed test"
    );

    // Build rate controller
    let rate_controller = netanvil_core::build_rate_controller(
        &config.rate,
        config.control_interval,
        std::time::Instant::now(),
    );

    let tls_config = match (&tls_ca, &tls_cert, &tls_key) {
        (Some(ca), Some(cert), Some(key)) => Some(netanvil_types::TlsConfig {
            ca_cert: ca.clone(),
            cert: cert.clone(),
            key: key.clone(),
        }),
        _ => None,
    };

    // Start leader metrics server if requested
    let metrics_state = metrics_port.map(|port| {
        let state = LeaderMetricsState::new();
        let server = LeaderServer::new(state.clone());
        server
            .spawn(port)
            .unwrap_or_else(|e| tracing::error!(port, "failed to start leader metrics server: {e}"));
        state
    });

    // Helper to configure and run a coordinator (generic over trait impls).
    fn run_coordinator<D, M, C>(
        mut coordinator: DistributedCoordinator<D, M, C>,
        external_metrics_url: Option<&str>,
        external_metrics_field: Option<&str>,
        metrics_state: Option<LeaderMetricsState>,
    ) -> netanvil_distributed::DistributedTestResult
    where
        D: netanvil_types::NodeDiscovery,
        M: netanvil_types::MetricsFetcher,
        C: netanvil_types::NodeCommander,
    {
        if let Some(poller) =
            HttpSignalPoller::from_config(external_metrics_url, external_metrics_field)
        {
            coordinator.set_signal_source(poller.into_source());
        }
        coordinator.on_progress(move |update| {
            if let Some(ref state) = metrics_state {
                state.update(update);
            }
            eprint!(
                "\r  [{:.1}s] {:.0} RPS target | {} requests | {} errors | {} nodes",
                update.elapsed.as_secs_f64(),
                update.target_rps,
                update.total_requests,
                update.total_errors,
                update.active_nodes,
            );
        });
        coordinator.run()
    }

    let result = if let Some(ref tls) = tls_config {
        tracing::info!("using mTLS for agent communication");
        let discovery = MtlsStaticDiscovery::new(workers, tls)
            .map_err(|e| anyhow::anyhow!("mTLS discovery: {e}"))?;
        let fetcher =
            MtlsMetricsFetcher::new(tls).map_err(|e| anyhow::anyhow!("mTLS fetcher: {e}"))?;
        let commander =
            MtlsNodeCommander::new(tls).map_err(|e| anyhow::anyhow!("mTLS commander: {e}"))?;
        let coordinator =
            DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);
        run_coordinator(
            coordinator,
            external_metrics_url.as_deref(),
            external_metrics_field.as_deref(),
            metrics_state.clone(),
        )
    } else {
        let discovery = StaticDiscovery::new(workers);
        let fetcher = HttpMetricsFetcher::new(Duration::from_secs(5));
        let commander = HttpNodeCommander::new(Duration::from_secs(10));
        let coordinator =
            DistributedCoordinator::new(discovery, fetcher, commander, config, rate_controller);
        run_coordinator(
            coordinator,
            external_metrics_url.as_deref(),
            external_metrics_field.as_deref(),
            metrics_state,
        )
    };
    eprintln!(); // newline after progress

    crate::output::print_distributed_results(&result, output_format);

    Ok(())
}
